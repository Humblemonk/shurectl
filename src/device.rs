//! Device I/O: wraps hidapi for Shure USB microphones.
//!
//! Supports five devices:
//!   - Shure MVX2U       (VID 0x14ED, PID 0x1013) — XLR-to-USB interface (Gen 1)
//!   - Shure MVX2U Gen 2 (VID 0x14ED, PID 0x1033) — XLR-to-USB interface (Gen 2)
//!   - Shure MV6         (VID 0x14ED, PID 0x1026) — USB gaming microphone
//!   - Shure MV7         (VID 0x14ED, PID 0x1012) — USB/XLR dynamic microphone (original)
//!   - Shure MV7+        (VID 0x14ED, PID 0x1019) — USB/XLR dynamic microphone (protocol unverified)
//!
//! All five devices expose a USB HID configuration interface alongside their
//! audio interface. hidapi opens it via /dev/hidrawN on Linux, IOKit on macOS,
//! and \\.\HID#VID_... paths on Windows, bypassing the audio driver entirely.
//!
//! # Transport
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
//! through `send_text()`; the binary `send_set()`/`send_get()` refuse to run on it.
//!
//! # Sequence numbers
//!
//! Each packet carries an incrementing sequence number (0–255, wrapping). The
//! device echoes this number in its response. We track it in `ShureDevice` and
//! increment after every `write()`.
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
    self, AutoTone, CompressorPreset, DeviceModel, DeviceState, EqPreset, MV6_PID, MV7_PID,
    MV7_PLUS_PID, MVX2U_GEN2_PID, MicPosition, PACKET_SIZE, PID, VID, apply_response, cmd_confirm,
    cmd_factory_reset, cmd_get_auto_gain, cmd_get_auto_position, cmd_get_auto_tone,
    cmd_get_compressor, cmd_get_device_name, cmd_get_eq_band_enable, cmd_get_eq_band_gain,
    cmd_get_eq_enable, cmd_get_firmware_version, cmd_get_gain, cmd_get_hpf, cmd_get_limiter,
    cmd_get_lock, cmd_get_mix, cmd_get_mode, cmd_get_mute, cmd_get_mv6_denoiser,
    cmd_get_mv6_gain_lock, cmd_get_mv6_mix, cmd_get_mv6_mute_btn_disable,
    cmd_get_mv6_popper_stopper, cmd_get_mv6_tone, cmd_get_mv7_led_behavior,
    cmd_get_mv7_led_brightness, cmd_get_mv7_led_live_edge, cmd_get_mv7_led_live_interior,
    cmd_get_mv7_led_live_middle, cmd_get_mv7_led_live_theme, cmd_get_mv7_led_pulsing_color,
    cmd_get_mv7_led_solid_color, cmd_get_mv7_led_solid_theme, cmd_get_mv7_playback_mix,
    cmd_get_mv7_reverb_intensity, cmd_get_mv7_reverb_monitor, cmd_get_mv7_reverb_output,
    cmd_get_mv7_reverb_type, cmd_get_phantom, cmd_get_serial, cmd_set_lock, cmd_set_mv7_gain,
    cmd_set_mv7_led_behavior, cmd_set_mv7_led_brightness, cmd_set_mv7_led_live_edge,
    cmd_set_mv7_led_live_interior, cmd_set_mv7_led_live_middle, cmd_set_mv7_led_live_theme,
    cmd_set_mv7_led_pulsing_color, cmd_set_mv7_led_pulsing_theme, cmd_set_mv7_led_solid_color,
    cmd_set_mv7_led_solid_theme, mv7_text, parse_response, parse_response_with_prefix,
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

/// How long to wait for an MV7 reply. A mode change takes ~200 ms to answer,
/// longer than a single read timeout, so waiting is bounded by time, not reads.
const MV7_REPLY_TIMEOUT: Duration = Duration::from_millis(1500);

/// A connected Shure USB microphone or interface.
pub struct ShureDevice {
    device: HidDevice,
    /// Which device model this is — drives protocol and UI decisions.
    pub model: DeviceModel,
    /// Packet sequence number. Increments after every write; wraps at 256.
    seq: AtomicU8,
    /// Serial number string read from the USB device descriptor at open time.
    pub serial_number: String,
}

impl ShureDevice {
    fn from_hid_device(device: HidDevice, model: DeviceModel) -> Self {
        device.set_blocking_mode(false).ok();
        let serial_number = device
            .get_serial_number_string()
            .ok()
            .flatten()
            .unwrap_or_else(|| "(unknown)".to_string());
        Self {
            device,
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
                "No Shure MVX2U, MVX2U Gen 2, MV6, MV7, or MV7+ device found.\nHint: {ACCESS_HINT}."
            )),
            1 => {
                let (info, model) = found[0];
                let c_path = std::ffi::CString::new(info.path().to_string_lossy().as_ref())
                    .map_err(|_| anyhow!("Device path contains a null byte"))?;
                let device = api
                    .open_path(c_path.as_c_str())
                    .map_err(|e| anyhow!("Cannot open device: {e}\nHint: {ACCESS_HINT}."))?;
                Ok(Self::from_hid_device(device, model))
            }
            n => Err(anyhow!(
                "{n} Shure devices found. Use --device to specify one.\n\
                Run --list to see available devices and their paths."
            )),
        }
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
                (VID={:#06x} PID={:#06x}); expected VID={:#06x} with PID={:#06x}, {:#06x}, {:#06x}, {:#06x}, or {:#06x}.",
                info.vendor_id(),
                pid,
                VID,
                PID,
                MVX2U_GEN2_PID,
                MV6_PID,
                MV7_PID,
                MV7_PLUS_PID,
            ));
        };

        let c_path = std::ffi::CString::new(path)
            .map_err(|_| anyhow!("Device path contains a null byte: {path}"))?;
        let device = api
            .open_path(c_path.as_c_str())
            .map_err(|e| anyhow!("Cannot open {path}: {e}\nHint: {ACCESS_HINT}."))?;
        Ok(Self::from_hid_device(device, model))
    }

    fn next_seq(&self) -> u8 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    fn write(&self, packet: &[u8]) -> Result<()> {
        let written = self.device.write(packet).context("HID write failed")?;
        if written == 0 {
            return Err(anyhow!("HID write returned 0 bytes"));
        }
        Ok(())
    }

    fn read(&self) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; PACKET_SIZE];
        match self.device.read_timeout(&mut buf, READ_TIMEOUT_MS) {
            Ok(0) => Err(anyhow!("HID read timed out — no response from device")),
            Ok(n) => Ok(buf[..n].to_vec()),
            Err(e) => Err(anyhow!("HID read failed (device disconnected?): {e}")),
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
            | DeviceModel::Mv7Plus => Ok(()),
        }
    }

    fn send_set(&self, set_packet: &[u8]) -> Result<()> {
        self.ensure_binary_protocol()?;
        self.write(set_packet)?;
        let confirm = cmd_confirm(self.next_seq());
        self.write(&confirm)?;
        // The MV7+ sends two unsolicited IN responses after every CONFIRM:
        // 1. a CONFIRM ACK (cmd=[09 00 00])
        // 2. a SET echo (RES_SET_FEAT or RES_SET_LOCK with the applied value)
        // Drain both so they don't offset subsequent GET reads in get_state().
        if self.model == DeviceModel::Mv7Plus {
            let mut buf = vec![0u8; PACKET_SIZE];
            for _ in 0..2 {
                let _ = self.device.read_timeout(&mut buf, 50);
            }
        }
        Ok(())
    }

    fn send_get(&self, get_packet: &[u8]) -> Result<Option<([u8; 2], Vec<u8>)>> {
        self.ensure_binary_protocol()?;
        self.write(get_packet)?;
        let buf = self.read()?;
        Ok(parse_response(&buf))
    }

    /// Like `send_get` but returns `(prefix, feat_addr, value)`.
    /// Used for MV7+ playback mix which shares a feature address with mic mix.
    #[allow(clippy::type_complexity)]
    fn send_get_with_prefix(&self, get_packet: &[u8]) -> Result<Option<(u8, [u8; 2], Vec<u8>)>> {
        self.ensure_binary_protocol()?;
        self.write(get_packet)?;
        let buf = self.read()?;
        Ok(parse_response_with_prefix(&buf))
    }

    /// Send one MV7 command line and return its reply line (which may be
    /// [`mv7_text::FAILED_REPLY`]). Unrelated lines in between are skipped.
    fn send_text(&self, command: &mv7_text::TextCommand) -> Result<String> {
        self.write(&command.packet())?;
        let mut received = String::new();
        let mut buf = vec![0u8; PACKET_SIZE];
        let deadline = Instant::now() + MV7_REPLY_TIMEOUT;
        while Instant::now() < deadline {
            let n = self
                .device
                .read_timeout(&mut buf, READ_TIMEOUT_MS)
                .map_err(|e| anyhow!("HID read failed (device disconnected?): {e}"))?;
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

    /// Send an MV7 SET and fail if the device rejected it.
    fn send_text_set(&self, command: &mv7_text::TextCommand) -> Result<()> {
        let reply = self.send_text(command)?;
        Self::check_text_reply(command, &reply)
    }

    fn check_text_reply(command: &mv7_text::TextCommand, reply: &str) -> Result<()> {
        match reply {
            mv7_text::FAILED_REPLY => Err(anyhow!("The MV7 rejected \"{}\"", command.line)),
            mv7_text::LOCKED_REPLY => Err(anyhow!(
                "The MV7 refused \"{}\": the setting is locked",
                command.line
            )),
            _ => Ok(()),
        }
    }

    /// The MV7 answers `[Failed]` when a gain rounds to the value it already
    /// has, so a rejected gain SET is only an error if the gain is still wrong.
    fn send_text_set_gain(&self, gain_tenths: u16) -> Result<()> {
        let command = mv7_text::set_gain(gain_tenths);
        let reply = self.send_text(&command)?;
        if reply != mv7_text::FAILED_REPLY {
            return Self::check_text_reply(&command, &reply);
        }
        let mut state = DeviceState::default();
        let reply = self.send_text(&mv7_text::get_gain())?;
        if mv7_text::apply_reply(&reply, &mut state)
            && state.gain_tenths == mv7_text::snap_gain(gain_tenths)
        {
            Ok(())
        } else {
            Err(anyhow!("The MV7 rejected \"{}\"", command.line))
        }
    }

    /// Fetch all 5 EQ band gain values and apply them to `state`.
    /// Used by both Gen 1 and Gen 2 state readback.
    fn fetch_eq_band_gains(&self, state: &mut DeviceState, context: &str) {
        for band in 0..5 {
            let gain_pkt = cmd_get_eq_band_gain(self.next_seq(), band);
            if let Ok(Some((feat, value))) = self.send_get(&gain_pkt)
                && !apply_response(feat, &value, state)
            {
                eprintln!(
                    "{context}: unrecognised feature {feat:#04x?} in EQ band {band} gain response"
                );
            }
        }
    }

    /// Send each getter, apply the response to `state`, and log any unrecognised features.
    fn run_getters(&self, getters: &[fn(u8) -> Vec<u8>], state: &mut DeviceState, context: &str) {
        for getter in getters {
            let pkt = getter(self.next_seq());
            if let Ok(Some((feat, value))) = self.send_get(&pkt)
                && !apply_response(feat, &value, state)
            {
                eprintln!("{context}: unrecognised feature {feat:#04x?} in response");
            }
        }
    }

    /// Fetch the complete device state by querying every feature for this model.
    pub fn get_state(&self) -> Result<DeviceState> {
        match self.model {
            DeviceModel::Mvx2u => self.get_state_mvx2u(),
            DeviceModel::Mvx2uGen2 => self.get_state_mvx2u_gen2(),
            DeviceModel::Mv6 => self.get_state_mv6(),
            DeviceModel::Mv7 => self.get_state_mv7(),
            DeviceModel::Mv7Plus => self.get_state_mv7_plus(),
        }
    }

    fn get_state_mv7(&self) -> Result<DeviceState> {
        let mut state = DeviceState::default();
        for command in mv7_text::state_queries() {
            let reply = self.send_text(&command)?;
            if !mv7_text::apply_reply(&reply, &mut state) {
                eprintln!(
                    "get_state(mv7): unrecognised reply {reply:?} to {:?}",
                    command.line
                );
            }
        }
        Ok(state)
    }

    fn get_state_mvx2u(&self) -> Result<DeviceState> {
        let mut state = DeviceState::default();

        let lock_pkt = cmd_get_lock(self.next_seq());
        if let Ok(Some((feat, value))) = self.send_get(&lock_pkt)
            && !apply_response(feat, &value, &mut state)
        {
            eprintln!("get_state: unrecognised feature {feat:#04x?} in lock response");
        }

        let getters: &[fn(u8) -> Vec<u8>] = &[
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

        self.run_getters(getters, &mut state, "get_state");

        for band in 0..5 {
            let en_pkt = cmd_get_eq_band_enable(self.next_seq(), band);
            if let Ok(Some((feat, value))) = self.send_get(&en_pkt)
                && !apply_response(feat, &value, &mut state)
            {
                eprintln!(
                    "get_state: unrecognised feature {feat:#04x?} in EQ band {band} enable response"
                );
            }
        }
        self.fetch_eq_band_gains(&mut state, "get_state");

        Ok(state)
    }

    fn get_state_mvx2u_gen2(&self) -> Result<DeviceState> {
        let mut state = DeviceState::default();

        // Gen 2 shares most getters with Gen 1 but has no config lock, no EQ master
        // enable, and no per-band enable. It adds denoiser, popper stopper, gain lock,
        // tone, and uses the MV6-style monitor mix framing.
        let getters: &[fn(u8) -> Vec<u8>] = &[
            cmd_get_gain,
            cmd_get_mute,
            cmd_get_phantom,
            cmd_get_mode,
            cmd_get_mv6_mix, // same address as Gen 1 mix; GET uses standard framing
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

        self.run_getters(getters, &mut state, "get_state(mvx2u_gen2)");

        // Gen 2 has 5-band EQ gain (no master enable, no per-band enable toggle).
        self.fetch_eq_band_gains(&mut state, "get_state(mvx2u_gen2)");

        Ok(state)
    }

    fn get_state_mv6(&self) -> Result<DeviceState> {
        let mut state = DeviceState::default();

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
            cmd_get_mv6_mute_btn_disable,
            cmd_get_device_name,
            cmd_get_firmware_version,
            cmd_get_serial,
        ];

        self.run_getters(getters, &mut state, "get_state(mv6)");

        Ok(state)
    }

    fn get_state_mv7_plus(&self) -> Result<DeviceState> {
        let mut state = DeviceState::default();

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
            cmd_get_mv6_mute_btn_disable,
            cmd_get_mv6_mix, // mic monitor mix (prefix=0x00)
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
        self.run_getters(getters, &mut state, "get_state(mv7plus)");

        // Playback mix uses the same FEAT_MIX address as mic mix but with prefix=0x03.
        // We issued the request so we know the response is the playback mix channel.
        let pmix_pkt = cmd_get_mv7_playback_mix(self.next_seq());
        if let Ok(Some((_prefix, _feat, value))) = self.send_get_with_prefix(&pmix_pkt)
            && !value.is_empty()
        {
            state.playback_mix = value[0].min(100);
        }

        Ok(state)
    }

    /// Read just the factory serial number (the serial printed on the device,
    /// shown in the MOTIV app) from an opened device. Returns `None` if the device
    /// can't be queried or doesn't report one. Used by `--list` to upgrade the
    /// displayed serial from the USB descriptor value to the printed serial.
    pub fn read_factory_serial(&self) -> Option<String> {
        if self.model == DeviceModel::Mv7 {
            let reply = self.send_text(&mv7_text::get_serial()).ok()?;
            let mut state = DeviceState::default();
            return mv7_text::apply_reply(&reply, &mut state).then_some(state.factory_serial);
        }
        let pkt = cmd_get_serial(self.next_seq());
        let (feat, value) = self.send_get(&pkt).ok()??;
        let mut state = DeviceState::default();
        apply_response(feat, &value, &mut state).then_some(state.factory_serial)
    }

    // ── Shared SET commands ───────────────────────────────────────────────────

    /// Set manual gain in tenths of a dB. Clamped to the model's maximum.
    pub fn set_gain(&self, gain_tenths: u16) -> Result<()> {
        let clamped = gain_tenths.min(self.model.max_gain_tenths());
        let pkt = match self.model {
            DeviceModel::Mv7 => return self.send_text_set_gain(clamped),
            DeviceModel::Mv7Plus => cmd_set_mv7_gain(self.next_seq(), clamped),
            DeviceModel::Mvx2u | DeviceModel::Mvx2uGen2 | DeviceModel::Mv6 => {
                protocol::cmd_set_gain(self.next_seq(), clamped)
            }
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
            DeviceModel::Mvx2u | DeviceModel::Mvx2uGen2 | DeviceModel::Mv6 => {
                protocol::cmd_set_mode(self.next_seq(), auto)
            }
        };
        self.send_set(&pkt)
    }

    pub fn set_mute(&self, muted: bool) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7 => return self.send_text_set(&mv7_text::set_mute(muted)),
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_mute(self.next_seq(), muted),
            DeviceModel::Mvx2u | DeviceModel::Mvx2uGen2 | DeviceModel::Mv6 => {
                protocol::cmd_set_mute(self.next_seq(), muted)
            }
        };
        self.send_set(&pkt)
    }

    pub fn set_hpf(&self, freq: &protocol::HpfFrequency) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_hpf(self.next_seq(), freq),
            _ => protocol::cmd_set_hpf(self.next_seq(), freq),
        };
        self.send_set(&pkt)
    }

    // ── MVX2U Gen 1 and Gen 2 SET commands ───────────────────────────────────

    /// Set the Auto Level mic position. `tone` is only used by the MV7, which
    /// stores both in one setting.
    pub fn set_auto_position(&self, position: MicPosition, tone: AutoTone) -> Result<()> {
        match self.model {
            DeviceModel::Mv7 => self.set_mv7_auto_level(position, tone),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
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

    pub fn set_monitor_mix(&self, mix: u8) -> Result<()> {
        self.send_set(&protocol::cmd_set_mix(self.next_seq(), mix))
    }

    pub fn set_limiter(&self, enabled: bool) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_limiter(self.next_seq(), enabled),
            _ => protocol::cmd_set_limiter(self.next_seq(), enabled),
        };
        self.send_set(&pkt)
    }

    pub fn set_compressor(&self, preset: &CompressorPreset) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7 => return self.send_text_set(&mv7_text::set_compressor(*preset)),
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_compressor(self.next_seq(), preset),
            DeviceModel::Mvx2u | DeviceModel::Mvx2uGen2 | DeviceModel::Mv6 => {
                protocol::cmd_set_compressor(self.next_seq(), preset)
            }
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
            | DeviceModel::Mv7Plus => self.send_set(&cmd_set_lock(self.next_seq(), locked)),
        }
    }

    // ── MV6 and MV7+ shared SET commands ─────────────────────────────────────

    pub fn set_mv6_denoiser(&self, enabled: bool) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_denoiser(self.next_seq(), enabled),
            _ => protocol::cmd_set_mv6_denoiser(self.next_seq(), enabled),
        };
        self.send_set(&pkt)
    }

    pub fn set_mv6_popper_stopper(&self, enabled: bool) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_popper_stopper(self.next_seq(), enabled),
            _ => protocol::cmd_set_mv6_popper_stopper(self.next_seq(), enabled),
        };
        self.send_set(&pkt)
    }

    /// Disable or enable the physical mute button. MV6 and MV7+ share identical framing here.
    pub fn set_mv6_mute_btn_disable(&self, disabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv6_mute_btn_disable(
            self.next_seq(),
            disabled,
        ))
    }

    pub fn set_mv6_tone(&self, tone: i8) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_tone(self.next_seq(), tone),
            _ => protocol::cmd_set_mv6_tone(self.next_seq(), tone),
        };
        self.send_set(&pkt)
    }

    pub fn set_mv6_gain_lock(&self, locked: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv6_gain_lock(self.next_seq(), locked))
    }

    /// Set monitor mic mix. MV7+ uses cmd_set_mv7_mic_mix (HDR_CONST=0x00 with prefix=0x00).
    pub fn set_mv6_monitor_mix(&self, mix: u8) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7 => return self.send_text_set(&mv7_text::set_monitor_mix(mix)),
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_mic_mix(self.next_seq(), mix),
            DeviceModel::Mvx2u | DeviceModel::Mvx2uGen2 | DeviceModel::Mv6 => {
                protocol::cmd_set_mv6_mix(self.next_seq(), mix)
            }
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

    /// Send a factory reset to the MV7+.
    ///
    /// The device disconnects and re-enumerates immediately; no CONFIRM is sent.
    /// After this call succeeds the device handle is stale — do not use it again.
    pub fn factory_reset(&self) -> Result<()> {
        self.write(&cmd_factory_reset(self.next_seq()))
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

/// Enumerate supported Shure devices, one entry per physical device.
///
/// Two-pass approach:
/// 1. Filter to VID/PID candidates.
/// 2. If any candidate advertises a vendor-defined HID usage page (0xFF00–0xFFFF),
///    restrict to those — this drops the Windows telephony/mute-button collection
///    (usage page 0x000B) that appears as a phantom second entry on Windows.
///    Linux backends sometimes report usage_page=0 (descriptor not parsed); in
///    that case no candidate looks vendor-defined, so all are kept and step 3
///    handles deduplication.
/// 3. Deduplicate by path — on Linux all collections share one /dev/hidrawN path.
const VENDOR_USAGE_PAGE_MIN: u16 = 0xFF00;

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
            serial: d.serial_number().unwrap_or("(unknown)").to_owned(),
            model,
        })
        .collect()
}
