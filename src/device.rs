//! Device I/O: wraps hidapi for the MVX2U.
//!
//! The MVX2U exposes three USB interfaces:
//!   Interface 0 – USB Audio (class 0x01, handled by the kernel's snd-usb-audio)
//!   Interface 1 – USB Audio streaming
//!   Interface 2 – HID configuration (the one we need)
//!
//! hidapi on Linux will open the HID interface by matching VID/PID.
//! Because snd-usb-audio also claims the device, hidapi uses /dev/hidrawN
//! which bypasses the audio driver entirely — no kernel detach needed.
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
//! device echoes this number in its response. We track it in `Mvx2u` and
//! increment after every `write()`.

use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::{Context, Result, anyhow};
use hidapi::{HidApi, HidDevice};

use crate::protocol::{
    self, DeviceState, PACKET_SIZE, PID, VID, apply_response, cmd_confirm, cmd_get_auto_gain,
    cmd_get_auto_position, cmd_get_auto_tone, cmd_get_compressor, cmd_get_eq_band_enable,
    cmd_get_eq_band_gain, cmd_get_eq_enable, cmd_get_gain, cmd_get_hpf, cmd_get_limiter,
    cmd_get_lock, cmd_get_mix, cmd_get_mode, cmd_get_mute, cmd_get_phantom, cmd_set_lock,
    parse_response,
};

/// How long to wait for a read response, in milliseconds.
const READ_TIMEOUT_MS: i32 = 200;

pub struct Mvx2u {
    device: HidDevice,
    /// Packet sequence number. Increments after every write; wraps at 256.
    seq: AtomicU8,
    /// Serial number string read from the USB device descriptor at open time.
    pub serial_number: String,
}

impl Mvx2u {
    /// Wrap an already-opened `HidDevice` into `Mvx2u`, reading the serial
    /// number from the USB device descriptor.
    fn from_hid_device(device: HidDevice) -> Self {
        device.set_blocking_mode(false).ok();
        // Serial number is read from the USB string descriptor at open time —
        // no HID packet exchange required.
        let serial_number = device
            .get_serial_number_string()
            .ok()
            .flatten()
            .unwrap_or_else(|| "(unknown)".to_string());
        Self {
            device,
            seq: AtomicU8::new(0),
            serial_number,
        }
    }

    /// Open the first MVX2U found on the system.
    pub fn open() -> Result<Self> {
        let api = HidApi::new().context("Failed to initialise hidapi")?;
        let device = api.open(VID, PID).map_err(|e| {
            anyhow!(
                "Cannot open MVX2U (VID={:#06x} PID={:#06x}): {e}\n\
                Hint: ensure the udev rule is installed or run with sudo.",
                VID,
                PID
            )
        })?;
        Ok(Self::from_hid_device(device))
    }

    /// Open the MVX2U at a specific hidraw path (e.g. `/dev/hidraw3`).
    ///
    /// Returns an error if the path is not found in the device list or does
    /// not identify a Shure MVX2U (VID/PID mismatch).
    pub fn open_path(path: &str) -> Result<Self> {
        let api = HidApi::new().context("Failed to initialise hidapi")?;
        // Validate the path against the enumerated device list before opening.
        // This catches both "no such hidraw node" and "wrong device type" with
        // clear error messages, rather than a generic hidapi open failure.
        let info = api
            .device_list()
            .find(|d| d.path().to_string_lossy() == path)
            .ok_or_else(|| {
                anyhow!("No device found at {path}. Use --list to see available devices.")
            })?;
        if info.vendor_id() != VID || info.product_id() != PID {
            return Err(anyhow!(
                "{path} is not a Shure MVX2U (VID={:#06x} PID={:#06x}); \
                expected VID={:#06x} PID={:#06x}.",
                info.vendor_id(),
                info.product_id(),
                VID,
                PID
            ));
        }
        // Open by path. We validated above that this path exists and matches
        // VID/PID, so any remaining error here is a permissions problem.
        let c_path = std::ffi::CString::new(path)
            .map_err(|_| anyhow!("Device path contains a null byte: {path}"))?;
        let device = api.open_path(c_path.as_c_str()).map_err(|e| {
            anyhow!(
                "Cannot open {path}: {e}\nHint: ensure the udev rule is installed or run with sudo."
            )
        })?;
        Ok(Self::from_hid_device(device))
    }

    /// Return the next sequence number and post-increment it (wraps at 256).
    fn next_seq(&self) -> u8 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Write one packet to the device.
    fn write(&self, packet: &[u8]) -> Result<()> {
        let written = self.device.write(packet).context("HID write failed")?;
        if written == 0 {
            return Err(anyhow!("HID write returned 0 bytes"));
        }
        Ok(())
    }

    /// Read one HID report, blocking up to `READ_TIMEOUT_MS`.
    ///
    /// Returns the raw bytes on success, or `Err` if the read failed (which
    /// typically means the device was disconnected).
    fn read(&self) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; PACKET_SIZE];
        match self.device.read_timeout(&mut buf, READ_TIMEOUT_MS) {
            Ok(0) => Err(anyhow!("HID read timed out — no response from device")),
            Ok(n) => Ok(buf[..n].to_vec()),
            Err(e) => Err(anyhow!("HID read failed (device disconnected?): {e}")),
        }
    }

    /// Send a SET command followed by a CONFIRM packet.
    ///
    /// Every SET on the MVX2U must be confirmed; the device will not apply the
    /// change without the subsequent CONFIRM.
    fn send_set(&self, set_packet: &[u8]) -> Result<()> {
        self.write(set_packet)?;
        let confirm = cmd_confirm(self.next_seq());
        self.write(&confirm)
    }

    /// Send a GET command and read back the response.
    ///
    /// Returns `Ok(Some((feat_addr, value)))` on a valid response, or
    /// `Ok(None)` if the response was a CONFIRM or failed to parse.
    fn send_get(&self, get_packet: &[u8]) -> Result<Option<([u8; 2], Vec<u8>)>> {
        self.write(get_packet)?;
        let buf = self.read()?;
        Ok(parse_response(&buf))
    }

    /// Fetch the complete device state by querying every feature individually.
    ///
    /// Returns a partially-filled `DeviceState` if some queries time out;
    /// a hard `Err` is returned only if the transport itself fails.
    pub fn get_state(&self) -> Result<DeviceState> {
        let mut state = DeviceState::default();

        // Query lock state first — it is on a separate command class.
        let lock_pkt = cmd_get_lock(self.next_seq());
        if let Ok(Some((feat, value))) = self.send_get(&lock_pkt)
            && !apply_response(feat, &value, &mut state)
        {
            eprintln!("get_state: unrecognised feature {feat:#04x?} in lock response");
        }

        // Query every feature in order. Errors on individual reads are non-fatal
        // (the default value stays in place); transport errors propagate.
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

        for getter in getters {
            let pkt = getter(self.next_seq());
            if let Ok(Some((feat, value))) = self.send_get(&pkt)
                && !apply_response(feat, &value, &mut state)
            {
                eprintln!("get_state: unrecognised feature {feat:#04x?} in response");
            }
        }

        // EQ bands: enable + gain for each of the 5 bands
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

    // ── SET commands ──────────────────────────────────────────────────────────

    /// Set manual gain (0–60 dB).
    pub fn set_gain(&self, gain_db: u8) -> Result<()> {
        self.send_set(&protocol::cmd_set_gain(self.next_seq(), gain_db))
    }

    /// Set input mode. `auto = true` selects Auto Level; `false` selects Manual.
    pub fn set_mode(&self, auto: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mode(self.next_seq(), auto))
    }

    /// Set mic position for Auto Level mode.
    pub fn set_auto_position(&self, position: &protocol::MicPosition) -> Result<()> {
        self.send_set(&protocol::cmd_set_auto_position(self.next_seq(), position))
    }

    /// Set tone preset for Auto Level mode.
    pub fn set_auto_tone(&self, tone: &protocol::AutoTone) -> Result<()> {
        self.send_set(&protocol::cmd_set_auto_tone(self.next_seq(), tone))
    }

    /// Set gain preset for Auto Level mode.
    pub fn set_auto_gain(&self, gain: &protocol::AutoGain) -> Result<()> {
        self.send_set(&protocol::cmd_set_auto_gain(self.next_seq(), gain))
    }

    /// Mute or unmute the microphone input.
    pub fn set_mute(&self, muted: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mute(self.next_seq(), muted))
    }

    /// Enable or disable 48 V phantom power.
    pub fn set_phantom(&self, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_phantom(self.next_seq(), enabled))
    }

    /// Set the direct-monitor mix (0–100). 0 = 100% mic; 100 = 100% playback.
    pub fn set_monitor_mix(&self, mix: u8) -> Result<()> {
        self.send_set(&protocol::cmd_set_mix(self.next_seq(), mix))
    }

    /// Enable or disable the output limiter.
    pub fn set_limiter(&self, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_limiter(self.next_seq(), enabled))
    }

    /// Set the compressor preset.
    pub fn set_compressor(&self, preset: &protocol::CompressorPreset) -> Result<()> {
        self.send_set(&protocol::cmd_set_compressor(self.next_seq(), preset))
    }

    /// Set the high-pass filter frequency.
    pub fn set_hpf(&self, freq: &protocol::HpfFrequency) -> Result<()> {
        self.send_set(&protocol::cmd_set_hpf(self.next_seq(), freq))
    }

    /// Enable or disable the 5-band parametric EQ.
    pub fn set_eq_enable(&self, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_eq_enable(self.next_seq(), enabled))
    }

    /// Set an EQ band's enabled state. `band` is 0–4.
    pub fn set_eq_band_enable(&self, band: usize, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_eq_band_enable(
            self.next_seq(),
            band,
            enabled,
        ))
    }

    /// Set an EQ band's gain (−8 to +6 dB, steps of 2). `band` is 0–4.
    pub fn set_eq_band_gain(&self, band: usize, gain_db: i8) -> Result<()> {
        self.send_set(&protocol::cmd_set_eq_band_gain(
            self.next_seq(),
            band,
            gain_db,
        ))
    }

    // ── Lock ──────────────────────────────────────────────────────────────────

    /// Lock or unlock the device configuration.
    /// When locked, the device ignores all SET commands until unlocked.
    pub fn set_lock(&self, locked: bool) -> Result<()> {
        self.send_set(&cmd_set_lock(self.next_seq(), locked))
    }
}

/// Information about a detected MVX2U, returned by [`list_devices`].
pub struct DeviceInfo {
    /// Path to the hidraw node, e.g. `/dev/hidraw3`.
    pub path: String,
    /// USB serial number string, e.g. `MVX2U#3-7d84d19...`.
    pub serial: String,
}

/// Probe the system for MVX2U devices without opening them.
pub fn list_devices() -> Vec<DeviceInfo> {
    let Ok(api) = HidApi::new() else {
        return vec![];
    };
    api.device_list()
        .filter(|d| d.vendor_id() == VID && d.product_id() == PID)
        .map(|d| DeviceInfo {
            path: d.path().to_string_lossy().into_owned(),
            serial: d.serial_number().unwrap_or("(unknown)").to_owned(),
        })
        .collect()
}
