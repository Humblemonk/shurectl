//! Real-time input level metering via cpal.
//!
//! Opens the connected device's own input, or the system default input when
//! that can't be told apart (`start_meter()`), and measures sample-peak dBFS
//! on a background thread. Results reach the render thread through
//! `peak_window: Arc<Mutex<PeakWindow>>`, a pair of rolling time windows
//! (short: 0.3 s, long: 3.0 s). The audio thread pushes every callback's peak
//! into both windows; the render thread reads the rolling maxima to drive the
//! bar and the peak-hold readout. Readings expire with their window, so a
//! stalled or failed stream drops to "no data" instead of freezing.
//!
//! On Linux the device's input is found through PipeWire or PulseAudio, which
//! share it with every other app. Never through ALSA's own devices: those
//! open the hardware itself (`hw:`, `plughw:`), which can keep PipeWire, and
//! every app recording through it, off the mic until shurectl exits. On plain
//! ALSA the meter keeps to the default input.
//!
//! The sentinel value `METER_SILENT` (`i32::MIN`) means no audio data has
//! arrived recently, or the stream could not be opened.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SizedSample, Stream, StreamConfig};

/// Sentinel meaning no data is available.
pub const METER_SILENT: i32 = i32::MIN;

/// Minimum dBFS we display (-60 dB floor).
pub const METER_FLOOR_DB: f32 = -60.0;

// ── Rolling peak window ───────────────────────────────────────────────────────

/// A rolling time-window that tracks the maximum dBFS value seen within the
/// last `keep_secs` seconds.
///
/// Values are stored as `peak_dbfs * 10` and timestamped with
/// `std::time::Instant`. Old entries are evicted on every `push` and ignored
/// by `max`, so the window empties once pushes stop.
pub struct RollingWindow {
    /// How many seconds of history to keep.
    keep_secs: f32,
    /// Timestamped samples, oldest first.
    samples: VecDeque<(Instant, i32)>,
}

impl RollingWindow {
    pub fn new(keep_secs: f32) -> Self {
        Self {
            keep_secs,
            samples: VecDeque::new(),
        }
    }

    /// Push a new sample and evict all entries older than `keep_secs`.
    pub fn push(&mut self, now: Instant, value: i32) {
        self.samples.push_back((now, value));

        let threshold_secs = self.keep_secs;
        while let Some(&(ts, _)) = self.samples.front() {
            if now.duration_since(ts).as_secs_f32() > threshold_secs {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// The maximum value pushed within `keep_secs` of `now`, or `None` if none.
    pub fn max(&self, now: Instant) -> Option<i32> {
        self.samples
            .iter()
            .filter(|&&(ts, _)| now.saturating_duration_since(ts).as_secs_f32() <= self.keep_secs)
            .map(|&(_, v)| v)
            .max()
    }
}

/// A pair of rolling windows shared between the audio thread and render thread.
///
/// * `short` — 0.3 s: drives the instantaneous bar height displayed in the gauge.
/// * `long`  — 3.0 s: drives the peak-hold readout (`pk -8.1`) beside the bar.
pub struct PeakWindow {
    pub short: RollingWindow,
    pub long: RollingWindow,
}

impl PeakWindow {
    pub fn new() -> Self {
        Self {
            short: RollingWindow::new(0.3),
            long: RollingWindow::new(3.0),
        }
    }

    /// Push a new value into both windows simultaneously.
    pub fn push(&mut self, now: Instant, value: i32) {
        self.short.push(now, value);
        self.long.push(now, value);
    }
}

impl Default for PeakWindow {
    fn default() -> Self {
        Self::new()
    }
}

// ── MeterStatus ───────────────────────────────────────────────────────────────

/// What `start_meter()` returns: the running stream, or why it couldn't start.
pub enum MeterStatus {
    /// Stream is running on this input; reads come via the shared `PeakWindow`.
    Running(Stream, MeterInput),
    /// cpal could not open a capture stream; message is shown in the UI.
    Failed(String),
}

/// Which input the meter listens to.
#[derive(Debug, Clone, PartialEq)]
pub enum MeterInput {
    /// The connected device's own input, by the name the system gives it.
    Device(String),
    /// The system default input, because the device's own wasn't found.
    Default,
}

/// What identifies the connected device among the system's inputs.
#[derive(Debug, Default)]
pub struct InputTarget<'a> {
    /// The USB serial number. PipeWire and PulseAudio put it in the input's
    /// ID (seen 2026-10-31 on the MVX2U Gen 2 under PipeWire).
    pub usb_serial: Option<&'a str>,
    /// The USB product string, for systems whose input names hold it
    /// (macOS and Windows; unconfirmed).
    pub product: Option<&'a str>,
}

/// Squashed serials shorter than this could match by chance.
const MIN_SERIAL_LEN: usize = 6;

/// Callback period asked of PipeWire and PulseAudio. Left to choose, they
/// called back about every 300 ms (MVX2U Gen 2 under PipeWire, 2026-10-31),
/// as long as the meter's whole 0.3 s bar window.
const PULSE_CALLBACK_HZ: u32 = 50;

// ── start_meter ───────────────────────────────────────────────────────────────

/// Start the capture stream on `target`'s own input, or on the system
/// default input when no single input matches it.
///
/// Every audio callback pushes its peak, as `peak_dbfs * 10`, into both
/// rolling windows in `peak_window`.
///
/// Returns `MeterStatus::Running(stream, input)` on success. The caller
/// **must** keep the returned `Stream` alive for as long as metering is
/// desired — dropping it stops the audio capture.
pub fn start_meter(peak_window: Arc<Mutex<PeakWindow>>, target: &InputTarget) -> MeterStatus {
    // cpal probes JACK, OSS, dmix, and dsnoop during host/device enumeration.
    // These C libraries print directly to stderr when their backends are
    // unavailable — there is no way to intercept them from Rust. We suppress
    // stderr for the duration of the noisy probing phase, then restore it.
    let stderr_suppressed = StderrSuppressor::new();

    // PipeWire or PulseAudio when one is running on Linux, else ALSA.
    let host = cpal::default_host();

    let (device, input) = match device_input(&host, target) {
        Some((device, name)) => (device, MeterInput::Device(name)),
        None => match host.default_input_device() {
            Some(device) => (device, MeterInput::Default),
            None => {
                drop(stderr_suppressed);
                return MeterStatus::Failed("No input device found for metering".to_string());
            }
        },
    };

    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            drop(stderr_suppressed);
            return MeterStatus::Failed(format!("Cannot get input config: {e}"));
        }
    };

    // Probing is done; restore stderr before building the stream.
    drop(stderr_suppressed);

    let mut stream_config: StreamConfig = config.into();
    if is_pulse(&host) {
        stream_config.buffer_size =
            cpal::BufferSize::Fixed(stream_config.sample_rate / PULSE_CALLBACK_HZ);
    }

    // Stream errors need no handling: once callbacks stop, the windows expire
    // and the UI shows no reading.
    let err_fn = |_e: cpal::Error| {};

    use cpal::SampleFormat;
    let stream = match config.sample_format() {
        SampleFormat::F32 => build_stream::<f32>(&device, &stream_config, peak_window, err_fn),
        SampleFormat::I16 => build_stream::<i16>(&device, &stream_config, peak_window, err_fn),
        SampleFormat::U16 => build_stream::<u16>(&device, &stream_config, peak_window, err_fn),
        SampleFormat::I8 => build_stream::<i8>(&device, &stream_config, peak_window, err_fn),
        SampleFormat::U8 => build_stream::<u8>(&device, &stream_config, peak_window, err_fn),
        SampleFormat::I24 => {
            build_stream::<cpal::I24>(&device, &stream_config, peak_window, err_fn)
        }
        SampleFormat::I32 => build_stream::<i32>(&device, &stream_config, peak_window, err_fn),
        SampleFormat::U32 => build_stream::<u32>(&device, &stream_config, peak_window, err_fn),
        SampleFormat::F64 => build_stream::<f64>(&device, &stream_config, peak_window, err_fn),
        // Rare formats (64-bit ints, U24, DSD): ask for f32. That works where the
        // host converts (ALSA's default plug, PipeWire); elsewhere it fails and
        // the UI shows "Meter unavailable".
        _ => build_stream::<f32>(&device, &stream_config, peak_window, err_fn),
    };

    match stream {
        Ok(s) => {
            if let Err(e) = s.play() {
                MeterStatus::Failed(format!("Cannot start meter stream: {e}"))
            } else {
                MeterStatus::Running(s, input)
            }
        }
        Err(e) => MeterStatus::Failed(format!("Cannot build meter stream: {e}")),
    }
}

/// `target`'s own input on `host`, with its name, when exactly one matches.
/// Never on plain ALSA (see the module doc).
fn device_input(host: &cpal::Host, target: &InputTarget) -> Option<(Device, String)> {
    if is_alsa(host) {
        return None;
    }
    let inputs: Vec<(Device, String, String)> = host
        .input_devices()
        .ok()?
        .map(|device| {
            let id = device
                .id()
                .map(|id| id.id().to_string())
                .unwrap_or_default();
            let name = device
                .description()
                .map(|description| description.name().to_string())
                .unwrap_or_default();
            (device, id, name)
        })
        .collect();
    let listed: Vec<(&str, &str)> = inputs
        .iter()
        .map(|(_, id, name)| (id.as_str(), name.as_str()))
        .collect();
    let index = matching_input(&listed, target)?;
    let (device, id, name) = inputs.into_iter().nth(index)?;
    Some((device, if name.is_empty() { id } else { name }))
}

/// The index of the one input in `inputs` (ID, name) that is `target`: the
/// one whose ID or name holds its USB serial, or failing that the one whose
/// name holds its product string. `None` when none or several match.
/// PulseAudio's monitor sources (what an output plays) are never the mic, and
/// carry the same serial.
fn matching_input(inputs: &[(&str, &str)], target: &InputTarget) -> Option<usize> {
    let candidates: Vec<(usize, String, String)> = inputs
        .iter()
        .enumerate()
        .filter(|(_, (id, _))| !id.ends_with(".monitor"))
        .map(|(index, (id, name))| (index, squash(id), squash(name)))
        .collect();
    let only = |found: &str, in_id: bool| {
        let mut matches = candidates
            .iter()
            .filter(|(_, id, name)| name.contains(found) || (in_id && id.contains(found)));
        match (matches.next(), matches.next()) {
            (Some((index, _, _)), None) => Some(*index),
            _ => None,
        }
    };
    let serial = target
        .usb_serial
        .map(squash)
        .filter(|serial| serial.len() >= MIN_SERIAL_LEN);
    let product = target
        .product
        .map(squash)
        .filter(|product| !product.is_empty());
    serial
        .and_then(|serial| only(&serial, true))
        .or_else(|| product.and_then(|product| only(&product, false)))
}

/// Lowercase ASCII letters and digits only: PipeWire writes a serial's
/// spaces and `#` as `_` in an input's ID.
fn squash(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(target_os = "linux")]
fn is_alsa(host: &cpal::Host) -> bool {
    host.id() == cpal::HostId::Alsa
}

#[cfg(not(target_os = "linux"))]
fn is_alsa(_host: &cpal::Host) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn is_pulse(host: &cpal::Host) -> bool {
    host.id() == cpal::HostId::PulseAudio
}

#[cfg(not(target_os = "linux"))]
fn is_pulse(_host: &cpal::Host) -> bool {
    false
}

// ── Stderr suppressor ─────────────────────────────────────────────────────────

/// Temporarily redirects file descriptor 2 (stderr) to `/dev/null` for the
/// lifetime of this value. Restores the original fd on drop.
///
/// This is necessary because cpal's ALSA and JACK backends print directly to
/// stderr via their C libraries during device enumeration, with no Rust-level
/// hook available to suppress them. The suppression window is kept as short as
/// possible — only the probing calls, not stream construction or playback.
///
/// On Windows, cpal's WASAPI backend does not write to stderr during probing,
/// so the non-unix impl is a no-op that keeps `start_meter()` unconditional.
///
/// Safety (unix): `dup` / `dup2` / `open` are async-signal-safe and do not
/// interact with Rust's I/O machinery. We never write to stderr ourselves
/// inside the suppression window, so there is no risk of losing our own output.
struct StderrSuppressor {
    #[cfg(unix)]
    saved_fd: i32,
}

#[cfg(unix)]
impl StderrSuppressor {
    fn new() -> Self {
        // SAFETY: dup(2) duplicates the stderr fd; we check for failure.
        let saved_fd = unsafe { libc::dup(2) };
        if saved_fd >= 0 {
            // SAFETY: open + dup2 are well-defined POSIX operations.
            let devnull = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY) };
            if devnull >= 0 {
                unsafe { libc::dup2(devnull, 2) };
                unsafe { libc::close(devnull) };
            }
        }
        Self { saved_fd }
    }
}

#[cfg(unix)]
impl Drop for StderrSuppressor {
    fn drop(&mut self) {
        if self.saved_fd >= 0 {
            // SAFETY: restore the original stderr fd we saved in new().
            unsafe {
                libc::dup2(self.saved_fd, 2);
                libc::close(self.saved_fd);
            }
        }
    }
}

#[cfg(not(unix))]
impl StderrSuppressor {
    fn new() -> Self {
        Self {}
    }
}

#[cfg(not(unix))]
impl Drop for StderrSuppressor {
    fn drop(&mut self) {}
}

// ── Stream builder ────────────────────────────────────────────────────────────

/// Sample-peak level of one callback buffer (all channels) as `dbfs * 10`,
/// rounded and floored at `METER_FLOOR_DB`.
fn peak_dbfs_x10<S>(data: &[S]) -> i32
where
    S: SizedSample,
    f32: FromSample<S>,
{
    let peak: f32 = data
        .iter()
        .map(|&s| <f32 as FromSample<S>>::from_sample_(s).abs())
        .fold(0.0_f32, f32::max);
    let dbfs = if peak > 0.0 {
        (20.0 * peak.log10()).max(METER_FLOOR_DB)
    } else {
        METER_FLOOR_DB
    };
    (dbfs * 10.0).round() as i32
}

/// Build a typed input stream for sample type `S` that pushes each callback's
/// `peak_dbfs_x10()` into both rolling windows.
fn build_stream<S>(
    device: &Device,
    config: &StreamConfig,
    peak_window: Arc<Mutex<PeakWindow>>,
    err_fn: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<Stream, cpal::Error>
where
    S: SizedSample + Send + 'static,
    f32: FromSample<S>,
{
    // Peak of callbacks that couldn't take the lock, carried into the next
    // push so a brief transient (or clip) is never dropped.
    let mut pending = METER_SILENT;
    device.build_input_stream(
        *config,
        move |data: &[S], _info: &cpal::InputCallbackInfo| {
            pending = pending.max(peak_dbfs_x10(data));
            // try_lock avoids blocking the audio thread while the render
            // thread reads the windows.
            if let Ok(mut pw) = peak_window.try_lock() {
                pw.push(Instant::now(), pending);
                pending = METER_SILENT;
            }
        },
        err_fn,
        None, // no timeout
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // As cpal's PulseAudio host listed them under PipeWire (2026-10-31), with
    // the serial made up.
    const GEN2_SERIAL: &str = "MVX2U GEN 2#2-0123456789abcdef0123456789abcdef";
    const PULSE_INPUTS: [(&str, &str); 4] = [
        (
            "alsa_output.usb-Shure_Inc_Shure_MVX2U_GEN_2_MVX2U_GEN_2_2-0123456789abcdef0123456789abcdef-01.analog-stereo.monitor",
            "Monitor of Shure MVX2U GEN 2 Analog Stereo",
        ),
        (
            "alsa_input.usb-Shure_Inc_Shure_MVX2U_GEN_2_MVX2U_GEN_2_2-0123456789abcdef0123456789abcdef-01.mono-fallback",
            "Shure MVX2U GEN 2 Mono",
        ),
        (
            "alsa_output.usb-SMSL_SMSL_USB_AUDIO-00.playback.0.0.monitor",
            "Monitor of SMSL RAW-MDA1",
        ),
        (
            "alsa_output.usb-FIIO_FiiO_K11_R2R-01.playback.0.0.monitor",
            "Monitor of FiiO K11 R2R",
        ),
    ];

    fn target<'a>(usb_serial: Option<&'a str>, product: Option<&'a str>) -> InputTarget<'a> {
        InputTarget {
            usb_serial,
            product,
        }
    }

    #[test]
    fn matching_input_finds_the_source_by_serial_and_skips_its_monitor() {
        let found = matching_input(&PULSE_INPUTS, &target(Some(GEN2_SERIAL), None));
        assert_eq!(found, Some(1));
    }

    #[test]
    fn matching_input_tells_two_of_a_model_apart_by_serial() {
        let inputs = [
            (
                "alsa_input.usb-Shure_Inc_Shure_MVX2U_GEN_2_MVX2U_GEN_2_2-aaaa1111-01.mono-fallback",
                "Shure MVX2U GEN 2 Mono",
            ),
            (
                "alsa_input.usb-Shure_Inc_Shure_MVX2U_GEN_2_MVX2U_GEN_2_2-bbbb2222-01.mono-fallback",
                "Shure MVX2U GEN 2 Mono",
            ),
        ];
        let product = Some("Shure MVX2U GEN 2");
        let second = target(Some("MVX2U GEN 2#2-bbbb2222"), product);
        assert_eq!(matching_input(&inputs, &second), Some(1));
        // Without a serial the product names both, so neither is picked.
        assert_eq!(matching_input(&inputs, &target(None, product)), None);
    }

    #[test]
    fn matching_input_falls_back_to_the_product_name() {
        let inputs = [
            ("{0.0.1.00000000}.{1111}", "Microphone (Realtek Audio)"),
            ("{0.0.1.00000000}.{2222}", "Microphone (Shure MV7+)"),
        ];
        let mv7_plus = target(Some("no-such-serial-anywhere"), Some("Shure MV7+"));
        assert_eq!(matching_input(&inputs, &mv7_plus), Some(1));
    }

    #[test]
    fn matching_input_ignores_a_short_or_missing_serial_and_an_empty_product() {
        assert_eq!(
            matching_input(&PULSE_INPUTS, &target(Some("2"), None)),
            None
        );
        assert_eq!(matching_input(&PULSE_INPUTS, &target(None, Some(""))), None);
        assert_eq!(matching_input(&PULSE_INPUTS, &InputTarget::default()), None);
    }

    // ── RollingWindow ─────────────────────────────────────────────────────────

    #[test]
    fn rolling_window_returns_max_of_recent_samples() {
        let mut w = RollingWindow::new(1.0);
        let t = Instant::now();
        w.push(t, -300);
        w.push(t, -100);
        w.push(t, -200);
        assert_eq!(w.max(Instant::now()), Some(-100));
    }

    #[test]
    fn rolling_window_boundary_sample_is_kept() {
        // A sample exactly at the boundary (0.9 s ago in a 1.0 s window) must
        // not be evicted.
        let mut w = RollingWindow::new(1.0);
        let recent = Instant::now() - Duration::from_millis(900);
        let now = Instant::now();

        w.push(recent, -200);
        w.push(now, -400);

        assert_eq!(
            w.max(Instant::now()),
            Some(-200),
            "sample within the window boundary must be retained"
        );
    }

    // ── PeakWindow ────────────────────────────────────────────────────────────

    #[test]
    fn peak_window_short_evicts_before_long() {
        let mut pw = PeakWindow::new();

        // 500 ms ago: within long (3 s) but outside short (0.3 s).
        let old = Instant::now() - Duration::from_millis(500);
        let now = Instant::now();

        pw.push(old, -100);
        // Push a low value now to trigger eviction in the short window.
        pw.push(now, -500);

        assert_eq!(
            pw.short.max(Instant::now()),
            Some(-500),
            "short window must evict the 500ms-old sample"
        );
        assert_eq!(
            pw.long.max(Instant::now()),
            Some(-100),
            "long window must still hold the 500ms-old sample"
        );
    }

    #[test]
    fn rolling_window_expires_without_new_pushes() {
        // A stalled stream must not leave the last reading frozen on screen.
        let mut w = RollingWindow::new(0.3);
        let t = Instant::now();
        w.push(t, -100);
        assert_eq!(w.max(t + Duration::from_millis(200)), Some(-100));
        assert!(w.max(t + Duration::from_millis(400)).is_none());
    }

    // ── peak_dbfs_x10 ─────────────────────────────────────────────────────────

    #[test]
    fn peak_dbfs_x10_matches_reference_levels() {
        // Full scale in either polarity is 0 dBFS.
        assert_eq!(peak_dbfs_x10(&[0.0_f32, -1.0, 0.5]), 0);
        assert_eq!(peak_dbfs_x10(&[i16::MIN, 0]), 0);
        // Half scale is -6.0 dBFS; a tenth is -20.0 dBFS.
        assert_eq!(peak_dbfs_x10(&[0.5_f32]), -60);
        assert_eq!(peak_dbfs_x10(&[0.1_f32]), -200);
        assert_eq!(peak_dbfs_x10(&[16384_i16]), -60);
        // u16 midpoint is silence; anything below the floor clamps to it.
        assert_eq!(peak_dbfs_x10(&[32768_u16]), -600);
        assert_eq!(peak_dbfs_x10(&[0.0001_f32]), -600);
        assert_eq!(peak_dbfs_x10::<f32>(&[]), -600);
        // Rounds rather than truncates: -2.96 dB is -3.0, not -2.9.
        assert_eq!(peak_dbfs_x10(&[10f32.powf(-2.96 / 20.0)]), -30);
    }
}
