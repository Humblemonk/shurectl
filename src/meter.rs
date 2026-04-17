//! Real-time input level metering via cpal.
//!
//! Opens the system default input device and measures peak dBFS on a
//! background thread. Two values are published to the render thread:
//!
//! * `meter_level: Arc<AtomicI32>` — instantaneous peak from the latest
//!   audio callback, stored as `peak_dbfs * 10`. Lock-free; safe to read
//!   from the render loop every 100 ms without blocking the audio thread.
//!
//! * `peak_window: Arc<Mutex<PeakWindow>>` — a pair of rolling time windows
//!   (short: 0.3 s, long: 3.0 s). The audio thread pushes every callback's
//!   peak into both windows; the render thread reads the rolling maxima to
//!   drive the bar ratio and the peak-hold marker respectively.
//!
//! We deliberately do NOT search for the device by name. On Linux, cpal's
//! default ALSA host would find the raw `hw:MVX2U` PCM device and open it
//! exclusively, preventing any other application (PipeWire, PulseAudio, DAW)
//! from capturing audio until shurectl exits. Using the default input
//! device instead lets PipeWire/PulseAudio act as the broker and share the
//! hardware normally. If the user has set the MVX2U as their default input,
//! the meter reads it correctly without the exclusivity problem.
//!
//! The sentinel value `METER_SILENT` (`i32::MIN`) means no audio data has
//! arrived yet, or the stream could not be opened.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SizedSample, Stream, StreamConfig};

/// Sentinel stored in the atomic when no data is available.
pub const METER_SILENT: i32 = i32::MIN;

/// Minimum dBFS we display (-60 dB floor).
pub const METER_FLOOR_DB: f32 = -60.0;

// ── Rolling peak window ───────────────────────────────────────────────────────

/// A rolling time-window that tracks the maximum dBFS value seen within the
/// last `keep_secs` seconds.
///
/// Values are stored as `peak_dbfs * 10` (same integer encoding as
/// `meter_level`) and timestamped with `std::time::Instant`. Old entries are
/// evicted lazily on every `push`.
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

    /// The maximum value seen in the current window, or `None` if empty.
    pub fn max(&self) -> Option<i32> {
        self.samples.iter().map(|&(_, v)| v).max()
    }
}

/// A pair of rolling windows shared between the audio thread and render thread.
///
/// * `short` — 0.3 s: drives the instantaneous bar height displayed in the gauge.
/// * `long`  — 3.0 s: drives the slow peak-hold marker shown beside the bar.
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

/// How the meter thread communicates failure back to the UI.
pub enum MeterStatus {
    /// Stream is running; reads come via the atomic and the shared window.
    Running(Stream),
    /// cpal could not open a capture stream; message is shown in the UI.
    Failed(String),
}

// ── start_meter ───────────────────────────────────────────────────────────────

/// Start the capture stream.
///
/// Opens the system default input device via cpal. On every audio callback:
/// - writes the instantaneous peak as `peak_dbfs * 10` into `level`
/// - pushes the same value into both rolling windows in `peak_window`
///
/// Returns `MeterStatus::Running(stream)` on success. The caller **must**
/// keep the returned `Stream` alive for as long as metering is desired —
/// dropping it stops the audio capture.
pub fn start_meter(level: Arc<AtomicI32>, peak_window: Arc<Mutex<PeakWindow>>) -> MeterStatus {
    // cpal probes JACK, OSS, dmix, and dsnoop during host/device enumeration.
    // These C libraries print directly to stderr when their backends are
    // unavailable — there is no way to intercept them from Rust. We suppress
    // stderr for the duration of the noisy probing phase, then restore it.
    let stderr_suppressed = StderrSuppressor::new();

    let host = cpal::default_host();

    // Use the system default input device. PipeWire/PulseAudio will route
    // from the MVX2U if it is set as the default, without exclusive access.
    let device: Device = match host.default_input_device() {
        Some(d) => d,
        None => {
            drop(stderr_suppressed);
            return MeterStatus::Failed("No input device found for metering".to_string());
        }
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

    let stream_config: StreamConfig = config.clone().into();

    // Error callback: write METER_SILENT so the UI shows no reading.
    let level_err = Arc::clone(&level);
    let err_fn = move |_e: cpal::StreamError| {
        level_err.store(METER_SILENT, Ordering::Relaxed);
    };

    use cpal::SampleFormat;
    let stream = match config.sample_format() {
        SampleFormat::F32 => {
            build_stream::<f32>(&device, &stream_config, level, peak_window, err_fn)
        }
        SampleFormat::I16 => {
            build_stream::<i16>(&device, &stream_config, level, peak_window, err_fn)
        }
        SampleFormat::U16 => {
            build_stream::<u16>(&device, &stream_config, level, peak_window, err_fn)
        }
        // cpal 0.15 added more formats; handle them all the same way.
        _ => build_stream::<f32>(&device, &stream_config, level, peak_window, err_fn),
    };

    match stream {
        Ok(s) => {
            if let Err(e) = s.play() {
                MeterStatus::Failed(format!("Cannot start meter stream: {e}"))
            } else {
                MeterStatus::Running(s)
            }
        }
        Err(e) => MeterStatus::Failed(format!("Cannot build meter stream: {e}")),
    }
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
/// Safety: `dup` / `dup2` / `open` are async-signal-safe and do not interact
/// with Rust's I/O machinery. We never write to stderr ourselves inside the
/// suppression window, so there is no risk of losing our own error output.
struct StderrSuppressor {
    saved_fd: i32,
}

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

// ── Stream builder ────────────────────────────────────────────────────────────

/// Build a typed input stream for sample type `S`.
///
/// On each callback:
/// 1. Compute the peak absolute sample value across all channels.
/// 2. Convert to dBFS, clamped to `METER_FLOOR_DB`.
/// 3. Store as `(dbfs * 10.0) as i32` in the atomic (instantaneous).
/// 4. Push the same value into both rolling windows under the shared Mutex.
fn build_stream<S>(
    device: &Device,
    config: &StreamConfig,
    level: Arc<AtomicI32>,
    peak_window: Arc<Mutex<PeakWindow>>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<Stream, cpal::BuildStreamError>
where
    S: SizedSample + Send + 'static,
    f32: FromSample<S>,
{
    device.build_input_stream(
        config,
        move |data: &[S], _info: &cpal::InputCallbackInfo| {
            // Find the peak absolute sample value in this callback buffer.
            let peak: f32 = data
                .iter()
                .map(|&s| <f32 as FromSample<S>>::from_sample_(s).abs())
                .fold(0.0_f32, f32::max);

            // Convert to dBFS; clamp to our display floor.
            let dbfs = if peak > 0.0 {
                (20.0 * peak.log10()).max(METER_FLOOR_DB)
            } else {
                METER_FLOOR_DB
            };

            let encoded = (dbfs * 10.0) as i32;

            // Publish instantaneous level lock-free.
            level.store(encoded, Ordering::Relaxed);

            // Push into rolling windows. try_lock avoids blocking the audio
            // thread if the render thread is mid-read (extremely rare).
            let now = Instant::now();
            if let Ok(mut pw) = peak_window.try_lock() {
                pw.push(now, encoded);
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

    // ── RollingWindow ─────────────────────────────────────────────────────────

    #[test]
    fn rolling_window_empty_returns_none() {
        let w = RollingWindow::new(1.0);
        assert!(w.max().is_none());
    }

    #[test]
    fn rolling_window_returns_max_of_recent_samples() {
        let mut w = RollingWindow::new(1.0);
        let t = Instant::now();
        w.push(t, -300);
        w.push(t, -100);
        w.push(t, -200);
        assert_eq!(w.max(), Some(-100));
    }

    #[test]
    fn rolling_window_evicts_expired_samples() {
        let mut w = RollingWindow::new(1.0);
        let old = Instant::now() - Duration::from_secs(2);
        let now = Instant::now();

        // Push a high value with a timestamp 2 seconds ago (outside the 1s window).
        w.push(old, -100);
        // Push a low value now — this triggers eviction of the old entry.
        w.push(now, -400);

        assert_eq!(
            w.max(),
            Some(-400),
            "expired sample must be evicted, leaving only the recent low value"
        );
    }

    #[test]
    fn rolling_window_keeps_samples_within_window() {
        let mut w = RollingWindow::new(2.0);
        let t = Instant::now();
        w.push(t, -200);
        w.push(t, -150);
        // Both are within the 2-second window; max should be -150.
        assert_eq!(w.max(), Some(-150));
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
            w.max(),
            Some(-200),
            "sample within the window boundary must be retained"
        );
    }

    // ── PeakWindow ────────────────────────────────────────────────────────────

    #[test]
    fn peak_window_push_updates_both_windows() {
        let mut pw = PeakWindow::new();
        let t = Instant::now();
        pw.push(t, -200);
        assert_eq!(pw.short.max(), Some(-200));
        assert_eq!(pw.long.max(), Some(-200));
    }

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
            pw.short.max(),
            Some(-500),
            "short window must evict the 500ms-old sample"
        );
        assert_eq!(
            pw.long.max(),
            Some(-100),
            "long window must still hold the 500ms-old sample"
        );
    }

    #[test]
    fn peak_window_both_empty_initially() {
        let pw = PeakWindow::new();
        assert!(pw.short.max().is_none());
        assert!(pw.long.max().is_none());
    }
}
