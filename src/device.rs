//! Device I/O: wraps hidapi for Shure USB microphones.
//!
//! Supports six devices:
//!   - Shure MVX2U       (VID 0x14ED, PID 0x1013) — XLR-to-USB interface (Gen 1)
//!   - Shure MVX2U Gen 2 (VID 0x14ED, PID 0x1033) — XLR-to-USB interface (Gen 2)
//!   - Shure MV6         (VID 0x14ED, PID 0x1026) — USB gaming microphone
//!   - Shure MV6 Gen 2   (VID 0x14ED, PID 0x1035) — USB gaming microphone (Gen 2)
//!   - Shure MV7         (VID 0x14ED, PID 0x1012) — USB/XLR dynamic microphone (original)
//!   - Shure MV7+        (VID 0x14ED, PID 0x1019) — USB/XLR dynamic microphone
//!
//! All six devices expose a USB HID configuration interface alongside their
//! audio interface. hidapi opens it via /dev/hidrawN on Linux, IOKit on macOS,
//! and \\.\HID#VID_... paths on Windows, bypassing the audio driver entirely.
//!
//! # Reports
//!
//! Configuration uses plain HID Output/Input reports:
//!   - `hid_write()` sends a command to the device (Output report).
//!   - `hid_read()` receives a response (Input report from the Interrupt IN endpoint).
//!
//! Every SET command must be followed immediately by a CONFIRM packet; the device
//! will not apply the change otherwise. GET commands receive one response packet
//! on the next read.
//!
//! The original MV7 is the exception: it takes one ASCII command line per report
//! and answers with a line of text (see `protocol::mv7_text`). Its commands go
//! through `send_text()`; the binary `send_set()`/`send_get_past_strays()` refuse to run on it.
//!
//! # Sequence numbers
//!
//! Each binary packet carries an incrementing sequence number (0–255, wrapping).
//! `ShureDevice::next_seq()` hands out one per packet built, CONFIRMs included.
//!
//! # Transport
//!
//! `ShureDevice` talks to a [`HidTransport`]: [`HidapiTransport`] for real
//! hardware, and `fake_hid::FakeHid` in tests and the `--qa` harness. Everything
//! above the transport (sequence numbers, ack draining, readback) runs the same
//! for both.
//!
//! # Multi-device
//!
//! If only one Shure device is plugged in, `open()` opens it automatically.
//! If multiple Shure devices are detected, `open()` returns an error directing
//! the user to specify one with `--device`. Use `list_devices()` to enumerate.
//!
//! # One shurectl at a time
//!
//! On Linux every open handle gets every input report, so two shurectl
//! processes reading at once take each other's replies as their own: each GET
//! is answered twice, the spare reply answers the next GET, and the settings
//! read last keep their defaults (two `--json` runs at once, MVX2U Gen 2,
//! 2026-11-19: EQ bands, firmware and serial wrong in both, both exit 0).
//! Every exchange with a real device therefore runs under [`ShureDevice::exclusive()`],
//! which holds that device's lock ([`open_lock_file()`]: on Linux its hidraw
//! node, so every shurectl locks the same file whatever its environment);
//! whatever the other process left queued is drained inside it.

use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use hidapi::{HidApi, HidDevice};

use crate::protocol::{
    self, AutoTone, CompressorPreset, DeviceModel, DeviceState, EqPreset, MV6_GEN2_PID, MV6_PID,
    MV7_PID, MV7_PLUS_PID, MVX2U_GEN2_PID, MicPosition, PACKET_SIZE, PID, Reply, VID, apply_reply,
    cmd_confirm, cmd_factory_reset, cmd_get_adaptation_rate, cmd_get_auto_gain,
    cmd_get_auto_position, cmd_get_auto_tone, cmd_get_compressor, cmd_get_denoiser_level,
    cmd_get_device_name, cmd_get_eq_band_enable, cmd_get_eq_band_gain, cmd_get_eq_enable,
    cmd_get_firmware_version, cmd_get_gain, cmd_get_hpf, cmd_get_limiter, cmd_get_lock,
    cmd_get_mix, cmd_get_mode, cmd_get_mute, cmd_get_mv6_denoiser, cmd_get_mv6_gain_lock,
    cmd_get_mv6_mix, cmd_get_mv6_mute_btn_disable, cmd_get_mv6_popper_stopper, cmd_get_mv6_tone,
    cmd_get_mv7_led_behavior, cmd_get_mv7_led_brightness, cmd_get_mv7_led_live_edge,
    cmd_get_mv7_led_live_interior, cmd_get_mv7_led_live_middle, cmd_get_mv7_led_live_theme,
    cmd_get_mv7_led_pulsing_color, cmd_get_mv7_led_solid_color, cmd_get_mv7_led_solid_theme,
    cmd_get_mv7_playback_mix, cmd_get_mv7_reverb_intensity, cmd_get_mv7_reverb_monitor,
    cmd_get_mv7_reverb_output, cmd_get_mv7_reverb_type, cmd_get_phantom, cmd_get_serial,
    cmd_get_tone_mode, cmd_set_lock, cmd_set_mv7_gain, cmd_set_mv7_led_behavior,
    cmd_set_mv7_led_brightness, cmd_set_mv7_led_live_edge, cmd_set_mv7_led_live_interior,
    cmd_set_mv7_led_live_middle, cmd_set_mv7_led_live_theme, cmd_set_mv7_led_pulsing_color,
    cmd_set_mv7_led_pulsing_theme, cmd_set_mv7_led_solid_color, cmd_set_mv7_led_solid_theme,
    mv7_text, parse_response,
};

#[cfg(target_os = "linux")]
const ACCESS_HINT: &str = "ensure the udev rule is installed, or run with sudo";
#[cfg(target_os = "windows")]
const ACCESS_HINT: &str =
    "ensure no other software (e.g. ShurePlus MOTIV) has exclusive access to the device";
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
const ACCESS_HINT: &str = "ensure the device is plugged in and accessible";

/// How long to wait for a read response, in milliseconds.
const READ_TIMEOUT_MS: i32 = 200;

/// How long to wait for each ack after a CONFIRM, in milliseconds. The MVX2U Gen 2
/// answers within 3 ms; a setting that sends fewer acks than
/// `reports_after_confirm()` costs this much per missing ack. If a model's acks
/// turn out slower than this, they are left queued and refresh shows stale values.
const ACK_TIMEOUT_MS: i32 = 15;

/// The most stray reports (`send_get_past_strays()`) one GET passes over;
/// after that it has no answer.
const STRAY_REPLIES_MAX: usize = 4;

/// How long to wait for an MV7 reply. A mode change takes ~200 ms to answer,
/// longer than a single read timeout, so waiting is bounded by time, not reads.
const MV7_REPLY_TIMEOUT: Duration = Duration::from_millis(1500);

/// How long a TUI step waits for the MV7's value when the live poll before it
/// got none (`reread_mv7_gain()`): longer than the poll's 200 ms, but short
/// enough that a held arrow key can't queue seconds of steps. Unconfirmed on
/// hardware whether a slow MV7 answers within it.
pub const MV7_STEP_REREAD: Duration = Duration::from_millis(500);

/// How long the live poll waits for each MV7 reply, as long as a
/// binary GET waits. The full `MV7_REPLY_TIMEOUT` every second would leave
/// the TUI waiting on a mic that doesn't answer; a reply that comes later is
/// drained before the next command. Unconfirmed on hardware whether `micMute`
/// answers within it.
const MV7_POLL_TIMEOUT: Duration = Duration::from_millis(READ_TIMEOUT_MS as u64);

/// Upper bound on stale input reports discarded before a command. Far more
/// than any reply needs (`help`, the longest MV7 reply, is a few dozen lines);
/// it only keeps the drain loop finite if the device never stops sending.
const DRAIN_MAX_REPORTS: usize = 256;

/// How long an exchange waits for another shurectl to finish its own before
/// giving up. How long others hold the lock is unmeasured; an MV7 readback
/// whose lines go unanswered holds it for seconds (`MV7_REPLY_TIMEOUT` each).
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// How often a waiting exchange tries the lock again.
const LOCK_RETRY: Duration = Duration::from_millis(5);

/// How long a read that only labels a device in a list waits for the lock
/// before showing it as busy, so a scan with several devices doesn't wait
/// [`LOCK_WAIT`] for each. How long other shurectl exchanges hold it is
/// unmeasured; a readback, preset load or `f` holding it longer than this
/// shows the device as busy, and `r` in the picker scans again.
const LABEL_LOCK_WAIT: Duration = Duration::from_secs(1);

/// Another shurectl kept the device for all of the wait: [`LOCK_WAIT`], or
/// [`LABEL_LOCK_WAIT`] for labels, or none for the live poll. Every exchange
/// takes the lock before it sends anything, and each `set_*` is one exchange,
/// so a single write that fails with this sent nothing. A run of writes that
/// must not be split (a preset load) holds [`ShureDevice::exclusive()`] around
/// all of them, so it can't stop with this partway either. Kept as its own type so `main.rs`
/// neither re-reads (waiting as long again) nor reconnects on it.
#[derive(Debug)]
pub struct Busy;

impl std::fmt::Display for Busy {
    /// No closing period: status lines add one before what follows.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Another shurectl has the device")
    }
}

impl std::error::Error for Busy {}

/// A HID read or write failed, or `reopen()` could not find the device: it was
/// unplugged or its handle went stale. Kept as its own type so `main.rs` can
/// tell it apart from a device that is present but refused a command, and show
/// the device as disconnected.
#[derive(Debug)]
pub struct Disconnected(pub String);

impl std::fmt::Display for Disconnected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Disconnected {}

/// The MV7 answered that it didn't take a SET: it is locked, or a re-read
/// showed the setting unchanged. Unlike a timeout or a bare `[Failed]` (which
/// can also mean the mic already had the value), the old value is certain, so
/// `main.rs` can put the screen back to it.
#[derive(Debug)]
pub struct Rejected(pub String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

fn read_failed(e: anyhow::Error) -> anyhow::Error {
    Disconnected(format!("HID read failed (device disconnected?): {e}")).into()
}

/// Shown, and matched on reconnect, when a device reports no USB serial number.
pub const UNKNOWN_SERIAL: &str = "(unknown)";

/// The byte pipe under [`ShureDevice`]. Any error from `write()` or
/// `read_timeout()` means the device is gone.
pub trait HidTransport {
    /// Send one output report. Returns the number of bytes written.
    fn write(&self, packet: &[u8]) -> Result<usize>;
    /// Read one input report into `buf`, waiting up to `timeout_ms`. `Ok(0)` is a timeout.
    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize>;
    /// The USB descriptor serial number, if the device reports one.
    fn serial_number(&self) -> Option<String>;
    /// The USB descriptor product string, if the device reports one.
    fn product(&self) -> Option<String>;
    /// Whether a device with this model and USB serial is still in the OS
    /// device list. Nothing is sent to the device.
    fn is_present(&self, model: DeviceModel, serial: &str) -> Result<bool>;
    /// A new handle to the same device after it was unplugged and plugged back in.
    fn reopen(&self, model: DeviceModel, serial: &str) -> Result<Box<dyn HidTransport>>;
    /// The file whose lock keeps other shurectl processes off this device
    /// while an exchange runs ([`ShureDevice::exclusive()`]), when another
    /// process can reach it. The fake device in tests and `--qa` can't, and
    /// never touches a lock file.
    fn lock_file(&self) -> Option<File> {
        None
    }
}

/// The only transport in a normal build: hidapi on the device's HID path.
pub struct HidapiTransport {
    device: HidDevice,
    /// The OS path it was opened at: `/dev/hidrawN` on Linux, which is also
    /// what [`open_lock_file()`] locks there.
    path: String,
}

impl HidTransport for HidapiTransport {
    fn write(&self, packet: &[u8]) -> Result<usize> {
        Ok(self.device.write(packet)?)
    }

    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize> {
        Ok(self.device.read_timeout(buf, timeout_ms)?)
    }

    fn serial_number(&self) -> Option<String> {
        self.device.get_serial_number_string().ok().flatten()
    }

    fn product(&self) -> Option<String> {
        self.device.get_product_string().ok().flatten()
    }

    fn lock_file(&self) -> Option<File> {
        open_lock_file(&self.path, || self.serial_number())
    }

    fn is_present(&self, model: DeviceModel, serial: &str) -> Result<bool> {
        let api = HidApi::new().context("Failed to initialise hidapi")?;
        Ok(!same_devices(&api, model, serial).is_empty())
    }

    /// A replugged device usually gets a new HID path, so it is found by model
    /// and USB serial number instead. Errors are one line, for the status bar.
    fn reopen(&self, model: DeviceModel, serial: &str) -> Result<Box<dyn HidTransport>> {
        let api = HidApi::new().context("Failed to initialise hidapi")?;
        match same_devices(&api, model, serial).as_slice() {
            [(info, found)] => open_hid(&api, info, *found, "device")
                .map_err(|e| anyhow!("{}", e.to_string().replace('\n', " "))),
            [] => Err(Disconnected(format!("{} not found", model.display_name())).into()),
            [_, _, ..] => Err(anyhow!(
                "more than one matching {} found; restart shurectl with --device",
                model.display_name()
            )),
        }
    }
}

/// The status line for a device shurectl refuses as not genuine: the picker's
/// row status, and the first line of the error from opening one.
pub const NOT_GENUINE: &str = "Not a genuine Shure device: shurectl won't open it";

/// Why, after [`NOT_GENUINE`]: the picker shows it under the devices.
pub const NOT_GENUINE_WHY: [&str; 2] = [
    "It has the Shure MV7's USB ID but calls itself an MV7+. A genuine MV7+ has",
    "its own USB ID. shurectl works only with genuine Shure hardware.",
];

/// Whether a device with `model`'s USB ID is not the Shure device it claims
/// to be, going by the product string in the OS device list. Nothing is sent
/// to the device. `false` when it reports no product string.
fn is_not_genuine(info: &hidapi::DeviceInfo, model: DeviceModel) -> bool {
    info.product_string()
        .is_some_and(|product| model.contradicted_by_product(product))
}

/// Open the HID device at `info`'s path, non-blocking. `what` names it in the
/// error: "device", or the path the user gave with `--device`. A device that
/// is not genuine is refused before it is opened: its shell parses commands
/// differently (`dspMode`, `inputGain`; probed 2026-11-09 by its owner, #139),
/// so a readback fails and its mode and whole-dB gain writes were ignored.
fn open_hid(
    api: &HidApi,
    info: &hidapi::DeviceInfo,
    model: DeviceModel,
    what: &str,
) -> Result<Box<dyn HidTransport>> {
    if is_not_genuine(info, model) {
        return Err(anyhow!("{NOT_GENUINE}.\n{}", NOT_GENUINE_WHY.join(" ")));
    }
    let path = info.path().to_string_lossy();
    let c_path = std::ffi::CString::new(path.as_ref())
        .map_err(|_| anyhow!("Device path contains a null byte: {path}"))?;
    let device = api
        .open_path(c_path.as_c_str())
        .map_err(|e| anyhow!("Cannot open {what}: {e}\nHint: {ACCESS_HINT}."))?;
    device.set_blocking_mode(false).ok();
    Ok(Box::new(HidapiTransport {
        device,
        path: path.into_owned(),
    }))
}

/// Supported devices in `api`'s list with this model and USB serial. One
/// refused as not genuine never counts, so a counterfeit beside the genuine
/// device, cloning its serial or reporting none, can't keep it from
/// reconnecting or from showing as unplugged.
fn same_devices<'a>(
    api: &'a HidApi,
    model: DeviceModel,
    serial: &str,
) -> Vec<(&'a hidapi::DeviceInfo, DeviceModel)> {
    shure_devices(api)
        .into_iter()
        .filter(|(info, found)| {
            *found == model
                && info.serial_number().unwrap_or(UNKNOWN_SERIAL) == serial
                && !is_not_genuine(info, *found)
        })
        .collect()
}

/// What `ShureDevice::read_live()` read: each setting, or `None` without a
/// usable answer.
#[derive(Debug, Default, PartialEq)]
pub struct Live {
    pub muted: Option<bool>,
    pub mode: Option<protocol::InputMode>,
    pub gain_tenths: Option<u16>,
    pub monitor_mix: Option<u8>,
    pub playback_mix: Option<u8>,
    /// The MV7's Mic Position and Tone, which its `dspMode` line holds with
    /// the mode, in Auto Level only: in Manual it reports neither.
    pub auto_level: Option<(MicPosition, AutoTone)>,
}

/// A connected Shure USB microphone or interface.
pub struct ShureDevice {
    transport: Box<dyn HidTransport>,
    /// Which device model this is — drives protocol and UI decisions.
    pub model: DeviceModel,
    /// Packet sequence number. Increments after every write; wraps at 256.
    seq: AtomicU8,
    /// Serial number string read from the USB device descriptor at open time.
    pub serial_number: String,
    /// Product string from the USB device descriptor, which finds the
    /// device's audio input for the meter on some systems.
    pub product: Option<String>,
    /// The lock other shurectl processes take for this device, shared with
    /// every guard on it.
    lock: Arc<DeviceLock>,
}

/// A device's lock, owned jointly by the device and its guards, so a guard
/// can stay held while the device moves (the CLI holds one from a toggle's
/// read through its write).
struct DeviceLock {
    /// This device's lock file ([`open_lock_file()`]), on a transport other
    /// processes can reach; `None` for the fake, or when it can't be opened.
    file: Option<File>,
    /// How many [`Exclusive`] guards are alive: nested exchanges take the
    /// lock file once, the outermost one.
    depth: AtomicUsize,
    /// Whether the outermost guard holds the lock file, and so unlocks it.
    held: AtomicBool,
    /// How many outermost guards were taken, for tests of what shares one.
    #[cfg(test)]
    turns: AtomicUsize,
}

impl DeviceLock {
    fn new(file: Option<File>) -> Arc<Self> {
        Arc::new(Self {
            file,
            depth: AtomicUsize::new(0),
            held: AtomicBool::new(false),
            #[cfg(test)]
            turns: AtomicUsize::new(0),
        })
    }
}

/// Held while this process talks to the device; see [`ShureDevice::exclusive()`].
pub struct Exclusive(Arc<DeviceLock>);

impl Drop for Exclusive {
    fn drop(&mut self) {
        let lock = &self.0;
        if lock.depth.fetch_sub(1, Ordering::Relaxed) == 1
            && lock.held.swap(false, Ordering::Relaxed)
            && let Some(file) = &lock.file
        {
            // A failed unlock leaves it to the OS, which drops the lock when
            // this process exits.
            let _ = file.unlock();
        }
    }
}

/// The file every shurectl locks for the device at `hid_path`. One per
/// device, so a slow exchange with one mic never holds up another. `None` when
/// it can't be opened, and shurectl then runs without it, as before it had one.
///
/// On Linux it is the device's hidraw node itself: the same file for every
/// process whatever its environment or user (`sudo`, cron, a hotkey daemon),
/// and only those allowed to use the device can open it to hold its lock.
/// This second handle is never read; the kernel skips new reports for it once
/// its queue is full (`HIDRAW_BUFFER_SIZE`, 64, in `hidraw_report_event()`,
/// drivers/hid/hidraw.c), so the main handle gets every report as before.
/// A node that can't be opened a second time (an unusual permission set)
/// falls back to a lock file in [`lock_dir()`], `$XDG_RUNTIME_DIR` here,
/// rather than no lock at all.
#[cfg(target_os = "linux")]
fn open_lock_file(hid_path: &str, usb_serial: impl FnOnce() -> Option<String>) -> Option<File> {
    File::open(hid_path)
        .ok()
        .or_else(|| open_temp_lock_file(hid_path, usb_serial))
}

#[cfg(not(target_os = "linux"))]
fn open_lock_file(hid_path: &str, usb_serial: impl FnOnce() -> Option<String>) -> Option<File> {
    open_temp_lock_file(hid_path, usb_serial)
}

/// Elsewhere hidapi's path isn't a file, so the lock is a file named after the
/// USB serial in [`lock_dir()`].
/// A device without one is named by its hidapi path instead, which every
/// process sees the same while it stays plugged in, so two such devices
/// don't share a lock. Whether those systems share input reports between
/// handles at all is unconfirmed (`.claude/rules/protocol.md`).
fn open_temp_lock_file(
    hid_path: &str,
    usb_serial: impl FnOnce() -> Option<String>,
) -> Option<File> {
    let name: String = usb_serial()
        .filter(|serial| !serial.is_empty())
        .unwrap_or_else(|| hid_path.to_string())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path = lock_dir().join(format!("shurectl-{name}.lock"));
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        // A file another user made there may be theirs to write: a lock needs
        // only a handle, so reading it is enough.
        .or_else(|_| File::open(&path))
        .ok()
}

/// Where the lock file goes: `$XDG_RUNTIME_DIR` on Linux (a per-user tmpfs,
/// so another user's stale file can't get in the way), else the temp
/// directory, which is per user on macOS and Windows.
fn lock_dir() -> std::path::PathBuf {
    #[cfg(target_os = "linux")]
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|dir| dir.is_absolute())
    {
        return dir;
    }
    std::env::temp_dir()
}

/// Take `file`'s lock, waiting up to `wait` while another process holds it.
/// `Ok(false)` when the OS can't lock it at all: the exchange then goes ahead
/// unlocked rather than stop every command.
fn wait_for_lock(file: &File, wait: Duration) -> Result<bool> {
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(true),
            Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < wait => {
                std::thread::sleep(LOCK_RETRY);
            }
            Err(std::fs::TryLockError::WouldBlock) => return Err(Busy.into()),
            Err(std::fs::TryLockError::Error(_)) => return Ok(false),
        }
    }
}

impl ShureDevice {
    /// A device on `transport`. The fake device in `fake_hid.rs` comes in here.
    pub fn with_transport(model: DeviceModel, transport: Box<dyn HidTransport>) -> Self {
        let serial_number = transport
            .serial_number()
            .unwrap_or_else(|| UNKNOWN_SERIAL.to_string());
        let product = transport.product();
        let lock = DeviceLock::new(transport.lock_file());
        Self {
            transport,
            model,
            seq: AtomicU8::new(0),
            serial_number,
            product,
            lock,
        }
    }

    /// Keep other shurectl processes off the device until the guard drops, so
    /// a GET is answered only once and nothing of theirs is read as a reply
    /// (module doc, "One shurectl at a time"). Every write and read runs under
    /// one; a readback holds one from its drain to its last GET. Nested guards
    /// lock once. Errors only when another shurectl kept the device for all of
    /// [`LOCK_WAIT`] ([`Busy`]). `main.rs` holds one around a preset load, so
    /// no other shurectl comes between its Gain Lock unlock and re-lock.
    pub fn exclusive(&self) -> Result<Exclusive> {
        self.exclusive_within(LOCK_WAIT)
    }

    /// [`Self::exclusive()`] without the wait: [`Busy`] at once while another
    /// shurectl holds the device. For the TUI's live poll, which skips a turn
    /// rather than stall the screen for [`LOCK_WAIT`] every second.
    pub fn exclusive_now(&self) -> Result<Exclusive> {
        self.exclusive_within(Duration::ZERO)
    }

    /// [`Self::exclusive()`] with a shorter wait, for reads that only label a
    /// device in a list (`--list`, the device picker): [`LABEL_LOCK_WAIT`].
    pub fn exclusive_for_labels(&self) -> Result<Exclusive> {
        self.exclusive_within(LABEL_LOCK_WAIT)
    }

    fn exclusive_within(&self, wait: Duration) -> Result<Exclusive> {
        let lock = &self.lock;
        if lock.depth.load(Ordering::Relaxed) == 0 {
            if let Some(file) = &lock.file {
                let locked = wait_for_lock(file, wait)?;
                lock.held.store(locked, Ordering::Relaxed);
            }
            #[cfg(test)]
            lock.turns.fetch_add(1, Ordering::Relaxed);
        }
        lock.depth.fetch_add(1, Ordering::Relaxed);
        Ok(Exclusive(Arc::clone(lock)))
    }

    /// How many turns at the device this handle has taken so far (outermost
    /// guards), still countable after the device itself is dropped.
    #[cfg(test)]
    pub fn turns(&self) -> impl Fn() -> usize + use<> {
        let lock = Arc::clone(&self.lock);
        move || lock.turns.load(Ordering::Relaxed)
    }

    /// Every write and read checks it runs under [`Self::exclusive()`], so a
    /// debug build (every test, fake device included) panics on a path that
    /// skips one.
    fn debug_assert_exclusive(&self) {
        debug_assert!(
            self.lock.depth.load(Ordering::Relaxed) > 0,
            "device I/O outside ShureDevice::exclusive()"
        );
    }

    /// Open the first (and only) Shure device found on the system.
    ///
    /// Returns an error if zero or more than one Shure device is detected,
    /// not counting one refused as not genuine beside one that isn't.
    /// Use `--device` to select a specific device when multiple are present;
    /// the TUI shows its device picker (`main.rs::pick_device()`) instead.
    pub fn open() -> Result<Self> {
        let api = HidApi::new().context("Failed to initialise hidapi")?;
        let found = shure_devices(&api);
        // A device refused as not genuine doesn't make the user choose. With
        // only refused ones, one is opened so that the refusal says why, rather
        // than ask the user to pick between devices shurectl won't open.
        let genuine: Vec<_> = found
            .iter()
            .filter(|(info, model)| !is_not_genuine(info, *model))
            .collect();
        let candidates = if genuine.is_empty() {
            found.iter().take(1).collect()
        } else {
            genuine
        };

        match candidates.len() {
            0 => Err(anyhow!(
                "No Shure MVX2U, MVX2U Gen 2, MV6, MV6 Gen 2, MV7, or MV7+ device found.\nHint: {ACCESS_HINT}."
            )),
            1 => {
                let (info, model) = *candidates[0];
                Self::open_info(&api, info, model)
            }
            n => Err(anyhow!(
                "{n} Shure devices found. Use --device to specify one.\n\
                Run --list to see available devices and their paths."
            )),
        }
    }

    /// Open the same physical device again after it was unplugged and plugged
    /// back in, when this handle has gone stale. Errors are one line, for the
    /// status bar.
    /// Never under a guard: the new handle has a lock of its own, which on
    /// Linux is a second open of the same hidraw node, so it would wait on
    /// this one's lock and report [`Busy`] with no other shurectl running.
    pub fn reopen(&self) -> Result<Self> {
        debug_assert!(
            self.lock.depth.load(Ordering::Relaxed) == 0,
            "reopen() under ShureDevice::exclusive()"
        );
        let transport = self.transport.reopen(self.model, &self.serial_number)?;
        Ok(Self::with_transport(self.model, transport))
    }

    /// Whether this device is still plugged in, from the OS device list alone.
    /// Nothing is sent to the device, so polling this cannot disturb the
    /// protocol or a command MOTIV has in flight.
    pub fn is_present(&self) -> Result<bool> {
        self.transport.is_present(self.model, &self.serial_number)
    }

    fn open_info(api: &HidApi, info: &hidapi::DeviceInfo, model: DeviceModel) -> Result<Self> {
        Ok(Self::with_transport(
            model,
            open_hid(api, info, model, "device")?,
        ))
    }

    /// Open a Shure device at a specific HID device path.
    pub fn open_path(path: &str) -> Result<Self> {
        let api = HidApi::new().context("Failed to initialise hidapi")?;
        let info = api
            .device_list()
            .find(|d| d.path().to_string_lossy() == path)
            .ok_or_else(|| {
                anyhow!("No device found at {path}. Use --list to see available devices.")
            })?;

        let pid = info.product_id();
        let Some(model) = supported_model(info) else {
            return Err(anyhow!(
                "{path} is not a supported Shure device \
                (VID={:#06x} PID={:#06x}); expected VID={:#06x} with PID={:#06x}, {:#06x}, {:#06x}, {:#06x}, {:#06x}, or {:#06x}.",
                info.vendor_id(),
                pid,
                VID,
                PID,
                MVX2U_GEN2_PID,
                MV6_PID,
                MV6_GEN2_PID,
                MV7_PID,
                MV7_PLUS_PID,
            ));
        };

        Ok(Self::with_transport(
            model,
            open_hid(&api, info, model, path)?,
        ))
    }

    /// Open the device `--device` names: a HID path from `--list`, or a
    /// serial, the one printed on the device (`--list`'s S/N) or the USB one,
    /// compared ignoring case. A path can change when the device is replugged
    /// or the computer restarts; a serial doesn't.
    ///
    /// The printed serial is only known by asking: when nothing matches the
    /// path or USB serial, every Shure device is opened and sent a serial GET,
    /// as `--list` does. On Linux, hidraw gives every open handle each input
    /// report, so a TUI with one of them open receives that reply too.
    pub fn open_selected(selector: &str) -> Result<Self> {
        let api = HidApi::new().context("Failed to initialise hidapi")?;
        if api
            .device_list()
            .any(|d| d.path().to_string_lossy() == selector)
        {
            return Self::open_path(selector);
        }
        let found = shure_devices(&api);
        let same = |serial: &str| serial.eq_ignore_ascii_case(selector);
        let mut matches: Vec<_> = found
            .iter()
            .filter(|(info, _)| info.serial_number().is_some_and(same))
            .collect();
        // Why a device couldn't be asked, for when nothing matches.
        let mut open_error = None;
        let mut busy = false;
        let deadline = Instant::now() + LOCK_WAIT;
        if matches.is_empty() {
            matches = found
                .iter()
                // A refused device has no serial to ask for, and its refusal
                // would hide why another couldn't be opened.
                .filter(|(info, model)| !is_not_genuine(info, *model))
                // The full wait, not `exclusive_for_labels()`'s: this is a
                // command that should wait its turn, not a list. One wait for
                // every device asked, so several busy ones don't wait 5 s each.
                .filter(|(info, model)| match Self::open_info(&api, info, *model) {
                    Ok(dev) => match dev
                        .exclusive_within(deadline.saturating_duration_since(Instant::now()))
                        .and_then(|_turn| dev.read_factory_serial())
                    {
                        Ok(serial) => serial.is_some_and(|serial| same(&serial)),
                        Err(_) => {
                            busy = true;
                            false
                        }
                    },
                    Err(e) => {
                        open_error.get_or_insert(e);
                        false
                    }
                })
                .collect();
        }
        match matches.as_slice() {
            [(info, model)] => Self::open_info(&api, info, *model),
            [] if busy => {
                let busy = format!(
                    "Another shurectl was busy with a Shure device, so its printed serial \
                     couldn't be read to match {selector}. Try again, or use a path from --list."
                );
                // Another device that couldn't be opened may be the one meant.
                Err(match open_error {
                    Some(e) => e.context(format!("{busy} Another one couldn't be opened")),
                    None => anyhow!(busy),
                })
            }
            [] if let Some(e) = open_error => Err(e.context(format!(
                "No Shure device has the path or serial {selector}, and one couldn't be opened"
            ))),
            [] => Err(anyhow!(
                "No Shure device has the path or serial {selector}. Run --list to see them.\n\
                 Hint: {ACCESS_HINT}."
            )),
            several => Err(anyhow!(
                "{} Shure devices have the serial {selector}. Use a path from --list instead.",
                several.len()
            )),
        }
    }

    fn next_seq(&self) -> u8 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    fn write(&self, packet: &[u8]) -> Result<()> {
        self.debug_assert_exclusive();
        let written = self
            .transport
            .write(packet)
            .map_err(|e| Disconnected(format!("HID write failed: {e}")))?;
        if written == 0 {
            return Err(anyhow!("HID write returned 0 bytes"));
        }
        Ok(())
    }

    /// Read one input report. `Ok(None)` means the read timed out, which is not
    /// a HID failure: a feature may not answer a GET (the MV6's Playback Level is
    /// unchecked), and `Readback` decides whether that fails the readback. A
    /// failed read is an error, since it means the device is gone.
    fn read(&self) -> Result<Option<Vec<u8>>> {
        self.debug_assert_exclusive();
        let mut buf = vec![0u8; PACKET_SIZE];
        match self.transport.read_timeout(&mut buf, READ_TIMEOUT_MS) {
            Ok(0) => Ok(None),
            Ok(n) => Ok(Some(buf[..n].to_vec())),
            Err(e) => Err(read_failed(e)),
        }
    }

    /// The MV7 would read a binary packet as a garbage command line, so every
    /// binary path refuses it. Reaching this is a bug in the per-model dispatch.
    fn ensure_binary_protocol(&self) -> Result<()> {
        match self.model {
            DeviceModel::Mv7 => Err(anyhow!("This setting is not available on the MV7")),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => Ok(()),
        }
    }

    fn send_set(&self, set_packet: &[u8]) -> Result<()> {
        self.ensure_binary_protocol()?;
        let _exclusive = self.exclusive()?;
        self.write(set_packet)?;
        let confirm = cmd_confirm(self.next_seq());
        self.write(&confirm)?;
        // Drain the acks sent after every CONFIRM so they don't offset
        // subsequent GET reads in get_state(). Every read is attempted even after
        // a timeout, so a late first ack is still caught by the second read.
        let mut buf = vec![0u8; PACKET_SIZE];
        for _ in 0..self.model.reports_after_confirm() {
            self.debug_assert_exclusive();
            let _ = self.transport.read_timeout(&mut buf, ACK_TIMEOUT_MS);
        }
        Ok(())
    }

    /// Send one MV7 command line and return its reply line (which may be
    /// [`mv7_text::FAILED_REPLY`]). Unrelated lines in between are skipped.
    fn send_text(&self, command: &mv7_text::TextCommand) -> Result<String> {
        self.send_text_within(command, MV7_REPLY_TIMEOUT)
    }

    /// [`Self::send_text`], waiting at most `timeout` for the reply.
    fn send_text_within(
        &self,
        command: &mv7_text::TextCommand,
        timeout: Duration,
    ) -> Result<String> {
        let _exclusive = self.exclusive()?;
        let mut buf = vec![0u8; PACKET_SIZE];
        self.drain_input(&mut buf)?;
        self.write(&command.packet())?;
        let mut received = String::new();
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.debug_assert_exclusive();
            let n = self
                .transport
                .read_timeout(&mut buf, READ_TIMEOUT_MS)
                .map_err(read_failed)?;
            if n == 0 {
                continue;
            }
            let text: Vec<u8> = buf[..n].iter().copied().filter(|&b| b != 0).collect();
            received.push_str(&String::from_utf8_lossy(&text));
            if let Some(reply) = mv7_text::find_reply(&received, &command.reply_prefix) {
                return Ok(reply.to_string());
            }
        }
        Err(anyhow!("No reply from the MV7 to \"{}\"", command.line))
    }

    /// Discard any input still queued from before this command: a reply that
    /// arrived after its command timed out, a late ack, or output from a
    /// command MOTIV sent. On the MV7, `[Failed]` and `Locked` carry no command
    /// name, so a stale one would otherwise be read as the answer to the next
    /// command.
    fn drain_input(&self, buf: &mut [u8]) -> Result<()> {
        self.debug_assert_exclusive();
        for _ in 0..DRAIN_MAX_REPORTS {
            let n = self.transport.read_timeout(buf, 0).map_err(read_failed)?;
            if n == 0 {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Send an MV7 SET and fail if the device rejected it.
    fn send_text_set(&self, command: &mv7_text::TextCommand) -> Result<()> {
        let reply = self.send_text(command)?;
        Self::check_text_reply(command, &reply)
    }

    /// Send an MV7 SET for the setting `label`. `Ok(None)` if the MV7 took it,
    /// otherwise how it refused. No reply at all is still an error.
    fn try_text_set(
        &self,
        label: &'static str,
        command: &mv7_text::TextCommand,
    ) -> Result<Option<Refusal>> {
        let reply = self.send_text(command)?;
        Ok(Refused::from_reply(&reply).map(|how| Refusal { label, how }))
    }

    /// Load a preset onto the MV7: `apply_preset` puts its settings onto the
    /// mic's own readback, and only the settings that then differ from what the
    /// mic has are written: it answers `[Failed]` to a SET that changes nothing.
    /// A refused setting is skipped and the rest still load. Returns the
    /// refused settings, for the status message.
    pub fn load_mv7_preset(
        &self,
        apply_preset: impl Fn(&mut DeviceState),
        write_monitor_mix: bool,
    ) -> Result<Vec<String>> {
        self.ensure_text_protocol()?;
        // Its readbacks and the writes they decide on are one exchange.
        let _exclusive = self.exclusive()?;
        let mut refused = Vec::new();
        let mut current = self.get_state()?.state;
        // The preset applied to what the mic has, so a setting the preset
        // didn't save keeps the mic's value.
        let target_on = |base: &DeviceState| {
            let mut target = base.clone();
            apply_preset(&mut target);
            target
        };
        let mut target = target_on(&current);
        if let Some(command) = mv7_text::mode_change(&current, &target) {
            let refusal = self.try_text_set(MV7_MODE_LABEL, &command)?;
            // Read the mic again either way: a mode change resets EQ and
            // compressor or reports a new gain.
            current = self.get_state()?.state;
            if mv7_text::mode_change(&current, &target).is_some() {
                refused.push(refusal.unwrap_or(Refusal {
                    label: MV7_MODE_LABEL,
                    how: Refused::Failed,
                }));
            }
            target = target_on(&current);
        }
        for (label, command) in mv7_text::setting_changes(&current, &target, write_monitor_mix) {
            refused.extend(self.try_text_set(label, &command)?);
        }
        Ok(refused.iter().map(Refusal::describe).collect())
    }

    fn check_text_reply(command: &mv7_text::TextCommand, reply: &str) -> Result<()> {
        match Refused::from_reply(reply) {
            None => Ok(()),
            Some(Refused::Failed) => Err(anyhow!("The MV7 rejected \"{}\"", command.line)),
            Some(Refused::Locked) => Err(Rejected(format!(
                "The MV7 refused \"{}\": the setting is locked",
                command.line
            ))
            .into()),
        }
    }

    /// Send an MV7 SET whose value the mic may already have. It answers
    /// `[Failed]` to a SET that changes nothing (a gain that rounds to the
    /// current one, `--mute on` when muted), so `[Failed]` is only an error if
    /// `query`'s reply shows the setting is still not `is_set`.
    fn send_text_set_checked(
        &self,
        command: &mv7_text::TextCommand,
        query: &mv7_text::TextCommand,
        is_set: impl FnOnce(&DeviceState) -> bool,
    ) -> Result<()> {
        // One exchange, so that [`Busy`] can only mean nothing was sent.
        let _exclusive = self.exclusive()?;
        let reply = self.send_text(command)?;
        if reply != mv7_text::FAILED_REPLY {
            return Self::check_text_reply(command, &reply);
        }
        let mut state = DeviceState::default();
        let reply = self.send_text(query)?;
        if !mv7_text::apply_reply(&reply, &mut state) {
            // Neither the old nor the new value is certain.
            Err(anyhow!("The MV7 rejected \"{}\"", command.line))
        } else if is_set(&state) {
            Ok(())
        } else {
            Err(Rejected(format!("The MV7 rejected \"{}\"", command.line)).into())
        }
    }

    /// Send one GET and apply its reply to `readback`. A failed write or read is
    /// an error (the device is gone). A timeout or an unrecognised reply is
    /// recorded in `readback`, and fails it when the readback finishes.
    fn query(&self, pkt: &[u8], readback: &mut Readback) -> Result<()> {
        self.query_as(pkt, Need::Required, readback)
    }

    /// [`Self::query`] for a GET that may go unanswered, as `need` says.
    fn query_as(&self, pkt: &[u8], need: Need, readback: &mut Readback) -> Result<()> {
        let reply = self.send_get_past_strays(pkt)?;
        readback.apply(self.model, pkt, reply.as_ref(), need);
        Ok(())
    }

    /// Send the binary GET `get` and read its answer, passing over reports that can't
    /// be it: the late answer to a read that timed out, or a report MOTIV caused.
    /// Another shurectl's replies can't come in between: each exchange holds the lock
    /// file (`exclusive()`), and a readback drains its queue first. Taken as an
    /// answer, a stray makes every later GET of a readback take the one before it,
    /// leaving one setting at its default.
    ///
    /// The live-poll GETs (mute, mode, gain, the monitor levels) are answered at the
    /// address asked, so for those any other report is stray, one that doesn't parse
    /// (an ack) included; for any other GET so is a reply at a live-poll address. When
    /// every report read is stray, there is no answer, and the readback fails rather
    /// than pass with a default. A stray between two other GETs can still shift those,
    /// but never cost a setting the CLI or the poll steps from (a level jump from a
    /// default). Whether every GET is answered at its own address is unconfirmed, so a
    /// general check needs captures first.
    fn send_get_past_strays(&self, get: &[u8]) -> Result<Option<Reply>> {
        self.ensure_binary_protocol()?;
        let _exclusive = self.exclusive()?;
        self.write(get)?;
        let live_get = protocol::is_live_get(get);
        for _ in 0..=STRAY_REPLIES_MAX {
            let Some(report) = self.read()? else {
                return Ok(None);
            };
            let reply = parse_response(&report);
            let stray = match &reply {
                Some(reply) => {
                    !protocol::answers_at_its_address(get, reply)
                        && (live_get || protocol::is_live_reply(reply))
                }
                None => live_get,
            };
            if !stray {
                return Ok(reply);
            }
        }
        Ok(None)
    }

    /// Fetch all 5 EQ band gain values. Used by the MVX2U Gen 1 and Gen 2 and MV6 Gen 2 readback.
    fn fetch_eq_band_gains(&self, readback: &mut Readback) -> Result<()> {
        for band in 0..5 {
            self.query(&cmd_get_eq_band_gain(self.next_seq(), band), readback)?;
        }
        Ok(())
    }

    /// Read the Playback Level. Its reply is told from Mic Level's only by the
    /// echoed prefix 0x03, captured on the MV6 Gen 2, MVX2U Gen 2 (which reads it
    /// in its getters slice) and MV7+ (MOTIV capture, firmware 1.6, every reply
    /// at FEAT_MIX echoing its request's prefix). Unconfirmed on the MV6 (#99):
    /// there a reply that doesn't echo it is passed over as a stray, and the
    /// read must not then stop every write.
    fn query_playback_mix(&self, readback: &mut Readback) -> Result<()> {
        let need = match self.model {
            DeviceModel::Mv6 => Need::Optional,
            // The MVX2U Gen 1 and MV7 have none and never call this.
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7
            | DeviceModel::Mv7Plus => Need::Required,
        };
        self.query_as(&cmd_get_mv7_playback_mix(self.next_seq()), need, readback)
    }

    /// Send each getter and apply its reply to `readback`.
    fn run_getters(&self, getters: &[fn(u8) -> Vec<u8>], readback: &mut Readback) -> Result<()> {
        for getter in getters {
            self.query(&getter(self.next_seq()), readback)?;
        }
        Ok(())
    }

    /// Fetch the complete device state by querying every feature for this model.
    pub fn get_state(&self) -> Result<Readback> {
        // One exchange from the drain to the last GET, so no other shurectl's
        // replies come in between.
        let _exclusive = self.exclusive()?;
        // Input still queued (a late ack, the reply to a mute read that timed
        // out, or another shurectl's) would answer the first GET and shift
        // every later one.
        self.drain_input(&mut [0u8; PACKET_SIZE])?;
        let mut readback = Readback::default();
        readback.state.mute_btn_disabled = None;
        match self.model {
            DeviceModel::Mvx2u => self.get_state_mvx2u(&mut readback),
            DeviceModel::Mvx2uGen2 => self.get_state_mvx2u_gen2(&mut readback),
            DeviceModel::Mv6 => self.get_state_mv6(&mut readback),
            DeviceModel::Mv6Gen2 => self.get_state_mv6_gen2(&mut readback),
            DeviceModel::Mv7 => self.get_state_mv7(&mut readback),
            DeviceModel::Mv7Plus => self.get_state_mv7_plus(&mut readback),
        }?;
        // A gain above the model's maximum is taken as not the device's (a
        // stray or corrupt report), since shown, the next step would write the
        // maximum gain. Unconfirmed whether the MVX2U Gen 1 or MV6 reports one
        // in Auto (a moving Auto gain); if so, that mic's readback always
        // fails. A one-off needn't come twice, so ask once more; only a second
        // one fails the readback, which then shows nothing as the mic's.
        if !self.usable_gain(readback.state.gain_tenths) {
            // Only a lost device is its own error; no answer leaves the
            // message about the gain above the maximum, which says why.
            match self.read_gain_again() {
                Ok(Some(gain_tenths)) => readback.state.gain_tenths = gain_tenths,
                Ok(None) => {}
                Err(e) if e.downcast_ref::<Disconnected>().is_some() => return Err(e),
                Err(_) => {}
            }
        }
        if !self.usable_gain(readback.state.gain_tenths) {
            return Err(anyhow!(
                "Couldn't read a setting: the gain read back, {} dB, is above the {} dB maximum",
                f32::from(readback.state.gain_tenths) / 10.0,
                f32::from(self.model.max_gain_tenths()) / 10.0
            ));
        }
        readback.finish()
    }

    /// The gain read alone, under the caller's guard: the MV7's line with a
    /// readback's wait, or the binary GET after a drain. `None` without a
    /// usable answer.
    fn read_gain_again(&self) -> Result<Option<u16>> {
        match self.model {
            DeviceModel::Mv7 => self.reread_mv7_gain(MV7_REPLY_TIMEOUT),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => {
                self.drain_input(&mut [0u8; PACKET_SIZE])?;
                Ok(self
                    .read_alone(&cmd_get_gain(self.next_seq()))?
                    .map(|state| state.gain_tenths)
                    .filter(|gain_tenths| self.usable_gain(*gain_tenths)))
            }
        }
    }

    /// Whether a gain read back is in the model's range, so usable.
    fn usable_gain(&self, gain_tenths: u16) -> bool {
        gain_tenths <= self.model.max_gain_tenths()
    }

    fn get_state_mv7(&self, readback: &mut Readback) -> Result<()> {
        for command in mv7_text::state_queries() {
            let reply = self.send_text(&command)?;
            let applied = mv7_text::apply_reply(&reply, &mut readback.state);
            readback.record(applied, Need::Required, || {
                format!("{reply:?} in reply to {:?}", command.line)
            });
        }
        // The Limiter block is unconfirmed on hardware (see `protocol::mv7_text`):
        // unanswered, only the Limiter stays unread.
        // No reply at all is unusable too; only a disconnect fails the readback.
        let command = mv7_text::get_limiter();
        readback.state.limiter_enabled = None;
        let reply = match self.send_text(&command) {
            Ok(reply) => reply,
            Err(e) if e.downcast_ref::<Disconnected>().is_some() => return Err(e),
            Err(_) => String::new(),
        };
        let applied = mv7_text::apply_reply(&reply, &mut readback.state);
        readback.record(applied, Need::Optional, || {
            format!("{reply:?} in reply to {:?}", command.line)
        });
        Ok(())
    }

    fn get_state_mvx2u(&self, readback: &mut Readback) -> Result<()> {
        let getters: &[fn(u8) -> Vec<u8>] = &[
            cmd_get_lock,
            cmd_get_gain,
            cmd_get_mute,
            cmd_get_phantom,
            cmd_get_mode,
            cmd_get_auto_position,
            cmd_get_auto_tone,
            cmd_get_auto_gain,
            cmd_get_mix,
            cmd_get_hpf,
            cmd_get_limiter,
            cmd_get_compressor,
            cmd_get_eq_enable,
            cmd_get_device_name,
            cmd_get_firmware_version,
            cmd_get_serial,
        ];

        self.run_getters(getters, readback)?;

        for band in 0..5 {
            self.query(&cmd_get_eq_band_enable(self.next_seq(), band), readback)?;
        }
        self.fetch_eq_band_gains(readback)
    }

    fn get_state_mvx2u_gen2(&self, readback: &mut Readback) -> Result<()> {
        // Gen 2 shares most getters with Gen 1 but has no config lock, no EQ master
        // enable, and no per-band enable. It adds denoiser, popper stopper, gain lock,
        // tone, and the MV6 Gen 2's two monitor levels (Mic Level, Playback Level).
        let getters: &[fn(u8) -> Vec<u8>] = &[
            cmd_get_gain,
            cmd_get_mute,
            cmd_get_phantom,
            cmd_get_mode,
            cmd_get_mv6_mix, // same address as Gen 1 mix; GET uses standard framing
            cmd_get_mv7_playback_mix, // replies echo prefix 0x03 (captured)
            cmd_get_hpf,
            cmd_get_limiter,
            cmd_get_compressor,
            cmd_get_mv6_denoiser,
            cmd_get_mv6_popper_stopper,
            cmd_get_mv6_tone,
            cmd_get_mv6_gain_lock,
            cmd_get_device_name,
            cmd_get_firmware_version,
            cmd_get_serial,
        ];

        self.run_getters(getters, readback)?;

        // Gen 2 has 5-band EQ gain (no master enable, no per-band enable toggle).
        self.fetch_eq_band_gains(readback)
    }

    fn get_state_mv6(&self, readback: &mut Readback) -> Result<()> {
        let getters: &[fn(u8) -> Vec<u8>] = &[
            cmd_get_gain,
            cmd_get_mute,
            cmd_get_hpf,
            cmd_get_mode,
            cmd_get_mv6_denoiser,
            cmd_get_mv6_popper_stopper,
            cmd_get_mv6_tone,
            cmd_get_mv6_gain_lock,
            cmd_get_mv6_mix,
        ];
        self.run_getters(getters, readback)?;
        self.query_playback_mix(readback)?;

        let getters: &[fn(u8) -> Vec<u8>] = &[
            cmd_get_mv6_mute_btn_disable,
            cmd_get_device_name,
            cmd_get_firmware_version,
            cmd_get_serial,
        ];
        self.run_getters(getters, readback)
    }

    fn get_state_mv6_gen2(&self, readback: &mut Readback) -> Result<()> {
        // The MV6 set (including Playback Mix) plus Target Level (the Gen 1 Auto
        // Gain address), Adaptation Rate, Denoiser level, Tone mode and the MVX2U
        // Gen 2 EQ bands.
        self.get_state_mv6(readback)?;
        let getters: &[fn(u8) -> Vec<u8>] = &[
            cmd_get_auto_gain,
            cmd_get_adaptation_rate,
            cmd_get_denoiser_level,
            cmd_get_tone_mode,
        ];
        self.run_getters(getters, readback)?;
        self.fetch_eq_band_gains(readback)
    }

    fn get_state_mv7_plus(&self, readback: &mut Readback) -> Result<()> {
        // Shared features: use standard GET framing (HDR_CONSTANT=0x03).
        let getters: &[fn(u8) -> Vec<u8>] = &[
            cmd_get_gain,
            cmd_get_mute,
            cmd_get_hpf,
            cmd_get_mode,
            cmd_get_limiter,
            cmd_get_compressor,
            cmd_get_mv6_denoiser,
            cmd_get_mv6_popper_stopper,
            cmd_get_mv6_tone,
        ];
        self.run_getters(getters, readback)?;
        // The MV7+ has this setting from firmware 1.4.0, and 1.3.0.13 answers
        // the GET with an unrecognised reply (see cmd_get_mv6_mute_btn_disable()).
        // Do not guess the button state: it stays None.
        let skipped_before_mute_button = readback.skipped.len();
        self.query_as(
            &cmd_get_mv6_mute_btn_disable(self.next_seq()),
            Need::Optional,
            readback,
        )?;
        let mute_button_note = (readback.skipped.len() > skipped_before_mute_button)
            .then_some(skipped_before_mute_button);
        self.query(&cmd_get_mv6_mix(self.next_seq()), readback)?; // Mic Level (prefix=0x00)
        self.query_playback_mix(readback)?;
        let getters: &[fn(u8) -> Vec<u8>] = &[
            cmd_get_mv7_reverb_output,
            cmd_get_mv7_reverb_type,
            cmd_get_mv7_reverb_intensity,
            cmd_get_mv7_reverb_monitor,
            cmd_get_mv7_led_behavior,
            cmd_get_mv7_led_brightness,
            cmd_get_mv7_led_live_theme,
            cmd_get_mv7_led_live_edge,
            cmd_get_mv7_led_live_middle,
            cmd_get_mv7_led_live_interior,
            cmd_get_mv7_led_solid_color,
            cmd_get_mv7_led_pulsing_color,
            cmd_get_mv7_led_solid_theme,
            // Pulsing theme (A6) aliases FEAT_LOCK — not readable via GET.
            cmd_get_device_name,
            cmd_get_firmware_version,
            cmd_get_serial,
        ];
        self.run_getters(getters, readback)?;
        // Firmware is read above without changing GET order. On firmware that
        // predates the setting, its card says so, so it isn't a read error.
        if let Some(index) = mute_button_note
            && readback
                .state
                .mute_button_needs_firmware(self.model)
                .is_some()
        {
            readback.skipped.remove(index);
        }
        Ok(())
    }

    /// Read the mute state alone, for `--mute toggle` and `status`, without a
    /// full readback. `Ok(None)` when the device gave no usable answer (a
    /// timeout, a refusal, another reply); only a HID failure or [`Busy`] is
    /// an error.
    pub fn read_mute(&self) -> Result<Option<bool>> {
        Ok(self
            .read_one(mv7_text::get_mute, cmd_get_mute)?
            .map(|state| state.muted))
    }

    /// Read the gain alone, as [`Self::read_mute`] reads mute.
    pub fn read_gain(&self) -> Result<Option<u16>> {
        Ok(self
            .read_one(mv7_text::get_gain, cmd_get_gain)?
            .map(|state| state.gain_tenths)
            .filter(|gain_tenths| self.usable_gain(*gain_tenths)))
    }

    /// The MV7's gain read alone, waiting up to `wait` for its line: longer
    /// than the live poll's 200 ms a line, which a slow answer may miss.
    /// `None` without a usable answer, and on the other models, whose poll
    /// waited as long as their readback does already.
    pub fn reread_mv7_gain(&self, wait: Duration) -> Result<Option<u16>> {
        Ok(self
            .reread_mv7(&mv7_text::get_gain(), wait)?
            .map(|state| state.gain_tenths)
            .filter(|gain_tenths| self.usable_gain(*gain_tenths)))
    }

    /// The MV7's Monitor Mix, read as [`Self::reread_mv7_gain`] reads the gain.
    pub fn reread_mv7_monitor_mix(&self, wait: Duration) -> Result<Option<u8>> {
        Ok(self
            .reread_mv7(&mv7_text::get_monitor_mix(), wait)?
            .map(|state| state.monitor_mix))
    }

    fn reread_mv7(
        &self,
        command: &mv7_text::TextCommand,
        wait: Duration,
    ) -> Result<Option<DeviceState>> {
        match self.model {
            DeviceModel::Mv7 => {
                let _exclusive = self.exclusive()?;
                let reply = self.send_text_within(command, wait)?;
                let mut state = DeviceState::default();
                Ok(mv7_text::apply_reply(&reply, &mut state).then_some(state))
            }
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => Ok(None),
        }
    }

    /// One setting read alone: the MV7's text line, or the binary GET. On the
    /// binary models, input still queued from earlier (a late ack) is
    /// discarded first, so it can't be read as the answer.
    fn read_one(
        &self,
        text: fn() -> mv7_text::TextCommand,
        binary: fn(u8) -> Vec<u8>,
    ) -> Result<Option<DeviceState>> {
        match self.model {
            DeviceModel::Mv7 => self.read_text_alone(&text()),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => {
                let _exclusive = self.exclusive()?;
                self.drain_input(&mut [0u8; PACKET_SIZE])?;
                self.read_alone(&binary(self.next_seq()))
            }
        }
    }

    /// Read what a script can change (mute, mode, gain and the monitor
    /// levels), each alone, for the TUI's poll, so a change made elsewhere
    /// shows without a full readback: the mic's own button, `shurectl --gain`
    /// from a Stream Deck, or MOTIV. Each is `None` without a usable answer;
    /// only a HID failure or [`Busy`] is an error. Binary models discard
    /// queued input first.
    pub fn read_live(&self) -> Result<Live> {
        let _exclusive = self.exclusive()?;
        let mut live = match self.model {
            DeviceModel::Mv7 => self.read_live_mv7(),
            // The Gen 1 balance and the Mic Level share an address, not a GET.
            DeviceModel::Mvx2u => self.read_live_binary(cmd_get_mix),
            DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => self.read_live_binary(cmd_get_mv6_mix),
        }?;
        // Out of range, the gain is an unusable answer (`get_state()`).
        live.gain_tenths = live
            .gain_tenths
            .filter(|gain_tenths| self.usable_gain(*gain_tenths));
        Ok(live)
    }

    /// The MV7's lines, each waiting at most 200 ms. The first that isn't
    /// answered (MOTIV busy on the shared channel) ends this poll, so the UI
    /// stalls for one line at most, not four.
    fn read_live_mv7(&self) -> Result<Live> {
        let mut live = Live::default();
        let Some(read) = self.read_text_alone(&mv7_text::get_mute())? else {
            return Ok(live);
        };
        live.muted = Some(read.muted);
        let Some(read) = self.read_text_alone(&mv7_text::get_dsp_mode())? else {
            return Ok(live);
        };
        live.mode = Some(read.mode);
        live.auto_level = (read.mode == protocol::InputMode::Auto)
            .then_some((read.auto_position, read.auto_tone));
        let Some(read) = self.read_text_alone(&mv7_text::get_gain())? else {
            return Ok(live);
        };
        live.gain_tenths = Some(read.gain_tenths);
        live.monitor_mix = self
            .read_text_alone(&mv7_text::get_monitor_mix())?
            .map(|read| read.monitor_mix);
        Ok(live)
    }

    fn read_live_binary(&self, monitor_mix: fn(u8) -> Vec<u8>) -> Result<Live> {
        self.drain_input(&mut [0u8; PACKET_SIZE])?;
        let read = |get: Vec<u8>| self.read_alone(&get);
        let muted = read(cmd_get_mute(self.next_seq()))?.map(|s| s.muted);
        let mode = read(cmd_get_mode(self.next_seq()))?.map(|s| s.mode);
        let gain_tenths = read(cmd_get_gain(self.next_seq()))?.map(|s| s.gain_tenths);
        let monitor_mix = read(monitor_mix(self.next_seq()))?.map(|s| s.monitor_mix);
        let playback_mix = if self.model.polls_playback_mix() {
            read(cmd_get_mv7_playback_mix(self.next_seq()))?.map(|s| s.playback_mix)
        } else {
            None
        };
        Ok(Live {
            muted,
            mode,
            gain_tenths,
            monitor_mix,
            playback_mix,
            // The binary models read Mic Position and Tone at their own
            // addresses, which the poll doesn't follow.
            auto_level: None,
        })
    }

    /// One binary GET, its reply applied to a fresh state only if it answers
    /// at the address asked; another report (a late one) is ignored.
    fn read_alone(&self, get: &[u8]) -> Result<Option<DeviceState>> {
        let mut state = DeviceState::default();
        let applied = self.send_get_past_strays(get)?.is_some_and(|reply| {
            protocol::answers_at_its_address(get, &reply) && apply_reply(&reply, &mut state)
        });
        Ok(applied.then_some(state))
    }

    /// One MV7 command, waiting at most the poll's 200 ms, its reply applied to
    /// a fresh state. Only losing the device is an error.
    fn read_text_alone(&self, command: &mv7_text::TextCommand) -> Result<Option<DeviceState>> {
        let mut state = DeviceState::default();
        let applied = match self.send_text_within(command, MV7_POLL_TIMEOUT) {
            Ok(reply) => mv7_text::apply_reply(&reply, &mut state),
            // Busy sent nothing: a fallback readback would only wait as long
            // again for the same lock.
            Err(e)
                if e.downcast_ref::<Disconnected>().is_some()
                    || e.downcast_ref::<Busy>().is_some() =>
            {
                return Err(e);
            }
            Err(_) => false,
        };
        Ok(applied.then_some(state))
    }

    /// Read just the factory serial number (the serial printed on the device,
    /// shown in the MOTIV app) from an opened device. `Ok(None)` if the device
    /// can't be queried or doesn't report one; an error only when another
    /// shurectl kept it [`Busy`], so `--device <serial>` can say so. Used by
    /// `--list` and the picker to show the printed serial instead of the USB
    /// descriptor's.
    pub fn read_factory_serial(&self) -> Result<Option<String>> {
        let _exclusive = self.exclusive()?;
        let mut state = DeviceState::default();
        let applied = match self.model {
            DeviceModel::Mv7 => match self.send_text(&mv7_text::get_serial()) {
                Ok(reply) => mv7_text::apply_reply(&reply, &mut state),
                Err(_) => false,
            },
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => {
                let reply = self
                    .drain_input(&mut [0u8; PACKET_SIZE])
                    .and_then(|()| self.send_get_past_strays(&cmd_get_serial(self.next_seq())));
                // Another reply (another shurectl's) would apply to its own
                // field and leave the serial unknown.
                matches!(reply, Ok(Some(reply))
                    if protocol::is_serial_reply(&reply) && apply_reply(&reply, &mut state))
            }
        };
        Ok(applied.then_some(state.factory_serial))
    }

    /// Read just the user-set device name, as `read_factory_serial()` reads
    /// the serial, for the device picker. `Ok(None)` on the MV7, which has no
    /// name, when none is set, or when the device doesn't answer; an error
    /// only when another shurectl kept it [`Busy`], so the picker can say so.
    pub fn read_device_name(&self) -> Result<Option<String>> {
        // Also keeps the MV7's text shell away from the binary GET.
        if !self.model.has_device_rename() {
            return Ok(None);
        }
        let _exclusive = self.exclusive()?;
        let reply = self
            .drain_input(&mut [0u8; PACKET_SIZE])
            .and_then(|()| self.send_get_past_strays(&cmd_get_device_name(self.next_seq())));
        let Ok(Some(reply)) = reply else {
            return Ok(None);
        };
        let mut state = DeviceState::default();
        let applied = protocol::is_device_name_reply(&reply) && apply_reply(&reply, &mut state);
        let unset = state.device_name.is_empty() || state.device_name == "Unknown";
        Ok((applied && !unset).then_some(state.device_name))
    }

    // ── Shared SET commands ───────────────────────────────────────────────────

    /// Set manual gain in tenths of a dB, clamped and snapped to the model's grid.
    pub fn set_gain(&self, gain_tenths: u16) -> Result<()> {
        let clamped = self.model.snap_gain_tenths(gain_tenths);
        let pkt = match self.model {
            DeviceModel::Mv7 => {
                return self.send_text_set_checked(
                    &mv7_text::set_gain(clamped),
                    &mv7_text::get_gain(),
                    |state| state.gain_tenths == mv7_text::snap_gain(clamped),
                );
            }
            DeviceModel::Mv7Plus => cmd_set_mv7_gain(self.next_seq(), clamped),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2 => protocol::cmd_set_gain(self.next_seq(), clamped),
        };
        self.send_set(&pkt)
    }

    /// Switch between Auto Level and Manual. The MV7 stores mode, mic position
    /// and tone as one setting, so they are passed along; other models ignore them.
    pub fn set_mode(&self, auto: bool, position: MicPosition, tone: AutoTone) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7 => {
                let mode = if auto {
                    protocol::InputMode::Auto
                } else {
                    protocol::InputMode::Manual
                };
                return self.send_text_set(&mv7_text::set_dsp_mode(mode, position, tone));
            }
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_mode(self.next_seq(), auto),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2 => protocol::cmd_set_mode(self.next_seq(), auto),
        };
        self.send_set(&pkt)
    }

    pub fn set_mute(&self, muted: bool) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7 => {
                return self.send_text_set_checked(
                    &mv7_text::set_mute(muted),
                    &mv7_text::get_mute(),
                    |state| state.muted == muted,
                );
            }
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_mute(self.next_seq(), muted),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2 => protocol::cmd_set_mute(self.next_seq(), muted),
        };
        self.send_set(&pkt)
    }

    pub fn set_hpf(&self, freq: &protocol::HpfFrequency) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7 => {
                return Err(anyhow!(
                    "HPF is not available on the {}",
                    self.model.display_name()
                ));
            }
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_hpf(self.next_seq(), freq),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2 => protocol::cmd_set_hpf(self.next_seq(), freq),
        };
        self.send_set(&pkt)
    }

    // ── MVX2U-first SET commands (Auto Level, dynamics, EQ, lock) ───────────

    /// Set the Auto Level mic position. `tone` is only used by the MV7, which
    /// stores both in one setting.
    pub fn set_auto_position(&self, position: MicPosition, tone: AutoTone) -> Result<()> {
        match self.model {
            DeviceModel::Mv7 => self.set_mv7_auto_level(position, tone),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => {
                self.send_set(&protocol::cmd_set_auto_position(self.next_seq(), &position))
            }
        }
    }

    /// Set the Auto Level tone. `position` is only used by the MV7, which
    /// stores both in one setting.
    pub fn set_auto_tone(&self, position: MicPosition, tone: AutoTone) -> Result<()> {
        match self.model {
            DeviceModel::Mv7 => self.set_mv7_auto_level(position, tone),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => {
                self.send_set(&protocol::cmd_set_auto_tone(self.next_seq(), &tone))
            }
        }
    }

    fn set_mv7_auto_level(&self, position: MicPosition, tone: AutoTone) -> Result<()> {
        let command = mv7_text::set_dsp_mode(protocol::InputMode::Auto, position, tone);
        self.send_text_set(&command)
    }

    pub fn set_auto_gain(&self, gain: &protocol::AutoGain) -> Result<()> {
        self.send_set(&protocol::cmd_set_auto_gain(self.next_seq(), gain))
    }

    pub fn set_phantom(&self, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_phantom(self.next_seq(), enabled))
    }

    pub fn set_limiter(&self, enabled: bool) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7 => return self.send_text_set(&mv7_text::set_limiter(enabled)),
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_limiter(self.next_seq(), enabled),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2 => protocol::cmd_set_limiter(self.next_seq(), enabled),
        };
        self.send_set(&pkt)
    }

    pub fn set_compressor(&self, preset: &CompressorPreset) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7 => return self.send_text_set(&mv7_text::set_compressor(*preset)),
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_compressor(self.next_seq(), preset),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2 => protocol::cmd_set_compressor(self.next_seq(), preset),
        };
        self.send_set(&pkt)
    }

    pub fn set_eq_enable(&self, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_eq_enable(self.next_seq(), enabled))
    }

    pub fn set_eq_band_enable(&self, band: usize, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_eq_band_enable(
            self.next_seq(),
            band,
            enabled,
        ))
    }

    pub fn set_eq_band_gain(&self, band: usize, gain_tenths: i16) -> Result<()> {
        self.send_set(&protocol::cmd_set_eq_band_gain(
            self.next_seq(),
            band,
            gain_tenths,
            self.model,
        ))
    }

    pub fn set_lock(&self, locked: bool) -> Result<()> {
        match self.model {
            DeviceModel::Mv7 => self.send_text_set(&mv7_text::set_lock(locked)),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => self.send_set(&cmd_set_lock(self.next_seq(), locked)),
        }
    }

    // ── MV6-first and MV6 Gen 2 SET commands, device name, Monitor Mix ──────

    pub fn set_mv6_denoiser(&self, enabled: bool) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_denoiser(self.next_seq(), enabled),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7 => protocol::cmd_set_mv6_denoiser(self.next_seq(), enabled),
        };
        self.send_set(&pkt)
    }

    pub fn set_adaptation_rate(&self, fast: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_adaptation_rate(self.next_seq(), fast))
    }

    pub fn set_mic_preset(&self, preset: protocol::MicPreset) -> Result<()> {
        self.send_set(&protocol::cmd_set_mic_preset(self.next_seq(), preset))
    }

    pub fn set_tone_mode(&self, advanced: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_tone_mode(self.next_seq(), advanced))
    }

    pub fn set_denoiser_level(&self, level: u8) -> Result<()> {
        self.send_set(&protocol::cmd_set_denoiser_level(self.next_seq(), level))
    }

    pub fn set_mv6_popper_stopper(&self, enabled: bool) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_popper_stopper(self.next_seq(), enabled),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7 => protocol::cmd_set_mv6_popper_stopper(self.next_seq(), enabled),
        };
        self.send_set(&pkt)
    }

    /// Disable or enable the physical mute button. The MV6, MV6 Gen 2 and MV7+ share
    /// identical framing here.
    pub fn set_mv6_mute_btn_disable(&self, disabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv6_mute_btn_disable(
            self.next_seq(),
            disabled,
        ))
    }

    pub fn set_mv6_tone(&self, tone: i8) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_tone(self.next_seq(), tone),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7 => protocol::cmd_set_mv6_tone(self.next_seq(), tone),
        };
        self.send_set(&pkt)
    }

    /// Set the user-set device name. The MV7 has none.
    pub fn set_device_name(&self, name: &str) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2 => protocol::cmd_set_device_name(self.next_seq(), name),
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_device_name(self.next_seq(), name),
            DeviceModel::Mv7 => {
                return Err(anyhow!(
                    "Renaming is not available on the {}",
                    self.model.display_name()
                ));
            }
        };
        self.send_set(&pkt)
    }

    pub fn set_mv6_gain_lock(&self, locked: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv6_gain_lock(self.next_seq(), locked))
    }

    /// Set the Mic Level, or the mic ↔ playback balance on the MVX2U Gen 1 and MV7.
    pub fn set_monitor_mix(&self, mix: u8) -> Result<()> {
        let pkt = match self.model {
            // 1% and 2% encode as 0%, so a step onto that floor changes
            // nothing. The MV7 answers [Failed] to an unchanged Monitor Mix
            // (noted as seen in 2.12.0, 2026-10-15, no capture recorded; see
            // .claude/rules/protocol.md); for the floor that is unconfirmed.
            // Not an error when the re-read matches.
            DeviceModel::Mv7 => {
                let stored = mv7_text::monitor_mix_as_stored(mix);
                return self.send_text_set_checked(
                    &mv7_text::set_monitor_mix(mix),
                    &mv7_text::get_monitor_mix(),
                    |state| state.monitor_mix == stored,
                );
            }
            // The Gen 1 balance is prefix 0x01 with the standard header.
            DeviceModel::Mvx2u => protocol::cmd_set_mix(self.next_seq(), mix),
            DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => protocol::cmd_set_mv6_mix(self.next_seq(), mix),
        };
        self.send_set(&pkt)
    }

    // ── MV7 exclusive SET commands ────────────────────────────────────────────

    pub fn set_eq_preset(&self, preset: EqPreset) -> Result<()> {
        self.ensure_text_protocol()?;
        self.send_text_set(&mv7_text::set_eq(preset))
    }

    pub fn set_led_live_meter(&self, enabled: bool) -> Result<()> {
        self.ensure_text_protocol()?;
        self.send_text_set(&mv7_text::set_live_meter(enabled))
    }

    pub fn set_led_night_mode(&self, enabled: bool) -> Result<()> {
        self.ensure_text_protocol()?;
        self.send_text_set(&mv7_text::set_night_mode(enabled))
    }

    /// Counterpart of `ensure_binary_protocol()` for the MV7-only setters.
    fn ensure_text_protocol(&self) -> Result<()> {
        match self.model {
            DeviceModel::Mv7 => Ok(()),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => Err(anyhow!(
                "This setting is only available on the MV7, not the {}",
                self.model.display_name()
            )),
        }
    }

    // ── MV7+ exclusive SET commands ───────────────────────────────────────────

    pub fn set_mv7_playback_mix(&self, mix: u8) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv7_playback_mix(self.next_seq(), mix))
    }

    pub fn set_mv7_reverb_output(&self, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv7_reverb_output(
            self.next_seq(),
            enabled,
        ))
    }

    pub fn set_mv7_reverb_monitor(&self, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv7_reverb_monitor(
            self.next_seq(),
            enabled,
        ))
    }

    pub fn set_mv7_reverb_type(&self, rtype: &protocol::ReverbType) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv7_reverb_type(self.next_seq(), rtype))
    }

    pub fn set_mv7_reverb_intensity(&self, intensity: u8) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv7_reverb_intensity(
            self.next_seq(),
            intensity,
        ))
    }

    pub fn set_mv7_led_behavior(&self, behavior: protocol::LedBehavior) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_behavior(self.next_seq(), behavior))
    }

    pub fn set_mv7_led_brightness(&self, brightness: protocol::LedBrightness) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_brightness(self.next_seq(), brightness))
    }

    pub fn set_mv7_led_live_theme(&self, theme: protocol::LedLiveTheme) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_live_theme(self.next_seq(), theme))
    }

    pub fn set_mv7_led_solid_theme(&self, theme: protocol::LedSolidTheme) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_solid_theme(self.next_seq(), theme))
    }

    pub fn set_mv7_led_pulsing_theme(&self, theme: protocol::LedPulsingTheme) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_pulsing_theme(self.next_seq(), theme))
    }

    pub fn set_mv7_led_solid_color(&self, rgb: [u8; 3]) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_solid_color(
            self.next_seq(),
            rgb[0],
            rgb[1],
            rgb[2],
        ))
    }

    pub fn set_mv7_led_pulsing_color(&self, rgb: [u8; 3]) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_pulsing_color(
            self.next_seq(),
            rgb[0],
            rgb[1],
            rgb[2],
        ))
    }

    pub fn set_mv7_led_live_edge(&self, rgb: [u8; 3]) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_live_edge(
            self.next_seq(),
            rgb[0],
            rgb[1],
            rgb[2],
        ))
    }

    pub fn set_mv7_led_live_middle(&self, rgb: [u8; 3]) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_live_middle(
            self.next_seq(),
            rgb[0],
            rgb[1],
            rgb[2],
        ))
    }

    pub fn set_mv7_led_live_interior(&self, rgb: [u8; 3]) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_live_interior(
            self.next_seq(),
            rgb[0],
            rgb[1],
            rgb[2],
        ))
    }

    /// Send a factory reset to the MV7+, MV6 Gen 2 or MVX2U Gen 2.
    ///
    /// The device disconnects and re-enumerates immediately; no CONFIRM is sent.
    /// After this call succeeds the device handle is stale — do not use it again.
    /// Refuses every other model: the packet is only captured on these three, and
    /// the original MV7 has no factory reset.
    pub fn factory_reset(&self) -> Result<()> {
        match self.model {
            // The MV6 Gen 2 and MVX2U Gen 2 take the same packet (captured from
            // MOTIV's Reset to defaults).
            DeviceModel::Mv7Plus | DeviceModel::Mv6Gen2 | DeviceModel::Mvx2uGen2 => {
                let _exclusive = self.exclusive()?;
                self.write(&cmd_factory_reset(self.next_seq()))
            }
            DeviceModel::Mvx2u | DeviceModel::Mv6 | DeviceModel::Mv7 => Err(anyhow!(
                "Factory reset is not available on the {}",
                self.model.display_name()
            )),
        }
    }
}

/// Label of the MV7's mode setting in a preset load's refusals.
const MV7_MODE_LABEL: &str = "Mode";

/// How the MV7 refused a SET.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Refused {
    /// `[Failed]`: rejected, or it changed nothing.
    Failed,
    /// `Locked`: the setting can't change right now.
    Locked,
}

impl Refused {
    /// The refusal `reply` carries, or `None` if the MV7 took the SET.
    fn from_reply(reply: &str) -> Option<Self> {
        match reply {
            mv7_text::FAILED_REPLY => Some(Self::Failed),
            mv7_text::LOCKED_REPLY => Some(Self::Locked),
            _ => None,
        }
    }
}

/// A setting the MV7 refused during a preset load.
struct Refusal {
    label: &'static str,
    how: Refused,
}

impl Refusal {
    /// The setting's label for the status bar, marked when it was locked.
    fn describe(&self) -> String {
        match self.how {
            Refused::Failed => self.label.to_string(),
            Refused::Locked => format!("{} (locked)", self.label),
        }
    }
}

/// A full state readback from [`ShureDevice::get_state`]. A query without a
/// usable reply (a timeout, or a reply that matched no known setting) leaves
/// its setting at `DeviceState::default()`, and a write based on that, such as
/// one gain step up from the default 36 dB, could jump the level. So
/// [`Readback::finish`] fails the whole readback unless every required query
/// was answered.
#[derive(Default)]
pub struct Readback {
    pub state: DeviceState,
    /// Optional queries that got no usable reply. The caller reports them: in
    /// the TUI that is the status bar, since writing to stderr would garble
    /// the screen.
    pub skipped: Vec<String>,
    /// The optional queries in `skipped`, as the GET packets sent, so a
    /// caller can tell which setting went unread.
    pub skipped_gets: Vec<Vec<u8>>,
    /// Required queries that got no usable reply.
    missing: Vec<String>,
    /// Replies applied to `state`. Zero means the device answered nothing.
    applied: usize,
}

/// Whether a readback fails without a usable reply to a query.
#[derive(Clone, Copy)]
enum Need {
    Required,
    /// Unconfirmed on the model's hardware, or missing from some firmware:
    /// noted in [`Readback::skipped`].
    Optional,
}

impl Readback {
    /// Apply the reply, if any, to the binary GET `request` sent to `model`.
    fn apply(&mut self, model: DeviceModel, request: &[u8], reply: Option<&Reply>, need: Need) {
        let applied = reply.is_some_and(|reply| apply_reply(reply, &mut self.state));
        if !applied && matches!(need, Need::Optional) {
            self.skipped_gets.push(request.to_vec());
        }
        self.record(applied, need, || match reply {
            Some(reply) => format!(
                "{} in reply to {}",
                reply.describe(),
                protocol::describe_get(model, request)
            ),
            None => format!(
                "no usable reply to {}",
                protocol::describe_get(model, request)
            ),
        });
    }

    /// Count an applied reply, or note a query without one as `describe()`.
    fn record(&mut self, applied: bool, need: Need, describe: impl FnOnce() -> String) {
        if applied {
            self.applied += 1;
            return;
        }
        match need {
            Need::Required => self.missing.push(describe()),
            Need::Optional => self.skipped.push(describe()),
        }
    }

    /// The readback, unless a required query went unanswered.
    fn finish(self) -> Result<Self> {
        if self.applied == 0 {
            return Err(anyhow!("No response from the device to any state query"));
        }
        match self.missing.as_slice() {
            [] => Ok(self),
            [only] => Err(anyhow!("Couldn't read a setting: {only}")),
            [first, ..] => Err(anyhow!(
                "Couldn't read {} settings, first: {first}",
                self.missing.len()
            )),
        }
    }
}

/// Information about a detected Shure device, returned by [`list_devices`].
pub struct DeviceInfo {
    pub path: String,
    pub serial: String,
    pub model: DeviceModel,
    /// Refused as not genuine ([`NOT_GENUINE`]): listed so its owner sees
    /// why, never opened.
    pub not_genuine: bool,
}

/// The model of a HID device if it is a supported Shure device.
fn supported_model(info: &hidapi::DeviceInfo) -> Option<DeviceModel> {
    if info.vendor_id() != VID {
        return None;
    }
    DeviceModel::from_pid(info.product_id())
}

/// The lowest vendor-defined HID usage page.
const VENDOR_USAGE_PAGE_MIN: u16 = 0xFF00;

/// Enumerate supported Shure devices, one entry per physical device.
///
/// Three steps:
/// 1. Filter to VID/PID candidates.
/// 2. If any candidate advertises a vendor-defined HID usage page (0xFF00–0xFFFF),
///    restrict to those — this drops the Windows telephony/mute-button collection
///    (usage page 0x000B) that appears as a phantom second entry on Windows.
///    Linux backends sometimes report usage_page=0 (descriptor not parsed); in
///    that case no candidate looks vendor-defined, so all are kept and step 3
///    handles deduplication.
/// 3. Deduplicate by path — on Linux all collections share one /dev/hidrawN path.
fn shure_devices(api: &HidApi) -> Vec<(&hidapi::DeviceInfo, DeviceModel)> {
    let candidates: Vec<(&hidapi::DeviceInfo, DeviceModel)> = api
        .device_list()
        .filter_map(|d| supported_model(d).map(|model| (d, model)))
        .collect();

    let has_vendor = candidates
        .iter()
        .any(|(d, _)| d.usage_page() >= VENDOR_USAGE_PAGE_MIN);

    let mut seen_paths = std::collections::HashSet::new();
    candidates
        .into_iter()
        .filter(|(d, _)| !has_vendor || d.usage_page() >= VENDOR_USAGE_PAGE_MIN)
        .filter(|(d, _)| seen_paths.insert(d.path().to_string_lossy().into_owned()))
        .collect()
}

/// Probe the system for supported Shure devices without opening them. Fails
/// only when hidapi can't start, which says nothing about the devices, so
/// callers report it rather than treat it as none connected.
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let api = HidApi::new().context("Failed to initialise hidapi")?;
    Ok(shure_devices(&api)
        .into_iter()
        .map(|(d, model)| DeviceInfo {
            path: d.path().to_string_lossy().into_owned(),
            serial: d.serial_number().unwrap_or(UNKNOWN_SERIAL).to_owned(),
            model,
            not_genuine: is_not_genuine(d, model),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unknown_reply() -> Reply {
        Reply {
            prefix: 0x00,
            feat: [0x7F, 0x7F],
            value: vec![1],
        }
    }

    /// The reply to `cmd_get_gain()` for 24 dB.
    fn gain_reply() -> Reply {
        use protocol::fake_wire::{Parsed, Request, as_reply, parse_request};
        let Some(Parsed {
            request: Request::Get(key),
            ..
        }) = parse_request(&cmd_get_gain(0))
        else {
            panic!("cmd_get_gain() is a GET");
        };
        as_reply(key, &2400u16.to_be_bytes())
    }

    #[test]
    fn readback_names_the_query_an_unrecognised_reply_answered() {
        let mut readback = Readback::default();
        readback.apply(
            DeviceModel::Mv6Gen2,
            &cmd_get_gain(0),
            Some(&unknown_reply()),
            Need::Optional,
        );
        let gain = protocol::describe_get(DeviceModel::Mv6Gen2, &cmd_get_gain(0));
        assert_eq!(
            readback.skipped,
            vec![format!(
                "feature 0x7f 0x7f (prefix 0x00) in reply to {gain}"
            )]
        );
    }

    #[test]
    fn refusal_marks_a_locked_setting() {
        let failed = Refusal {
            label: "Gain",
            how: Refused::Failed,
        };
        let locked = Refusal {
            label: MV7_MODE_LABEL,
            how: Refused::Locked,
        };
        assert_eq!(Refused::from_reply("[Failed]"), Some(Refused::Failed));
        assert_eq!(Refused::from_reply("Locked"), Some(Refused::Locked));
        assert_eq!(Refused::from_reply("micMute=on"), None);
        assert_eq!(failed.describe(), "Gain");
        assert_eq!(locked.describe(), "Mode (locked)");
    }

    #[test]
    fn readback_with_no_applied_reply_is_an_error() {
        let mut readback = Readback::default();
        readback.apply(
            DeviceModel::Mv6Gen2,
            &cmd_get_gain(0),
            Some(&unknown_reply()),
            Need::Optional,
        );
        let err = readback.finish().err().expect("nothing applied");
        assert!(err.to_string().contains("No response"), "{err}");
    }

    /// A setting left at its default (36 dB gain) must not pass as read.
    #[test]
    fn readback_fails_unless_every_required_query_is_answered() {
        let mut readback = Readback::default();
        readback.apply(
            DeviceModel::Mv6Gen2,
            &cmd_get_gain(0),
            Some(&gain_reply()),
            Need::Required,
        );
        readback.apply(DeviceModel::Mv6Gen2, &cmd_get_mute(1), None, Need::Required);
        let err = readback.finish().err().expect("mute unanswered");
        assert_eq!(
            err.to_string(),
            format!(
                "Couldn't read a setting: no usable reply to {}",
                protocol::describe_get(DeviceModel::Mv6Gen2, &cmd_get_mute(1))
            )
        );

        let mut readback = Readback::default();
        readback.apply(
            DeviceModel::Mv6Gen2,
            &cmd_get_gain(0),
            Some(&gain_reply()),
            Need::Required,
        );
        readback.apply(
            DeviceModel::Mv6Gen2,
            &cmd_get_mute(1),
            Some(&unknown_reply()),
            Need::Required,
        );
        readback.apply(DeviceModel::Mv6Gen2, &cmd_get_hpf(2), None, Need::Required);
        let err = readback.finish().err().expect("two unanswered");
        assert!(
            err.to_string()
                .starts_with("Couldn't read 2 settings, first: feature 0x7f")
        );
    }

    #[test]
    fn readback_only_notes_an_unanswered_optional_query() {
        let mut readback = Readback::default();
        readback.apply(
            DeviceModel::Mv6Gen2,
            &cmd_get_gain(0),
            Some(&gain_reply()),
            Need::Required,
        );
        readback.apply(
            DeviceModel::Mv6Gen2,
            &cmd_get_mv7_playback_mix(1),
            None,
            Need::Optional,
        );
        let readback = readback
            .finish()
            .expect("only an optional query unanswered");
        assert_eq!(readback.state.gain_tenths, 240);
        assert_eq!(readback.skipped.len(), 1);
    }

    /// A lock file at `path` for the lock tests: each handle stands in for one
    /// process, as the lock belongs to the open file.
    fn lock_test_file(path: &std::path::Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .expect("lock file")
    }

    /// A second shurectl waits while the lock file is held, gives up with the
    /// busy error after the wait, and gets it once it is released. Two handles
    /// on one file stand in for two processes: the lock belongs to the open file.
    #[test]
    fn a_held_lock_file_keeps_another_handle_waiting() {
        let path = std::env::temp_dir().join(format!("shurectl-lock-test-{}", std::process::id()));
        let open = || lock_test_file(&path);
        let (first, second) = (open(), open());
        assert!(wait_for_lock(&first, Duration::ZERO).expect("free lock"));
        let busy = wait_for_lock(&second, Duration::from_millis(20)).expect_err("held lock");
        assert!(busy.downcast_ref::<Busy>().is_some(), "{busy}");
        first.unlock().expect("unlock");
        assert!(wait_for_lock(&second, Duration::ZERO).expect("released lock"));
        drop((first, second));
        let _ = std::fs::remove_file(&path);
    }

    /// The live poll's guard gives up at once on a held lock instead of
    /// waiting, and takes it once it is free.
    #[test]
    fn the_live_polls_guard_doesnt_wait_for_a_held_lock() {
        use crate::fake_hid::FakeDevice;

        let path =
            std::env::temp_dir().join(format!("shurectl-poll-lock-test-{}", std::process::id()));
        let open = || lock_test_file(&path);
        let mut device = FakeDevice::new(DeviceModel::Mvx2uGen2).open();
        device.lock = DeviceLock::new(Some(open()));
        let other = open();
        other.lock().expect("other shurectl's lock");
        let start = Instant::now();
        let busy = device.exclusive_now().err().expect("held lock");
        assert!(busy.downcast_ref::<Busy>().is_some(), "{busy}");
        assert!(start.elapsed() < Duration::from_millis(100), "waited");
        other.unlock().expect("unlock");
        let turn = device.exclusive_now().expect("free lock");
        drop(turn);
        drop((device, other));
        let _ = std::fs::remove_file(&path);
    }

    /// A guard stays held when the device moves, as the CLI's toggle moves it
    /// into the write path between its read and its write: another process
    /// can't take the lock in between, and the writes inside don't wait.
    #[test]
    fn a_guard_holds_the_lock_while_the_device_moves() {
        use crate::fake_hid::FakeDevice;

        let path =
            std::env::temp_dir().join(format!("shurectl-move-lock-test-{}", std::process::id()));
        let mut device = FakeDevice::new(DeviceModel::Mvx2uGen2).open();
        device.lock = DeviceLock::new(Some(lock_test_file(&path)));
        let other = lock_test_file(&path);
        let turn = device.exclusive().expect("free lock");
        let moved = Some(device);
        assert!(matches!(
            other.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        let start = Instant::now();
        moved
            .as_ref()
            .expect("device")
            .set_mute(true)
            .expect("mute");
        assert!(start.elapsed() < Duration::from_millis(100), "waited");
        drop(turn);
        other.try_lock().expect("released with the guard");
        drop((moved, other));
        let _ = std::fs::remove_file(&path);
    }

    /// Exchanges nest: a readback's GETs run inside its own guard, and every
    /// guard is gone afterwards, so the next exchange takes the lock again.
    #[test]
    fn nested_exchanges_leave_no_guard_behind() {
        use crate::fake_hid::FakeDevice;

        let fake = FakeDevice::new(DeviceModel::Mvx2uGen2);
        let device = fake.open();
        device.get_state().expect("readback");
        device.set_mute(true).expect("mute");
        assert_eq!(device.lock.depth.load(Ordering::Relaxed), 0);
        let outer = device.exclusive().expect("outer");
        let inner = device.exclusive().expect("inner");
        assert_eq!(device.lock.depth.load(Ordering::Relaxed), 2);
        drop((inner, outer));
        assert_eq!(device.lock.depth.load(Ordering::Relaxed), 0);
    }

    /// A readback that fails names the setting, so a user's report says which.
    /// Every GET a binary model reads its state with needs a label in
    /// `protocol::setting_label()`.
    #[test]
    fn every_readback_query_names_its_setting() {
        use crate::fake_hid::{FakeDevice, PacketKind};

        let binary_models = [
            DeviceModel::Mvx2u,
            DeviceModel::Mvx2uGen2,
            DeviceModel::Mv6,
            DeviceModel::Mv6Gen2,
            DeviceModel::Mv7Plus,
        ];
        for model in binary_models {
            let fake = FakeDevice::new(model);
            fake.open().get_state().expect("readback");
            for packet in fake.log_since(0) {
                if let PacketKind::Get { feature } = packet.kind {
                    let described = protocol::describe_get(model, &packet.raw);
                    assert!(
                        described.contains(" at feature"),
                        "{model:?} {feature}: {described}"
                    );
                }
            }
        }
        assert_eq!(
            protocol::describe_get(DeviceModel::Mv6Gen2, &cmd_get_auto_gain(0)),
            "Target Level at feature 0x01 0x87 (prefix 0x00)"
        );
    }
}
