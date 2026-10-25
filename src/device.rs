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

use std::sync::atomic::{AtomicU8, Ordering};
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
const UNKNOWN_SERIAL: &str = "(unknown)";

/// The byte pipe under [`ShureDevice`]. Any error from `write()` or
/// `read_timeout()` means the device is gone.
pub trait HidTransport {
    /// Send one output report. Returns the number of bytes written.
    fn write(&self, packet: &[u8]) -> Result<usize>;
    /// Read one input report into `buf`, waiting up to `timeout_ms`. `Ok(0)` is a timeout.
    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize>;
    /// The USB descriptor serial number, if the device reports one.
    fn serial_number(&self) -> Option<String>;
    /// Whether a device with this model and USB serial is still in the OS
    /// device list. Nothing is sent to the device.
    fn is_present(&self, model: DeviceModel, serial: &str) -> Result<bool>;
    /// A new handle to the same device after it was unplugged and plugged back in.
    fn reopen(&self, model: DeviceModel, serial: &str) -> Result<Box<dyn HidTransport>>;
}

/// The only transport in a normal build: hidapi on the device's HID path.
pub struct HidapiTransport(HidDevice);

impl HidTransport for HidapiTransport {
    fn write(&self, packet: &[u8]) -> Result<usize> {
        Ok(self.0.write(packet)?)
    }

    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize> {
        Ok(self.0.read_timeout(buf, timeout_ms)?)
    }

    fn serial_number(&self) -> Option<String> {
        self.0.get_serial_number_string().ok().flatten()
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
            [(info, _)] => open_hid(&api, info, "device")
                .map_err(|e| anyhow!("{}", e.to_string().replace('\n', " "))),
            [] => Err(Disconnected(format!("{} not found", model.display_name())).into()),
            [_, _, ..] => Err(anyhow!(
                "more than one matching {} found; restart shurectl with --device",
                model.display_name()
            )),
        }
    }
}

/// Open the HID device at `info`'s path, non-blocking. `what` names it in the
/// error: "device", or the path the user gave with `--device`.
fn open_hid(api: &HidApi, info: &hidapi::DeviceInfo, what: &str) -> Result<Box<dyn HidTransport>> {
    let path = info.path().to_string_lossy();
    let c_path = std::ffi::CString::new(path.as_ref())
        .map_err(|_| anyhow!("Device path contains a null byte: {path}"))?;
    let device = api
        .open_path(c_path.as_c_str())
        .map_err(|e| anyhow!("Cannot open {what}: {e}\nHint: {ACCESS_HINT}."))?;
    device.set_blocking_mode(false).ok();
    Ok(Box::new(HidapiTransport(device)))
}

/// Supported devices in `api`'s list with this model and USB serial.
fn same_devices<'a>(
    api: &'a HidApi,
    model: DeviceModel,
    serial: &str,
) -> Vec<(&'a hidapi::DeviceInfo, DeviceModel)> {
    shure_devices(api)
        .into_iter()
        .filter(|(info, found)| {
            *found == model && info.serial_number().unwrap_or(UNKNOWN_SERIAL) == serial
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
}

impl ShureDevice {
    /// A device on `transport`. The fake device in `fake_hid.rs` comes in here.
    pub fn with_transport(model: DeviceModel, transport: Box<dyn HidTransport>) -> Self {
        let serial_number = transport
            .serial_number()
            .unwrap_or_else(|| UNKNOWN_SERIAL.to_string());
        Self {
            transport,
            model,
            seq: AtomicU8::new(0),
            serial_number,
        }
    }

    /// Open the first (and only) Shure device found on the system.
    ///
    /// Returns an error if zero or more than one Shure device is detected.
    /// Use `--device` to select a specific device when multiple are present.
    pub fn open() -> Result<Self> {
        let api = HidApi::new().context("Failed to initialise hidapi")?;
        let found = shure_devices(&api);

        match found.len() {
            0 => Err(anyhow!(
                "No Shure MVX2U, MVX2U Gen 2, MV6, MV6 Gen 2, MV7, or MV7+ device found.\nHint: {ACCESS_HINT}."
            )),
            1 => {
                let (info, model) = found[0];
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
    pub fn reopen(&self) -> Result<Self> {
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
        Ok(Self::with_transport(model, open_hid(api, info, "device")?))
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

        Ok(Self::with_transport(model, open_hid(&api, info, path)?))
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
        if matches.is_empty() {
            matches = found
                .iter()
                .filter(|(info, model)| match Self::open_info(&api, info, *model) {
                    Ok(dev) => dev
                        .read_factory_serial()
                        .is_some_and(|serial| same(&serial)),
                    Err(e) => {
                        open_error.get_or_insert(e);
                        false
                    }
                })
                .collect();
        }
        match matches.as_slice() {
            [(info, model)] => Self::open_info(&api, info, *model),
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
        self.write(set_packet)?;
        let confirm = cmd_confirm(self.next_seq());
        self.write(&confirm)?;
        // Drain the acks sent after every CONFIRM so they don't offset
        // subsequent GET reads in get_state(). Every read is attempted even after
        // a timeout, so a late first ack is still caught by the second read.
        let mut buf = vec![0u8; PACKET_SIZE];
        for _ in 0..self.model.reports_after_confirm() {
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
        let mut buf = vec![0u8; PACKET_SIZE];
        self.drain_input(&mut buf)?;
        self.write(&command.packet())?;
        let mut received = String::new();
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
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
    /// be it: the late answer to a read that timed out or, on Linux, where hidraw gives
    /// every open handle each report, another shurectl's (a running TUI's poll). Taken
    /// as an answer, a stray makes every later GET of a readback take the one before
    /// it, leaving one setting at its default.
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
    /// echoed prefix 0x03, captured on the MV6 Gen 2 and MVX2U Gen 2 (which reads
    /// it in its getters slice). Unconfirmed on the MV6 (#99) and inferred on the
    /// MV7+ from its LED replies: there a reply that doesn't echo it is passed
    /// over as a stray, and the read must not then stop every write.
    fn query_playback_mix(&self, readback: &mut Readback) -> Result<()> {
        let need = match self.model {
            DeviceModel::Mv6 | DeviceModel::Mv7Plus => Need::Optional,
            // The MVX2U Gen 1 and MV7 have none and never call this.
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7 => Need::Required,
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
        // Input still queued (a late ack, or the reply to a mute read that
        // timed out) would answer the first GET and shift every later one.
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
        readback.finish()
    }

    fn get_state_mv7(&self, readback: &mut Readback) -> Result<()> {
        for command in mv7_text::state_queries() {
            let reply = self.send_text(&command)?;
            let applied = mv7_text::apply_reply(&reply, &mut readback.state);
            readback.record(applied, Need::Required, || {
                format!("{reply:?} in reply to {:?}", command.line)
            });
        }
        // The Limiter block is unconfirmed on hardware (see protocol.rs
        // `mv7_text`): unanswered, only the Limiter stays unread.
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
    /// timeout, a refusal, another reply); only a HID failure is an error. On
    /// the binary models, input still queued from earlier (a late ack) is
    /// discarded first, so it can't be read as the answer.
    pub fn read_mute(&self) -> Result<Option<bool>> {
        let read = match self.model {
            DeviceModel::Mv7 => self.read_text_alone(&mv7_text::get_mute())?,
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => {
                self.drain_input(&mut [0u8; PACKET_SIZE])?;
                self.read_alone(&cmd_get_mute(self.next_seq()))?
            }
        };
        Ok(read.map(|state| state.muted))
    }

    /// Read what a script can change (mute, mode, gain and the monitor
    /// levels), each alone, for the TUI's poll, so a change made elsewhere
    /// shows without a full readback: the mic's own button, `shurectl --gain`
    /// from a Stream Deck, or MOTIV. Each is `None` without a usable answer;
    /// only a HID failure is an error. Binary models discard queued input first.
    /// `playback` is false while the last readback found the Playback Level
    /// unanswered: polling it would cost every poll a timeout.
    pub fn read_live(&self, playback: bool) -> Result<Live> {
        match self.model {
            DeviceModel::Mv7 => self.read_live_mv7(),
            // The Gen 1 balance and the Mic Level share an address, not a GET.
            DeviceModel::Mvx2u => self.read_live_binary(cmd_get_mix, playback),
            DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => self.read_live_binary(cmd_get_mv6_mix, playback),
        }
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
        let Some(read) = self.read_text_alone(&mv7_text::get_gain())? else {
            return Ok(live);
        };
        live.gain_tenths = Some(read.gain_tenths);
        live.monitor_mix = self
            .read_text_alone(&mv7_text::get_monitor_mix())?
            .map(|read| read.monitor_mix);
        Ok(live)
    }

    fn read_live_binary(&self, monitor_mix: fn(u8) -> Vec<u8>, playback: bool) -> Result<Live> {
        self.drain_input(&mut [0u8; PACKET_SIZE])?;
        let read = |get: Vec<u8>| self.read_alone(&get);
        let muted = read(cmd_get_mute(self.next_seq()))?.map(|s| s.muted);
        let mode = read(cmd_get_mode(self.next_seq()))?.map(|s| s.mode);
        let gain_tenths = read(cmd_get_gain(self.next_seq()))?.map(|s| s.gain_tenths);
        let monitor_mix = read(monitor_mix(self.next_seq()))?.map(|s| s.monitor_mix);
        let playback_mix = match self.model {
            DeviceModel::Mvx2uGen2 | DeviceModel::Mv6Gen2 | DeviceModel::Mv7Plus if playback => {
                read(cmd_get_mv7_playback_mix(self.next_seq()))?.map(|s| s.playback_mix)
            }
            DeviceModel::Mvx2uGen2 | DeviceModel::Mv6Gen2 | DeviceModel::Mv7Plus => None,
            // The MV6's Playback Level GET is unconfirmed (#99): unanswered it
            // would cost every poll a timeout. None on models without one.
            DeviceModel::Mv6 | DeviceModel::Mvx2u | DeviceModel::Mv7 => None,
        };
        Ok(Live {
            muted,
            mode,
            gain_tenths,
            monitor_mix,
            playback_mix,
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
            Err(e) if e.downcast_ref::<Disconnected>().is_some() => return Err(e),
            Err(_) => false,
        };
        Ok(applied.then_some(state))
    }

    /// Read just the factory serial number (the serial printed on the device,
    /// shown in the MOTIV app) from an opened device. Returns `None` if the device
    /// can't be queried or doesn't report one. Used by `--list` to upgrade the
    /// displayed serial from the USB descriptor value to the printed serial.
    pub fn read_factory_serial(&self) -> Option<String> {
        let mut state = DeviceState::default();
        let applied = match self.model {
            DeviceModel::Mv7 => {
                let reply = self.send_text(&mv7_text::get_serial()).ok()?;
                mv7_text::apply_reply(&reply, &mut state)
            }
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => {
                let reply = self
                    .send_get_past_strays(&cmd_get_serial(self.next_seq()))
                    .ok()??;
                // Another reply (another shurectl's) would apply to its own
                // field and leave the serial unknown.
                protocol::is_serial_reply(&reply) && apply_reply(&reply, &mut state)
            }
        };
        applied.then_some(state.factory_serial)
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
            DeviceModel::Mv7 => return self.send_text_set(&mv7_text::set_monitor_mix(mix)),
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

/// Probe the system for supported Shure devices without opening them.
pub fn list_devices() -> Vec<DeviceInfo> {
    let Ok(api) = HidApi::new() else {
        return vec![];
    };
    shure_devices(&api)
        .into_iter()
        .map(|(d, model)| DeviceInfo {
            path: d.path().to_string_lossy().into_owned(),
            serial: d.serial_number().unwrap_or(UNKNOWN_SERIAL).to_owned(),
            model,
        })
        .collect()
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
