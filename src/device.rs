//! Device I/O: wraps hidapi for Shure USB microphones.
//!
//! Supports two devices:
//!   - Shure MVX2U (VID 0x14ED, PID 0x1013) — XLR-to-USB interface
//!   - Shure MV6   (VID 0x14ED, PID 0x1026) — USB gaming microphone
//!
//! Both devices expose a USB HID configuration interface alongside their
//! audio interface. hidapi opens the HID interface via /dev/hidrawN on Linux,
//! bypassing the audio driver entirely.
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

use anyhow::{Context, Result, anyhow};
use hidapi::{HidApi, HidDevice};

use crate::protocol::{
    self, DeviceModel, DeviceState, MV6_PID, PACKET_SIZE, PID, VID, apply_response, cmd_confirm,
    cmd_get_auto_gain, cmd_get_auto_position, cmd_get_auto_tone, cmd_get_compressor,
    cmd_get_eq_band_enable, cmd_get_eq_band_gain, cmd_get_eq_enable, cmd_get_gain, cmd_get_hpf,
    cmd_get_limiter, cmd_get_lock, cmd_get_mix, cmd_get_mode, cmd_get_mute, cmd_get_mv6_denoiser,
    cmd_get_mv6_gain_lock, cmd_get_mv6_popper_stopper, cmd_get_mv6_tone, cmd_get_phantom,
    cmd_set_lock, parse_response,
};

#[cfg(target_os = "linux")]
const ACCESS_HINT: &str = "ensure the udev rule is installed, or run with sudo";
#[cfg(not(target_os = "linux"))]
const ACCESS_HINT: &str = "ensure the device is plugged in and accessible";

/// How long to wait for a read response, in milliseconds.
const READ_TIMEOUT_MS: i32 = 200;

/// A connected Shure USB microphone (MVX2U or MV6).
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
    fn from_hid_device(device: HidDevice, pid: u16) -> Self {
        device.set_blocking_mode(false).ok();
        let serial_number = device
            .get_serial_number_string()
            .ok()
            .flatten()
            .unwrap_or_else(|| "(unknown)".to_string());
        let model = if pid == MV6_PID {
            DeviceModel::Mv6
        } else {
            DeviceModel::Mvx2u
        };
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

        // Deduplicate by path — some devices expose multiple HID interfaces
        // and hidapi enumerates each one separately under the same path.
        let mut seen_paths = std::collections::HashSet::new();
        let found: Vec<_> = api
            .device_list()
            .filter(|d| is_shure_device(d, &mut seen_paths))
            .collect();

        match found.len() {
            0 => Err(anyhow!(
                "No Shure MVX2U or MV6 device found.\nHint: {ACCESS_HINT}."
            )),
            1 => {
                let info = found[0];
                let pid = info.product_id();
                let c_path = std::ffi::CString::new(info.path().to_string_lossy().as_ref())
                    .map_err(|_| anyhow!("Device path contains a null byte"))?;
                let device = api
                    .open_path(c_path.as_c_str())
                    .map_err(|e| anyhow!("Cannot open device: {e}\nHint: {ACCESS_HINT}."))?;
                Ok(Self::from_hid_device(device, pid))
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
        if info.vendor_id() != VID || (pid != PID && pid != MV6_PID) {
            return Err(anyhow!(
                "{path} is not a supported Shure device \
                (VID={:#06x} PID={:#06x}); expected VID={:#06x} with PID={:#06x} or {:#06x}.",
                info.vendor_id(),
                pid,
                VID,
                PID,
                MV6_PID,
            ));
        }

        let c_path = std::ffi::CString::new(path)
            .map_err(|_| anyhow!("Device path contains a null byte: {path}"))?;
        let device = api
            .open_path(c_path.as_c_str())
            .map_err(|e| anyhow!("Cannot open {path}: {e}\nHint: {ACCESS_HINT}."))?;
        Ok(Self::from_hid_device(device, pid))
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

    fn send_set(&self, set_packet: &[u8]) -> Result<()> {
        self.write(set_packet)?;
        let confirm = cmd_confirm(self.next_seq());
        self.write(&confirm)
    }

    fn send_get(&self, get_packet: &[u8]) -> Result<Option<([u8; 2], Vec<u8>)>> {
        self.write(get_packet)?;
        let buf = self.read()?;
        Ok(parse_response(&buf))
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
            DeviceModel::Mv6 => self.get_state_mv6(),
        }
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
            let gain_pkt = cmd_get_eq_band_gain(self.next_seq(), band);
            if let Ok(Some((feat, value))) = self.send_get(&gain_pkt)
                && !apply_response(feat, &value, &mut state)
            {
                eprintln!(
                    "get_state: unrecognised feature {feat:#04x?} in EQ band {band} gain response"
                );
            }
        }

        Ok(state)
    }

    fn get_state_mv6(&self) -> Result<DeviceState> {
        let mut state = DeviceState::default();

        // MV6 shares gain, mute, HPF, and auto level addresses with the MVX2U.
        // mute_btn_disabled is persisted host-side (see presets::Mv6State) because
        // the device always returns the same value for that GET regardless of state.
        let getters: &[fn(u8) -> Vec<u8>] = &[
            cmd_get_gain,
            cmd_get_mute,
            cmd_get_hpf,
            cmd_get_mode,
            cmd_get_mv6_denoiser,
            cmd_get_mv6_popper_stopper,
            cmd_get_mv6_tone,
            cmd_get_mv6_gain_lock,
        ];

        self.run_getters(getters, &mut state, "get_state(mv6)");

        Ok(state)
    }

    // ── Shared SET commands ───────────────────────────────────────────────────

    /// Set manual gain. Clamped to the model's maximum (60 dB MVX2U, 36 dB MV6).
    pub fn set_gain(&self, gain_db: u8) -> Result<()> {
        let clamped = gain_db.min(self.model.max_gain_db());
        self.send_set(&protocol::cmd_set_gain(self.next_seq(), clamped))
    }

    pub fn set_mode(&self, auto: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mode(self.next_seq(), auto))
    }

    pub fn set_mute(&self, muted: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mute(self.next_seq(), muted))
    }

    pub fn set_hpf(&self, freq: &protocol::HpfFrequency) -> Result<()> {
        self.send_set(&protocol::cmd_set_hpf(self.next_seq(), freq))
    }

    // ── MVX2U-only SET commands ───────────────────────────────────────────────

    pub fn set_auto_position(&self, position: &protocol::MicPosition) -> Result<()> {
        self.send_set(&protocol::cmd_set_auto_position(self.next_seq(), position))
    }

    pub fn set_auto_tone(&self, tone: &protocol::AutoTone) -> Result<()> {
        self.send_set(&protocol::cmd_set_auto_tone(self.next_seq(), tone))
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
        self.send_set(&protocol::cmd_set_limiter(self.next_seq(), enabled))
    }

    pub fn set_compressor(&self, preset: &protocol::CompressorPreset) -> Result<()> {
        self.send_set(&protocol::cmd_set_compressor(self.next_seq(), preset))
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

    pub fn set_eq_band_gain(&self, band: usize, gain_db: i8) -> Result<()> {
        self.send_set(&protocol::cmd_set_eq_band_gain(
            self.next_seq(),
            band,
            gain_db,
        ))
    }

    pub fn set_lock(&self, locked: bool) -> Result<()> {
        self.send_set(&cmd_set_lock(self.next_seq(), locked))
    }

    // ── MV6-only SET commands ─────────────────────────────────────────────────

    pub fn set_mv6_denoiser(&self, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv6_denoiser(self.next_seq(), enabled))
    }

    pub fn set_mv6_popper_stopper(&self, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv6_popper_stopper(
            self.next_seq(),
            enabled,
        ))
    }

    pub fn set_mv6_mute_btn_disable(&self, disabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv6_mute_btn_disable(
            self.next_seq(),
            disabled,
        ))
    }

    pub fn set_mv6_tone(&self, tone: i8) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv6_tone(self.next_seq(), tone))
    }

    pub fn set_mv6_gain_lock(&self, locked: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv6_gain_lock(self.next_seq(), locked))
    }
}

/// Information about a detected Shure device, returned by [`list_devices`].
pub struct DeviceInfo {
    pub path: String,
    pub serial: String,
    pub model: DeviceModel,
}

/// Returns `true` if `d` is a supported Shure device that has not been seen before.
///
/// `seen_paths` is used to deduplicate entries — hidapi may enumerate the same
/// physical device multiple times when it exposes more than one HID interface.
fn is_shure_device(
    d: &hidapi::DeviceInfo,
    seen_paths: &mut std::collections::HashSet<String>,
) -> bool {
    d.vendor_id() == VID
        && (d.product_id() == PID || d.product_id() == MV6_PID)
        && seen_paths.insert(d.path().to_string_lossy().into_owned())
}

/// Probe the system for supported Shure devices without opening them.
pub fn list_devices() -> Vec<DeviceInfo> {
    let Ok(api) = HidApi::new() else {
        return vec![];
    };
    let mut seen_paths = std::collections::HashSet::new();
    api.device_list()
        .filter(|d| is_shure_device(d, &mut seen_paths))
        .map(|d| DeviceInfo {
            path: d.path().to_string_lossy().into_owned(),
            serial: d.serial_number().unwrap_or("(unknown)").to_owned(),
            model: if d.product_id() == MV6_PID {
                DeviceModel::Mv6
            } else {
                DeviceModel::Mvx2u
            },
        })
        .collect()
}
