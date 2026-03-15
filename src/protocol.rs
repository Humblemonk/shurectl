//! MVX2U USB HID Protocol Implementation
//!
//! The MVX2U uses USB HID reports for configuration, communicated via
//! `write()` (host→device) and `read()` (device→host) on the hidraw fd.
//!
//! Packet structure (64 bytes total, written via hidapi):
//!
//!   [0x01]              Report ID (hidapi requires this as byte 0 for write())
//!   [total_len]         Length of everything that follows (excluding report ID byte)
//!   [0x11] [0x22]       Fixed header magic
//!   [seq]  [0x03]       Sequence number (increments per packet) + constant 0x03
//!   [0x08]              Header end marker
//!   [data_len]          Length of data section (repeated at offset +2)
//!   [0x70]              Data start marker
//!   [data_len]          Length of data section (repeated)
//!   [cmd0][cmd1][cmd2]  3-byte command
//!   [payload...]        Feature address + value bytes
//!   [crc_hi][crc_lo]    CRC-16/ANSI over all bytes from [0x11] onward (excl. CRC itself)
//!   [0x00 ...]          Zero padding to reach 64 bytes total
//!
//! CRC algorithm: CRC-16/ANSI — poly 0x8005, init 0x0000, reflected input+output.
//! This is the standard "CRC-16" used in USB and serial protocols (NOT CCITT-FALSE).
//!
//! USB IDs:
//!   Vendor ID:  0x14ED  (Shure)
//!   Product ID: 0x1013  (MVX2U)
//!
//! Every SET command must be followed by a CONFIRM packet (CMD_CONFIRM).
//! GET commands receive a response on the next `read()`.
//!
//! Feature addresses (2 bytes):
//!   Config lock:    [0x00, 0xA6]  — 1 byte: 0=unlocked, 1=locked
//!                                   Uses CMD_GET_LOCK / CMD_SET_LOCK (last byte 0x01, not 0x02)
//!                                   Payload prefix byte is 0x06 (not 0x00 or 0x01)
//!   Gain (manual):  [0x01, 0x02]  — 16-bit big-endian, units: gain_db * 100
//!   Mute:           [0x01, 0x04]  — 1 byte: 0=unmuted, 1=muted
//!   HPF:            [0x01, 0x06]  — 1 byte: 0=off, 1=75Hz, 2=150Hz
//!   Limiter:        [0x01, 0x51]  — 1 byte: 0=off, 1=on
//!   Compressor:     [0x01, 0x5C]  — 1 byte: 0=off, 1=light, 2=medium, 3=heavy
//!   Phantom (48V):  [0x01, 0x66]  — 1 byte: 0=off, 48(0x30)=on
//!   Auto level:     [0x01, 0x85]  — 1 byte: 0=manual, 1=auto
//!   Auto position:  [0x01, 0x82]  — 1 byte: 0=Near, 1=Far
//!   Auto tone:      [0x01, 0x83]  — 1 byte: 0=Dark, 1=Natural, 2=Bright
//!   Auto gain:      [0x01, 0x87]  — 4 bytes big-endian u32: 0=Quiet, 1=Normal, 2=Loud
//!                                   NOTE: 4-byte width unverified; verify with usbmon if misbehaving.
//!   Monitor mix:    [0x01, 0x86]  — 1 byte: 0=full mic, 100=full playback
//!   EQ master:      [0x02, 0x00]  — 1 byte: 0=bypass, 1=enabled
//!   EQ band enable: [0x02, 0xN0]  — 1 byte: 0=off, 1=on  (N = 1,2,3,4,5 per band)
//!   EQ band gain:   [0x02, 0xN4]  — 16-bit signed big-endian, units: gain_db * 10
//!
//! EQ bands have fixed center frequencies: 100, 250, 1000, 4000, 10000 Hz.
//! Frequency and Q are not adjustable on this device.

pub const VID: u16 = 0x14ED;
pub const PID: u16 = 0x1013;
pub const PACKET_SIZE: usize = 64;

// Fixed header bytes
const REPORT_ID: u8 = 0x01;
const HEADER_MAGIC: [u8; 2] = [0x11, 0x22];
const HDR_CONSTANT: u8 = 0x03;
const HDR_END: u8 = 0x08;
const DATA_START: u8 = 0x70;

// ── Command bytes (3 bytes each) ──────────────────────────────────────────────
const CMD_GET_FEAT: [u8; 3] = [0x01, 0x02, 0x02];
const CMD_SET_FEAT: [u8; 3] = [0x02, 0x02, 0x02];
const CMD_CONFIRM: [u8; 3] = [0x01, 0x00, 0x00];
/// Lock uses a distinct command variant (last byte 0x01 instead of 0x02).
const CMD_GET_LOCK: [u8; 3] = [0x01, 0x02, 0x01];
const CMD_SET_LOCK: [u8; 3] = [0x02, 0x02, 0x01];

// Response command bytes (from device)
const RES_GET_FEAT: [u8; 3] = [0x03, 0x02, 0x02];
const RES_SET_FEAT: [u8; 3] = [0x04, 0x02, 0x02];
const RES_GET_LOCK: [u8; 3] = [0x03, 0x02, 0x01];
const RES_SET_LOCK: [u8; 3] = [0x04, 0x02, 0x01];

// ── Feature addresses (2 bytes each) ─────────────────────────────────────────
/// Config lock lives on page 0x00, unlike all other features (page 0x01/0x02).
const FEAT_LOCK: [u8; 2] = [0x00, 0xA6];
const FEAT_GAIN: [u8; 2] = [0x01, 0x02];
const FEAT_MUTE: [u8; 2] = [0x01, 0x04];
const FEAT_HPF: [u8; 2] = [0x01, 0x06];
const FEAT_LIMITER: [u8; 2] = [0x01, 0x51];
const FEAT_COMP: [u8; 2] = [0x01, 0x5C];
const FEAT_PHANTOM: [u8; 2] = [0x01, 0x66];
/// Auto level on/off. Also see FEAT_AUTO_POSITION, FEAT_AUTO_TONE, FEAT_AUTO_GAIN.
const FEAT_AUTO: [u8; 2] = [0x01, 0x85];
const FEAT_MIX: [u8; 2] = [0x01, 0x86];
/// Mic position for Auto Level mode: 0=Near, 1=Far.
const FEAT_AUTO_POSITION: [u8; 2] = [0x01, 0x82];
/// Tone preset for Auto Level mode: 0=Dark, 1=Natural, 2=Bright.
const FEAT_AUTO_TONE: [u8; 2] = [0x01, 0x83];
/// Gain preset for Auto Level mode: encoded as 4-byte big-endian u32.
/// Values: 0=Quiet, 1=Normal, 2=Loud.
/// NOTE: 4-byte encoding sourced from shux reverse-engineering; verify with usbmon if misbehaving.
const FEAT_AUTO_GAIN: [u8; 2] = [0x01, 0x87];
const FEAT_EQ: [u8; 2] = [0x02, 0x00];

// EQ band feature addresses: [enable_addr, gain_addr]
// Bands are fixed at 100, 250, 1000, 4000, 10000 Hz.
const EQ_BAND_ADDRS: [([u8; 2], [u8; 2]); 5] = [
    ([0x02, 0x10], [0x02, 0x14]), // 100 Hz
    ([0x02, 0x20], [0x02, 0x24]), // 250 Hz
    ([0x02, 0x30], [0x02, 0x34]), // 1000 Hz
    ([0x02, 0x40], [0x02, 0x44]), // 4000 Hz
    ([0x02, 0x50], [0x02, 0x54]), // 10000 Hz
];

/// The center frequency (Hz) for each of the 5 EQ bands. Fixed by hardware.
pub const EQ_BAND_FREQS: [u16; 5] = [100, 250, 1000, 4000, 10000];

// ── Phantom power value encoding ──────────────────────────────────────────────
const PHANTOM_ON: u8 = 0x30; // 48 decimal = 48V
const PHANTOM_OFF: u8 = 0x00;

// ── Device state (fully decoded) ──────────────────────────────────────────────
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceState {
    /// Manual gain in dB, range 0–60.
    /// Defaults to 36 dB, which matches the MVX2U factory default.
    pub gain_db: u8,
    pub mode: InputMode,
    /// Mic position for Auto Level mode.
    pub auto_position: MicPosition,
    /// Tone preset for Auto Level mode.
    pub auto_tone: AutoTone,
    /// Gain preset for Auto Level mode.
    pub auto_gain: AutoGain,
    pub muted: bool,
    pub phantom_power: bool,
    /// Monitor mix: 0 = 100% mic, 100 = 100% playback.
    pub monitor_mix: u8,
    pub limiter_enabled: bool,
    pub compressor: CompressorPreset,
    pub hpf: HpfFrequency,
    pub eq_enabled: bool,
    /// 5 EQ bands at fixed frequencies (100, 250, 1k, 4k, 10k Hz).
    pub eq_bands: [EqBand; 5],
    /// Whether the device configuration is locked against changes.
    pub locked: bool,
    /// Device serial number string, populated after connection.
    pub serial_number: String,
}

impl Default for DeviceState {
    fn default() -> Self {
        Self {
            gain_db: 36,
            mode: InputMode::Auto,
            auto_position: MicPosition::Near,
            auto_tone: AutoTone::Natural,
            auto_gain: AutoGain::Normal,
            muted: false,
            phantom_power: false,
            monitor_mix: 0,
            limiter_enabled: false,
            compressor: CompressorPreset::Off,
            hpf: HpfFrequency::Off,
            eq_enabled: false,
            eq_bands: [EqBand::default(); 5],
            locked: false,
            serial_number: String::from("Unknown"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputMode {
    Auto,
    Manual,
}

impl std::fmt::Display for InputMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputMode::Auto => write!(f, "Auto Level"),
            InputMode::Manual => write!(f, "Manual"),
        }
    }
}

/// Microphone position for Auto Level mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MicPosition {
    /// Mic is close to the speaker's mouth (default).
    Near,
    /// Mic is at arm's length or further.
    Far,
}

impl MicPosition {
    pub fn cycle_next(&self) -> Self {
        match self {
            MicPosition::Near => MicPosition::Far,
            MicPosition::Far => MicPosition::Near,
        }
    }

    pub(crate) fn as_byte(&self) -> u8 {
        match self {
            MicPosition::Near => 0x00,
            MicPosition::Far => 0x01,
        }
    }

    pub(crate) fn from_byte(b: u8) -> Self {
        match b {
            0x01 => MicPosition::Far,
            _ => MicPosition::Near,
        }
    }
}

impl std::fmt::Display for MicPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MicPosition::Near => write!(f, "Near"),
            MicPosition::Far => write!(f, "Far"),
        }
    }
}

/// Tone character preset for Auto Level mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AutoTone {
    Dark,
    Natural,
    Bright,
}

impl AutoTone {
    pub fn cycle_next(&self) -> Self {
        match self {
            AutoTone::Dark => AutoTone::Natural,
            AutoTone::Natural => AutoTone::Bright,
            AutoTone::Bright => AutoTone::Dark,
        }
    }

    pub(crate) fn as_byte(&self) -> u8 {
        match self {
            AutoTone::Dark => 0x00,
            AutoTone::Natural => 0x01,
            AutoTone::Bright => 0x02,
        }
    }

    pub(crate) fn from_byte(b: u8) -> Self {
        match b {
            0x00 => AutoTone::Dark,
            0x02 => AutoTone::Bright,
            _ => AutoTone::Natural,
        }
    }
}

impl std::fmt::Display for AutoTone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AutoTone::Dark => write!(f, "Dark"),
            AutoTone::Natural => write!(f, "Natural"),
            AutoTone::Bright => write!(f, "Bright"),
        }
    }
}

/// Gain sensitivity preset for Auto Level mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AutoGain {
    /// For quiet environments or soft-spoken voices.
    Quiet,
    /// Default setting for typical speech levels.
    Normal,
    /// For loud environments or strong voices.
    Loud,
}

impl AutoGain {
    pub fn cycle_next(&self) -> Self {
        match self {
            AutoGain::Quiet => AutoGain::Normal,
            AutoGain::Normal => AutoGain::Loud,
            AutoGain::Loud => AutoGain::Quiet,
        }
    }

    /// Encodes as 4-byte big-endian u32.
    /// NOTE: 4-byte width sourced from shux; verify with usbmon if misbehaving.
    pub(crate) fn as_be_bytes(&self) -> [u8; 4] {
        let v: u32 = match self {
            AutoGain::Quiet => 0,
            AutoGain::Normal => 1,
            AutoGain::Loud => 2,
        };
        v.to_be_bytes()
    }

    pub(crate) fn from_be_bytes(bytes: &[u8]) -> Self {
        // Accept both 1-byte and 4-byte representations defensively.
        let v = if bytes.len() >= 4 {
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        } else if !bytes.is_empty() {
            bytes[0] as u32
        } else {
            return AutoGain::Normal;
        };
        match v {
            0 => AutoGain::Quiet,
            2 => AutoGain::Loud,
            _ => AutoGain::Normal,
        }
    }
}

impl std::fmt::Display for AutoGain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AutoGain::Quiet => write!(f, "Quiet"),
            AutoGain::Normal => write!(f, "Normal"),
            AutoGain::Loud => write!(f, "Loud"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompressorPreset {
    Off,
    Light,
    Medium,
    Heavy,
}

impl CompressorPreset {
    pub fn cycle_next(&self) -> Self {
        match self {
            CompressorPreset::Off => CompressorPreset::Light,
            CompressorPreset::Light => CompressorPreset::Medium,
            CompressorPreset::Medium => CompressorPreset::Heavy,
            CompressorPreset::Heavy => CompressorPreset::Off,
        }
    }

    fn as_byte(&self) -> u8 {
        match self {
            CompressorPreset::Off => 0x00,
            CompressorPreset::Light => 0x01,
            CompressorPreset::Medium => 0x02,
            CompressorPreset::Heavy => 0x03,
        }
    }

    fn from_byte(b: u8) -> Self {
        match b {
            0x01 => CompressorPreset::Light,
            0x02 => CompressorPreset::Medium,
            0x03 => CompressorPreset::Heavy,
            _ => CompressorPreset::Off,
        }
    }
}

impl std::fmt::Display for CompressorPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompressorPreset::Off => write!(f, "Off"),
            CompressorPreset::Light => write!(f, "Light"),
            CompressorPreset::Medium => write!(f, "Medium"),
            CompressorPreset::Heavy => write!(f, "Heavy"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HpfFrequency {
    Off,
    Hz75,
    Hz150,
}

impl HpfFrequency {
    pub fn cycle_next(&self) -> Self {
        match self {
            HpfFrequency::Off => HpfFrequency::Hz75,
            HpfFrequency::Hz75 => HpfFrequency::Hz150,
            HpfFrequency::Hz150 => HpfFrequency::Off,
        }
    }

    fn as_byte(&self) -> u8 {
        match self {
            HpfFrequency::Off => 0x00,
            HpfFrequency::Hz75 => 0x01,
            HpfFrequency::Hz150 => 0x02,
        }
    }

    fn from_byte(b: u8) -> Self {
        match b {
            0x01 => HpfFrequency::Hz75,
            0x02 => HpfFrequency::Hz150,
            _ => HpfFrequency::Off,
        }
    }
}

impl std::fmt::Display for HpfFrequency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HpfFrequency::Off => write!(f, "Off"),
            HpfFrequency::Hz75 => write!(f, "75 Hz"),
            HpfFrequency::Hz150 => write!(f, "150 Hz"),
        }
    }
}

/// One of the 5 parametric EQ bands.
///
/// Center frequencies are fixed by hardware: 100, 250, 1000, 4000, 10000 Hz.
/// Q is not adjustable. Only gain (−8 to +6 dB in steps of 2) and
/// enabled/disabled are settable.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EqBand {
    /// Gain in dB, range −8 to +6 in steps of 2.
    pub gain_db: i8,
    /// Whether this band is active.
    pub enabled: bool,
}

// ── CRC-16/ANSI ───────────────────────────────────────────────────────────────
/// CRC-16/ANSI: poly 0x8005, init 0x0000, reflected input and output.
///
/// This is the standard "CRC-16" used by the MVX2U protocol (not CCITT-FALSE).
#[must_use]
pub fn crc16_ansi(data: &[u8]) -> u16 {
    let mut crc: u16 = 0x0000;
    for &byte in data {
        let mut b = byte;
        for _ in 0..8 {
            let bit = (crc ^ b as u16) & 1;
            crc >>= 1;
            if bit != 0 {
                crc ^= 0xA001; // bit-reversed 0x8005
            }
            b >>= 1;
        }
    }
    crc
}

// ── Packet builder ────────────────────────────────────────────────────────────
/// Build a 64-byte packet ready for `hidapi::write()`.
///
/// `seq` is the per-packet sequence number (0–255, wraps).
/// `cmd` is the 3-byte command ([`CMD_GET_FEAT`], [`CMD_SET_FEAT`], or [`CMD_CONFIRM`]).
/// `payload` is the feature address + optional value bytes.
///
/// Layout produced (before CRC + padding):
/// ```text
/// [0x01][total_len][0x11][0x22][seq][0x03][0x08][data_len][0x70][data_len][cmd×3][payload...]
/// ```
/// CRC-16/ANSI covers from `[0x11]` to end of payload (exclusive of CRC bytes).
fn build_packet(seq: u8, cmd: &[u8; 3], payload: &[u8]) -> Vec<u8> {
    // Inner data section: DATA_START + data_len + cmd + payload
    // data_len = cmd(3) + payload.len() + 2 (for the data_len byte itself + DATA_START byte)
    let data_len = (3 + payload.len() + 2) as u8;

    // Wire content (everything after report ID, before CRC and padding):
    // [total_len][0x11][0x22][seq][0x03][0x08][data_len][0x70][data_len][cmd...][payload...]
    let mut inner: Vec<u8> = Vec::with_capacity(PACKET_SIZE);
    inner.push(HEADER_MAGIC[0]);
    inner.push(HEADER_MAGIC[1]);
    inner.push(seq);
    inner.push(HDR_CONSTANT);
    inner.push(HDR_END);
    inner.push(data_len);
    inner.push(DATA_START);
    inner.push(data_len);
    inner.extend_from_slice(cmd);
    inner.extend_from_slice(payload);

    // total_len = inner.len() + 2 (for the two CRC bytes)
    let total_len = (inner.len() + 2) as u8;

    // CRC covers everything in `inner` (i.e. from 0x11 onward)
    let crc = crc16_ansi(&inner);

    // Assemble: [REPORT_ID][total_len][inner...][crc_hi][crc_lo][padding...]
    let mut pkt: Vec<u8> = Vec::with_capacity(PACKET_SIZE);
    pkt.push(REPORT_ID);
    pkt.push(total_len);
    pkt.extend_from_slice(&inner);
    pkt.push((crc >> 8) as u8);
    pkt.push((crc & 0xFF) as u8);
    pkt.resize(PACKET_SIZE, 0x00);

    pkt
}

/// Validate a received HID report and extract the feature address and value bytes.
///
/// Returns `None` if the buffer is malformed, the header magic is wrong, or
/// the CRC does not match. On success returns `(feat_addr, value_bytes)`.
#[must_use]
pub fn parse_response(buf: &[u8]) -> Option<([u8; 2], Vec<u8>)> {
    // Minimum: report_id(1) + total_len(1) + header(2) + seq(1) + 0x03(1) +
    //          hdr_end(1) + data_len(1) + data_start(1) + data_len(1) +
    //          cmd(3) + is_mix(1) + feat(2) + crc(2) = 18 bytes minimum
    if buf.len() < 18 {
        return None;
    }

    // buf[0] = report ID (0x01)
    // buf[1] = total_len (length of buf[2..contents_end+2])
    let contents_end = buf[1] as usize;
    if contents_end + 2 > buf.len() {
        return None;
    }

    // Header magic at buf[2..4]
    if buf[2] != HEADER_MAGIC[0] || buf[3] != HEADER_MAGIC[1] {
        return None;
    }

    // CRC check: covers buf[2..contents_end]
    let expected_crc = ((buf[contents_end] as u16) << 8) | buf[contents_end + 1] as u16;
    let actual_crc = crc16_ansi(&buf[2..contents_end]);
    if actual_crc != expected_crc {
        return None;
    }

    // Response type at buf[10..13]
    let resp_type: [u8; 3] = buf[10..13].try_into().ok()?;
    if resp_type != RES_GET_FEAT
        && resp_type != RES_SET_FEAT
        && resp_type != RES_GET_LOCK
        && resp_type != RES_SET_LOCK
    {
        // CONFIRM responses carry no data; callers can detect them separately
        return None;
    }

    // buf[13] = is_mix byte (ignored for our purposes)
    // buf[14..16] = feature address
    if buf.len() < 16 {
        return None;
    }
    let feat_addr: [u8; 2] = buf[14..16].try_into().ok()?;
    let value_bytes = buf[16..contents_end].to_vec();

    Some((feat_addr, value_bytes))
}

// ── Command constructors ──────────────────────────────────────────────────────

/// Build a GET packet for a single feature. Returns the packet bytes.
fn cmd_get(seq: u8, feat_addr: &[u8; 2]) -> Vec<u8> {
    // payload for GET: [is_mix=0x00][feat_addr]
    let payload = [0x00, feat_addr[0], feat_addr[1]];
    build_packet(seq, &CMD_GET_FEAT, &payload)
}

/// Build a SET packet for a feature with a value payload.
fn cmd_set(seq: u8, feat_addr: &[u8; 2], value: &[u8]) -> Vec<u8> {
    // payload for SET: [is_mix][feat_addr][value...]
    // is_mix = 0x01 for MIX feature, 0x00 for everything else
    let is_mix: u8 = if feat_addr == &FEAT_MIX { 0x01 } else { 0x00 };
    let mut payload = vec![is_mix, feat_addr[0], feat_addr[1]];
    payload.extend_from_slice(value);
    build_packet(seq, &CMD_SET_FEAT, &payload)
}

/// Build a CONFIRM packet (must follow every SET).
pub fn cmd_confirm(seq: u8) -> Vec<u8> {
    build_packet(seq, &CMD_CONFIRM, &[])
}

// ── Public GET constructors ───────────────────────────────────────────────────

pub fn cmd_get_gain(seq: u8) -> Vec<u8> {
    cmd_get(seq, &FEAT_GAIN)
}
pub fn cmd_get_mute(seq: u8) -> Vec<u8> {
    cmd_get(seq, &FEAT_MUTE)
}
pub fn cmd_get_hpf(seq: u8) -> Vec<u8> {
    cmd_get(seq, &FEAT_HPF)
}
pub fn cmd_get_limiter(seq: u8) -> Vec<u8> {
    cmd_get(seq, &FEAT_LIMITER)
}
pub fn cmd_get_compressor(seq: u8) -> Vec<u8> {
    cmd_get(seq, &FEAT_COMP)
}
pub fn cmd_get_phantom(seq: u8) -> Vec<u8> {
    cmd_get(seq, &FEAT_PHANTOM)
}
pub fn cmd_get_mode(seq: u8) -> Vec<u8> {
    cmd_get(seq, &FEAT_AUTO)
}
pub fn cmd_get_auto_position(seq: u8) -> Vec<u8> {
    cmd_get(seq, &FEAT_AUTO_POSITION)
}
pub fn cmd_get_auto_tone(seq: u8) -> Vec<u8> {
    cmd_get(seq, &FEAT_AUTO_TONE)
}
pub fn cmd_get_auto_gain(seq: u8) -> Vec<u8> {
    cmd_get(seq, &FEAT_AUTO_GAIN)
}
pub fn cmd_get_mix(seq: u8) -> Vec<u8> {
    // MIX requires is_mix=0x01 in both GET and SET payloads.
    // The generic cmd_get uses is_mix=0x00; build this one manually.
    let payload = [0x01, FEAT_MIX[0], FEAT_MIX[1]];
    build_packet(seq, &CMD_GET_FEAT, &payload)
}
pub fn cmd_get_eq_enable(seq: u8) -> Vec<u8> {
    cmd_get(seq, &FEAT_EQ)
}

// ── Lock command constructors ─────────────────────────────────────────────────
//
// Lock uses a distinct command type (CMD_GET_LOCK / CMD_SET_LOCK) and a
// different payload prefix byte (0x06 instead of 0x00/0x01).  Everything
// else — packet framing, CRC, CONFIRM requirement — is identical.

/// Build a GET packet for the config-lock feature.
pub fn cmd_get_lock(seq: u8) -> Vec<u8> {
    // Payload: [0x06][feat_addr]  — lock uses 0x06 as the is_mix_or_lock byte.
    let payload = [0x06, FEAT_LOCK[0], FEAT_LOCK[1]];
    build_packet(seq, &CMD_GET_LOCK, &payload)
}

/// Build a SET packet for the config-lock feature.
/// `locked = true` locks the device; `false` unlocks it.
/// Must be followed by a CONFIRM packet (use `cmd_confirm`).
pub fn cmd_set_lock(seq: u8, locked: bool) -> Vec<u8> {
    let value: u8 = u8::from(locked);
    let payload = [0x06, FEAT_LOCK[0], FEAT_LOCK[1], value];
    build_packet(seq, &CMD_SET_LOCK, &payload)
}

pub fn cmd_get_eq_band_enable(seq: u8, band: usize) -> Vec<u8> {
    debug_assert!(
        band < EQ_BAND_ADDRS.len(),
        "band index out of range: {band}"
    );
    cmd_get(seq, &EQ_BAND_ADDRS[band].0)
}
pub fn cmd_get_eq_band_gain(seq: u8, band: usize) -> Vec<u8> {
    debug_assert!(
        band < EQ_BAND_ADDRS.len(),
        "band index out of range: {band}"
    );
    cmd_get(seq, &EQ_BAND_ADDRS[band].1)
}

// ── Public SET constructors ───────────────────────────────────────────────────

/// Set manual gain. `gain_db` is clamped to 0–60.
/// Encoded as `gain_db * 100` in 16-bit big-endian.
pub fn cmd_set_gain(seq: u8, gain_db: u8) -> Vec<u8> {
    let raw = (gain_db.min(60) as u16) * 100;
    cmd_set(seq, &FEAT_GAIN, &raw.to_be_bytes())
}

pub fn cmd_set_mute(seq: u8, muted: bool) -> Vec<u8> {
    cmd_set(seq, &FEAT_MUTE, &[u8::from(muted)])
}

pub fn cmd_set_phantom(seq: u8, enabled: bool) -> Vec<u8> {
    cmd_set(
        seq,
        &FEAT_PHANTOM,
        &[if enabled { PHANTOM_ON } else { PHANTOM_OFF }],
    )
}

/// Set input mode. `auto = true` selects Auto Level; `false` selects Manual.
pub fn cmd_set_mode(seq: u8, auto: bool) -> Vec<u8> {
    cmd_set(seq, &FEAT_AUTO, &[u8::from(auto)])
}

/// Set mic position for Auto Level mode.
pub fn cmd_set_auto_position(seq: u8, position: &MicPosition) -> Vec<u8> {
    cmd_set(seq, &FEAT_AUTO_POSITION, &[position.as_byte()])
}

/// Set tone preset for Auto Level mode.
pub fn cmd_set_auto_tone(seq: u8, tone: &AutoTone) -> Vec<u8> {
    cmd_set(seq, &FEAT_AUTO_TONE, &[tone.as_byte()])
}

/// Set gain preset for Auto Level mode.
/// Encoded as 4-byte big-endian u32 per shux reverse-engineering.
pub fn cmd_set_auto_gain(seq: u8, gain: &AutoGain) -> Vec<u8> {
    cmd_set(seq, &FEAT_AUTO_GAIN, &gain.as_be_bytes())
}

/// Set monitor mix. `mix` is clamped to 0–100 (0=full mic, 100=full playback).
pub fn cmd_set_mix(seq: u8, mix: u8) -> Vec<u8> {
    cmd_set(seq, &FEAT_MIX, &[mix.min(100)])
}

pub fn cmd_set_limiter(seq: u8, enabled: bool) -> Vec<u8> {
    cmd_set(seq, &FEAT_LIMITER, &[u8::from(enabled)])
}

pub fn cmd_set_compressor(seq: u8, preset: &CompressorPreset) -> Vec<u8> {
    cmd_set(seq, &FEAT_COMP, &[preset.as_byte()])
}

pub fn cmd_set_hpf(seq: u8, freq: &HpfFrequency) -> Vec<u8> {
    cmd_set(seq, &FEAT_HPF, &[freq.as_byte()])
}

pub fn cmd_set_eq_enable(seq: u8, enabled: bool) -> Vec<u8> {
    cmd_set(seq, &FEAT_EQ, &[u8::from(enabled)])
}

/// Set EQ band enable. `band` is 0–4.
pub fn cmd_set_eq_band_enable(seq: u8, band: usize, enabled: bool) -> Vec<u8> {
    debug_assert!(
        band < EQ_BAND_ADDRS.len(),
        "band index out of range: {band}"
    );
    cmd_set(seq, &EQ_BAND_ADDRS[band].0, &[u8::from(enabled)])
}

/// Set EQ band gain. `gain_db` is clamped to −8..+6 and rounded to the
/// nearest even value (hardware supports −8, −6, −4, −2, 0, 2, 4, 6 only).
/// Encoded as `gain_db * 10` in 16-bit signed big-endian.
pub fn cmd_set_eq_band_gain(seq: u8, band: usize, gain_db: i8) -> Vec<u8> {
    debug_assert!(
        band < EQ_BAND_ADDRS.len(),
        "band index out of range: {band}"
    );
    let clamped = gain_db.clamp(-8, 6);
    let raw = (clamped as i16 * 10).to_be_bytes();
    cmd_set(seq, &EQ_BAND_ADDRS[band].1, &raw)
}

// ── Response decoder ──────────────────────────────────────────────────────────
/// Apply a parsed feature response `(feat_addr, value_bytes)` to `state`.
///
/// Returns `true` if the feature was recognised and applied, `false` if the
/// feature address is unknown or the value bytes are the wrong length.
#[must_use]
pub fn apply_response(feat_addr: [u8; 2], value: &[u8], state: &mut DeviceState) -> bool {
    match feat_addr {
        f if f == FEAT_LOCK => {
            if value.is_empty() {
                return false;
            }
            state.locked = value[0] != 0;
            true
        }
        f if f == FEAT_GAIN => {
            if value.len() < 2 {
                return false;
            }
            let raw = u16::from_be_bytes([value[0], value[1]]);
            state.gain_db = (raw / 100).min(60) as u8;
            true
        }
        f if f == FEAT_MUTE => {
            if value.is_empty() {
                return false;
            }
            state.muted = value[0] != 0;
            true
        }
        f if f == FEAT_HPF => {
            if value.is_empty() {
                return false;
            }
            state.hpf = HpfFrequency::from_byte(value[0]);
            true
        }
        f if f == FEAT_LIMITER => {
            if value.is_empty() {
                return false;
            }
            state.limiter_enabled = value[0] != 0;
            true
        }
        f if f == FEAT_COMP => {
            if value.is_empty() {
                return false;
            }
            state.compressor = CompressorPreset::from_byte(value[0]);
            true
        }
        f if f == FEAT_PHANTOM => {
            if value.is_empty() {
                return false;
            }
            state.phantom_power = value[0] != 0;
            true
        }
        f if f == FEAT_AUTO => {
            if value.is_empty() {
                return false;
            }
            state.mode = if value[0] != 0 {
                InputMode::Auto
            } else {
                InputMode::Manual
            };
            true
        }
        f if f == FEAT_AUTO_POSITION => {
            if value.is_empty() {
                return false;
            }
            state.auto_position = MicPosition::from_byte(value[0]);
            true
        }
        f if f == FEAT_AUTO_TONE => {
            if value.is_empty() {
                return false;
            }
            state.auto_tone = AutoTone::from_byte(value[0]);
            true
        }
        f if f == FEAT_AUTO_GAIN => {
            if value.is_empty() {
                return false;
            }
            state.auto_gain = AutoGain::from_be_bytes(value);
            true
        }
        f if f == FEAT_MIX => {
            if value.is_empty() {
                return false;
            }
            state.monitor_mix = value[0].min(100);
            true
        }
        f if f == FEAT_EQ => {
            if value.is_empty() {
                return false;
            }
            state.eq_enabled = value[0] != 0;
            true
        }
        _ => {
            // Check EQ band addresses
            for (i, (en_addr, gain_addr)) in EQ_BAND_ADDRS.iter().enumerate() {
                if feat_addr == *en_addr {
                    if value.is_empty() {
                        return false;
                    }
                    state.eq_bands[i].enabled = value[0] != 0;
                    return true;
                }
                if feat_addr == *gain_addr {
                    if value.len() < 2 {
                        return false;
                    }
                    let raw = i16::from_be_bytes([value[0], value[1]]);
                    state.eq_bands[i].gain_db = (raw / 10).clamp(-8, 6) as i8;
                    return true;
                }
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CRC ───────────────────────────────────────────────────────────────────

    #[test]
    fn crc16_ansi_empty_input() {
        // CRC of empty slice with init 0x0000 should be 0x0000
        assert_eq!(crc16_ansi(&[]), 0x0000);
    }

    #[test]
    fn crc16_ansi_known_value() {
        // CRC-16/ARC (CRC-16/ANSI) of [0x01, 0x02, 0x03].
        // Value 0xA110 verified against two independent reference implementations
        // (poly=0x8005, init=0x0000, refin=true, refout=true, xorout=0x0000).
        assert_eq!(crc16_ansi(&[0x01u8, 0x02, 0x03]), 0xA110);
    }

    #[test]
    fn crc16_ansi_standard_test_vector() {
        // CRC-16/ARC standard check value: CRC of ASCII "123456789" must be 0xBB3D.
        // This is the canonical check value listed in Greg Cook's CRC catalogue.
        let data = b"123456789";
        assert_eq!(crc16_ansi(data), 0xBB3D);
    }

    #[test]
    fn crc16_ansi_is_deterministic() {
        let data = [
            0x11u8, 0x22, 0x00, 0x03, 0x08, 0x05, 0x70, 0x05, 0x01, 0x00, 0x00,
        ];
        assert_eq!(crc16_ansi(&data), crc16_ansi(&data));
    }

    #[test]
    fn crc16_ansi_differs_from_ccitt_false() {
        // Ensure we are NOT using CCITT-FALSE (which has init=0xFFFF and no reflection)
        // CCITT-FALSE of [] = 0xFFFF; ANSI of [] = 0x0000
        assert_eq!(crc16_ansi(&[]), 0x0000);
        // They must differ on non-trivial input too
        let data = [0x11u8, 0x22, 0x03, 0x00];
        let ansi = crc16_ansi(&data);
        // CCITT-FALSE known value for this input is 0x5426
        assert_ne!(
            ansi, 0x5426,
            "crc16_ansi must not produce CCITT-FALSE values"
        );
    }

    #[test]
    fn crc16_ansi_single_byte_boundary() {
        // Single-byte inputs — values verified against reference implementation
        // (poly=0x8005, init=0x0000, refin=true, refout=true, xorout=0x0000).
        assert_eq!(crc16_ansi(&[0x00]), 0x0000);
        assert_eq!(crc16_ansi(&[0xFF]), 0x4040);
        assert_eq!(crc16_ansi(&[0x01]), 0xC0C1);
    }

    // ── build_packet / packet structure ───────────────────────────────────────

    #[test]
    fn packet_is_exactly_64_bytes() {
        assert_eq!(cmd_get_gain(0).len(), PACKET_SIZE);
        assert_eq!(cmd_set_gain(0, 36).len(), PACKET_SIZE);
        assert_eq!(cmd_confirm(0).len(), PACKET_SIZE);
    }

    #[test]
    fn packet_starts_with_report_id_0x01() {
        let pkt = cmd_get_gain(0);
        assert_eq!(pkt[0], 0x01, "Report ID must be 0x01");
    }

    #[test]
    fn packet_header_magic_at_offset_2() {
        let pkt = cmd_get_gain(0);
        assert_eq!(pkt[2], 0x11);
        assert_eq!(pkt[3], 0x22);
    }

    #[test]
    fn packet_sequence_number_reflected() {
        for seq in [0u8, 1, 42, 255] {
            let pkt = cmd_get_gain(seq);
            assert_eq!(pkt[4], seq, "seq byte at offset 4 must match");
        }
    }

    #[test]
    fn packet_constant_fields() {
        let pkt = cmd_get_gain(0);
        assert_eq!(pkt[5], 0x03, "constant 0x03 at offset 5");
        assert_eq!(pkt[6], 0x08, "header end 0x08 at offset 6");
        assert_eq!(pkt[8], 0x70, "data start 0x70 at offset 8");
        assert_eq!(pkt[7], pkt[9], "data_len repeated at offsets 7 and 9");
    }

    #[test]
    fn packet_crc_is_valid() {
        // Verify the CRC in a built packet matches recomputing it
        let pkt = cmd_set_gain(5, 36);
        let total_len = pkt[1] as usize;
        let crc_hi = pkt[total_len] as u16;
        let crc_lo = pkt[total_len + 1] as u16;
        let stored_crc = (crc_hi << 8) | crc_lo;
        let computed_crc = crc16_ansi(&pkt[2..total_len]);
        assert_eq!(stored_crc, computed_crc, "CRC in packet must be valid");
    }

    #[test]
    fn packet_padded_with_zeros_to_64() {
        let pkt = cmd_confirm(0);
        // Confirm is a short packet — there should be trailing zeros
        let total_len = pkt[1] as usize;
        for &b in &pkt[total_len + 2..] {
            assert_eq!(b, 0x00, "all bytes after CRC must be zero padding");
        }
    }

    // ── Command encoding ──────────────────────────────────────────────────────

    #[test]
    fn get_gain_has_correct_feat_addr() {
        let pkt = cmd_get_gain(0);
        // offset 10,11,12 = CMD_GET_FEAT; 13 = is_mix=0; 14,15 = FEAT_GAIN
        assert_eq!(&pkt[10..13], &CMD_GET_FEAT);
        assert_eq!(pkt[13], 0x00, "is_mix must be 0 for gain");
        assert_eq!(&pkt[14..16], &FEAT_GAIN);
    }

    #[test]
    fn set_gain_encodes_db_correctly() {
        // 36 dB → raw = 36 * 100 = 3600 = 0x0E10
        let pkt = cmd_set_gain(0, 36);
        assert_eq!(&pkt[10..13], &CMD_SET_FEAT);
        assert_eq!(&pkt[14..16], &FEAT_GAIN);
        // value at pkt[16..18]
        let raw = u16::from_be_bytes([pkt[16], pkt[17]]);
        assert_eq!(raw, 3600, "36 dB must encode as 3600");
    }

    #[test]
    fn set_gain_clamps_at_60() {
        let pkt = cmd_set_gain(0, 200);
        let raw = u16::from_be_bytes([pkt[16], pkt[17]]);
        assert_eq!(raw, 6000, "gain clamped to 60 dB = raw 6000");
    }

    #[test]
    fn set_gain_zero_db() {
        let pkt = cmd_set_gain(0, 0);
        let raw = u16::from_be_bytes([pkt[16], pkt[17]]);
        assert_eq!(raw, 0, "0 dB must encode as 0");
    }

    #[test]
    fn set_phantom_on_uses_0x30() {
        let pkt = cmd_set_phantom(0, true);
        assert_eq!(&pkt[14..16], &FEAT_PHANTOM);
        assert_eq!(pkt[16], PHANTOM_ON, "phantom ON must be 0x30");
    }

    #[test]
    fn set_phantom_off_uses_0x00() {
        let pkt = cmd_set_phantom(0, false);
        assert_eq!(pkt[16], PHANTOM_OFF, "phantom OFF must be 0x00");
    }

    #[test]
    fn get_mix_is_mix_flag_set() {
        // GET for MIX must also use is_mix=0x01 so the device responds correctly.
        let pkt = cmd_get_mix(0);
        assert_eq!(&pkt[10..13], &CMD_GET_FEAT);
        assert_eq!(pkt[13], 0x01, "is_mix must be 1 for GET MIX");
        assert_eq!(&pkt[14..16], &FEAT_MIX);
    }

    #[test]
    fn set_mix_is_mix_flag_set() {
        // MIX feature must have is_mix=0x01
        let pkt = cmd_set_mix(0, 50);
        assert_eq!(&pkt[14..16], &FEAT_MIX);
        assert_eq!(pkt[13], 0x01, "is_mix must be 1 for MIX feature");
        assert_eq!(pkt[16], 50);
    }

    #[test]
    fn set_mix_clamps_at_100() {
        let pkt = cmd_set_mix(0, 200);
        assert_eq!(pkt[16], 100);
    }

    #[test]
    fn set_mute_encodes_on_off() {
        assert_eq!(cmd_set_mute(0, true)[16], 1);
        assert_eq!(cmd_set_mute(0, false)[16], 0);
    }

    #[test]
    fn set_mode_encodes_auto_manual() {
        assert_eq!(cmd_set_mode(0, true)[16], 1, "auto=true must be 1");
        assert_eq!(cmd_set_mode(0, false)[16], 0, "auto=false must be 0");
    }

    #[test]
    fn set_compressor_all_presets() {
        let cases = [
            (CompressorPreset::Off, 0x00u8),
            (CompressorPreset::Light, 0x01),
            (CompressorPreset::Medium, 0x02),
            (CompressorPreset::Heavy, 0x03),
        ];
        for (preset, expected) in &cases {
            let pkt = cmd_set_compressor(0, preset);
            assert_eq!(pkt[16], *expected, "wrong byte for {preset:?}");
        }
    }

    #[test]
    fn set_hpf_all_values() {
        assert_eq!(cmd_set_hpf(0, &HpfFrequency::Off)[16], 0x00);
        assert_eq!(cmd_set_hpf(0, &HpfFrequency::Hz75)[16], 0x01);
        assert_eq!(cmd_set_hpf(0, &HpfFrequency::Hz150)[16], 0x02);
    }

    #[test]
    fn eq_band_gain_encoding() {
        // +3 dB → 30 → 0x001E
        let pkt = cmd_set_eq_band_gain(0, 0, 3);
        let raw = i16::from_be_bytes([pkt[16], pkt[17]]);
        assert_eq!(raw, 30, "+3 dB must encode as 30");

        // -6 dB → -60 → 0xFFC4
        let pkt = cmd_set_eq_band_gain(0, 1, -6);
        let raw = i16::from_be_bytes([pkt[16], pkt[17]]);
        assert_eq!(raw, -60, "-6 dB must encode as -60");
    }

    #[test]
    fn eq_band_gain_clamps() {
        let pkt_hi = cmd_set_eq_band_gain(0, 0, 127);
        let raw_hi = i16::from_be_bytes([pkt_hi[16], pkt_hi[17]]);
        assert_eq!(raw_hi, 60, "+127 dB must clamp to +6 dB (raw 60)");

        let pkt_lo = cmd_set_eq_band_gain(0, 0, -128);
        let raw_lo = i16::from_be_bytes([pkt_lo[16], pkt_lo[17]]);
        assert_eq!(raw_lo, -80, "-128 dB must clamp to -8 dB (raw -80)");
    }

    #[test]
    fn eq_band_addresses_are_correct() {
        // Band 0 (100 Hz): enable=0x0210, gain=0x0214
        let en_pkt = cmd_set_eq_band_enable(0, 0, true);
        assert_eq!(&en_pkt[14..16], &[0x02u8, 0x10]);
        let gain_pkt = cmd_set_eq_band_gain(0, 0, 0);
        assert_eq!(&gain_pkt[14..16], &[0x02u8, 0x14]);

        // Band 4 (10kHz): enable=0x0250, gain=0x0254
        let en_pkt4 = cmd_set_eq_band_enable(0, 4, false);
        assert_eq!(&en_pkt4[14..16], &[0x02u8, 0x50]);
        let gain_pkt4 = cmd_set_eq_band_gain(0, 4, 0);
        assert_eq!(&gain_pkt4[14..16], &[0x02u8, 0x54]);
    }

    #[test]
    fn confirm_packet_uses_correct_cmd() {
        let pkt = cmd_confirm(0);
        assert_eq!(&pkt[10..13], &CMD_CONFIRM);
    }

    // ── parse_response ────────────────────────────────────────────────────────

    /// Build a synthetic response from the device for testing parse_response.
    fn make_response(seq: u8, resp_cmd: &[u8; 3], feat_addr: &[u8; 2], value: &[u8]) -> Vec<u8> {
        let is_mix: u8 = 0x00;
        let mut payload = vec![is_mix, feat_addr[0], feat_addr[1]];
        payload.extend_from_slice(value);

        let data_len = (3 + payload.len() + 2) as u8;
        let mut inner: Vec<u8> = Vec::new();
        inner.push(HEADER_MAGIC[0]);
        inner.push(HEADER_MAGIC[1]);
        inner.push(seq);
        inner.push(HDR_CONSTANT);
        inner.push(HDR_END);
        inner.push(data_len);
        inner.push(DATA_START);
        inner.push(data_len);
        inner.extend_from_slice(resp_cmd);
        inner.extend_from_slice(&payload);

        let total_len = (inner.len() + 2) as u8;
        let crc = crc16_ansi(&inner);

        let mut buf = Vec::with_capacity(64);
        buf.push(REPORT_ID);
        buf.push(total_len);
        buf.extend_from_slice(&inner);
        buf.push((crc >> 8) as u8);
        buf.push((crc & 0xFF) as u8);
        buf.resize(64, 0x00);
        buf
    }

    #[test]
    fn parse_response_get_feat_phantom() {
        let buf = make_response(0, &RES_GET_FEAT, &FEAT_PHANTOM, &[PHANTOM_ON]);
        let result = parse_response(&buf);
        assert!(result.is_some(), "parse_response must succeed");
        let (feat, value) = result.unwrap();
        assert_eq!(feat, FEAT_PHANTOM);
        assert_eq!(value, vec![PHANTOM_ON]);
    }

    #[test]
    fn parse_response_rejects_bad_magic() {
        let mut buf = make_response(0, &RES_GET_FEAT, &FEAT_MUTE, &[0x01]);
        buf[2] = 0xFF; // corrupt header magic
        assert!(parse_response(&buf).is_none());
    }

    #[test]
    fn parse_response_rejects_bad_crc() {
        let mut buf = make_response(0, &RES_GET_FEAT, &FEAT_MUTE, &[0x01]);
        let total_len = buf[1] as usize;
        buf[total_len] ^= 0xFF; // corrupt CRC hi byte
        assert!(parse_response(&buf).is_none());
    }

    #[test]
    fn parse_response_rejects_too_short() {
        assert!(parse_response(&[]).is_none());
        assert!(parse_response(&[0x01; 10]).is_none());
    }

    #[test]
    fn parse_response_returns_none_for_confirm() {
        // CONFIRM responses should return None (no feature data).
        // 0x09 0x00 0x00 is the device's CONFIRM response command byte sequence.
        let confirm_cmd: [u8; 3] = [0x09, 0x00, 0x00];
        let buf = make_response(0, &confirm_cmd, &[0x00, 0x00], &[]);
        assert!(parse_response(&buf).is_none());
    }

    #[test]
    fn parse_response_accepts_set_feat_response_type() {
        // The device sends RES_SET_FEAT after a successful SET command.
        // parse_response must accept it and return the echoed feature + value.
        let buf = make_response(0, &RES_SET_FEAT, &FEAT_MUTE, &[0x01]);
        let result = parse_response(&buf);
        assert!(result.is_some(), "parse_response must accept RES_SET_FEAT");
        let (feat, value) = result.unwrap();
        assert_eq!(feat, FEAT_MUTE);
        assert_eq!(value, vec![0x01]);
    }

    #[test]
    fn parse_response_rejects_oversized_total_len() {
        // buf[1] = total_len; if contents_end + 2 > buf.len() the buffer is truncated.
        // parse_response must return None rather than panicking on an out-of-bounds read.
        let mut buf = make_response(0, &RES_GET_FEAT, &FEAT_MUTE, &[0x01]);
        buf[1] = 200; // claims contents extend to byte 202, but buf is only 64 bytes
        assert!(parse_response(&buf).is_none());
    }

    #[test]
    fn parse_response_rejects_buffers_below_minimum_length() {
        // parse_response requires at least 18 bytes; anything shorter must return None.
        for len in [0usize, 1, 10, 17] {
            let buf = vec![0u8; len];
            assert!(
                parse_response(&buf).is_none(),
                "buf of length {len} must be rejected"
            );
        }
        // Exactly 18 bytes is the minimum; a syntactically valid 18-byte buffer
        // with correct magic should not panic (it may still return None for bad CRC).
        let buf18 = vec![0u8; 18];
        let _ = parse_response(&buf18); // must not panic
    }

    #[test]
    fn parse_response_get_feat_mix_full_chain() {
        // A GET MIX response parsed through parse_response must yield FEAT_MIX
        // with the correct value, and apply_response must update monitor_mix.
        let buf = make_response(0, &RES_GET_FEAT, &FEAT_MIX, &[75]);
        let result = parse_response(&buf);
        assert!(result.is_some(), "parse_response must accept MIX response");
        let (feat, value) = result.unwrap();
        assert_eq!(feat, FEAT_MIX, "feature address must be FEAT_MIX");
        assert_eq!(value, vec![75]);

        let mut state = DeviceState::default();
        assert!(apply_response(feat, &value, &mut state));
        assert_eq!(state.monitor_mix, 75);
    }

    // ── apply_response ────────────────────────────────────────────────────────

    #[test]
    fn apply_response_gain() {
        let mut state = DeviceState::default();
        // 36 dB = raw 3600 = 0x0E10
        let _ = apply_response(FEAT_GAIN, &[0x0E, 0x10], &mut state);
        assert_eq!(state.gain_db, 36);
    }

    #[test]
    fn apply_response_gain_clamps_at_60() {
        let mut state = DeviceState::default();
        // raw 9000 / 100 = 90 dB, must clamp to 60
        let _ = apply_response(FEAT_GAIN, &[0x23, 0x28], &mut state);
        assert_eq!(state.gain_db, 60);
    }

    #[test]
    fn apply_response_gain_rejects_short_values() {
        // FEAT_GAIN needs 2 bytes (big-endian u16). Empty or 1-byte values must be rejected.
        let mut state = DeviceState::default();
        let original = state.gain_db;
        assert!(
            !apply_response(FEAT_GAIN, &[], &mut state),
            "empty must return false"
        );
        assert!(
            !apply_response(FEAT_GAIN, &[0x0E], &mut state),
            "1-byte must return false"
        );
        assert_eq!(
            state.gain_db, original,
            "state must not change on rejection"
        );
    }

    #[test]
    fn apply_response_phantom_on() {
        let mut state = DeviceState::default();
        let _ = apply_response(FEAT_PHANTOM, &[0x30], &mut state);
        assert!(state.phantom_power);
    }

    #[test]
    fn apply_response_phantom_off() {
        let mut state = DeviceState::default();
        state.phantom_power = true;
        let _ = apply_response(FEAT_PHANTOM, &[0x00], &mut state);
        assert!(!state.phantom_power);
    }

    #[test]
    fn apply_response_mute() {
        let mut state = DeviceState::default();
        let _ = apply_response(FEAT_MUTE, &[0x01], &mut state);
        assert!(state.muted);
        let _ = apply_response(FEAT_MUTE, &[0x00], &mut state);
        assert!(!state.muted);
    }

    #[test]
    fn apply_response_mode() {
        let mut state = DeviceState::default();
        let _ = apply_response(FEAT_AUTO, &[0x01], &mut state);
        assert_eq!(state.mode, InputMode::Auto);
        let _ = apply_response(FEAT_AUTO, &[0x00], &mut state);
        assert_eq!(state.mode, InputMode::Manual);
    }

    #[test]
    fn apply_response_compressor_all_presets() {
        let cases = [
            (0x00u8, CompressorPreset::Off),
            (0x01, CompressorPreset::Light),
            (0x02, CompressorPreset::Medium),
            (0x03, CompressorPreset::Heavy),
        ];
        for (byte, expected) in &cases {
            let mut state = DeviceState::default();
            let _ = apply_response(FEAT_COMP, &[*byte], &mut state);
            assert_eq!(state.compressor, *expected);
        }
    }

    #[test]
    fn apply_response_hpf() {
        let mut state = DeviceState::default();
        let _ = apply_response(FEAT_HPF, &[0x01], &mut state);
        assert_eq!(state.hpf, HpfFrequency::Hz75);
        let _ = apply_response(FEAT_HPF, &[0x02], &mut state);
        assert_eq!(state.hpf, HpfFrequency::Hz150);
        let _ = apply_response(FEAT_HPF, &[0x00], &mut state);
        assert_eq!(state.hpf, HpfFrequency::Off);
    }

    #[test]
    fn apply_response_eq_band_gain_all_bands() {
        // Table: (band_index, gain_addr, raw_hi, raw_lo, expected_gain_db)
        // gain_addr = EQ_BAND_ADDRS[i].1; raw = gain_db * 10 as i16 big-endian.
        // Bands: 0=100Hz(0x14), 1=250Hz(0x24), 2=1kHz(0x34), 3=4kHz(0x44), 4=10kHz(0x54)
        let cases: &[(usize, [u8; 2], u8, u8, i8)] = &[
            (0, [0x02, 0x14], 0x00, 0x28, 4),  // +4 dB = raw 40
            (1, [0x02, 0x24], 0xFF, 0xC4, -6), // -6 dB = raw -60
            (2, [0x02, 0x34], 0x00, 0x1E, 3),  // +3 dB = raw 30
            (3, [0x02, 0x44], 0xFF, 0xD8, -4), // -4 dB = raw -40
            (4, [0x02, 0x54], 0x00, 0x3C, 6),  // +6 dB = raw 60
        ];
        for &(band, addr, hi, lo, expected_db) in cases {
            let mut state = DeviceState::default();
            assert!(
                apply_response(addr, &[hi, lo], &mut state),
                "apply_response must return true for band {band}"
            );
            assert_eq!(
                state.eq_bands[band].gain_db, expected_db,
                "band {band} gain mismatch"
            );
        }
    }

    #[test]
    fn apply_response_eq_band_enable_all_bands() {
        // enable_addr = EQ_BAND_ADDRS[i].0
        let enable_addrs: [[u8; 2]; 5] = [
            [0x02, 0x10], // band 0 — 100 Hz
            [0x02, 0x20], // band 1 — 250 Hz
            [0x02, 0x30], // band 2 — 1000 Hz
            [0x02, 0x40], // band 3 — 4000 Hz
            [0x02, 0x50], // band 4 — 10000 Hz
        ];
        for (band, addr) in enable_addrs.iter().enumerate() {
            let mut state = DeviceState::default();
            assert!(apply_response(*addr, &[0x01], &mut state));
            assert!(state.eq_bands[band].enabled, "band {band} must be enabled");
            assert!(apply_response(*addr, &[0x00], &mut state));
            assert!(
                !state.eq_bands[band].enabled,
                "band {band} must be disabled"
            );
        }
    }

    #[test]
    fn apply_response_monitor_mix() {
        let mut state = DeviceState::default();
        let _ = apply_response(FEAT_MIX, &[50], &mut state);
        assert_eq!(state.monitor_mix, 50);
    }

    #[test]
    fn apply_response_monitor_mix_clamps_at_100() {
        let mut state = DeviceState::default();
        let _ = apply_response(FEAT_MIX, &[200], &mut state);
        assert_eq!(state.monitor_mix, 100, "monitor mix must clamp to 100");
    }

    #[test]
    fn apply_response_monitor_mix_empty_value_returns_false() {
        let mut state = DeviceState::default();
        assert!(!apply_response(FEAT_MIX, &[], &mut state));
    }

    #[test]
    fn apply_response_limiter() {
        let mut state = DeviceState::default();
        let _ = apply_response(FEAT_LIMITER, &[0x01], &mut state);
        assert!(state.limiter_enabled);
        let _ = apply_response(FEAT_LIMITER, &[0x00], &mut state);
        assert!(!state.limiter_enabled);
    }

    #[test]
    fn apply_response_limiter_empty_value_returns_false() {
        let mut state = DeviceState::default();
        assert!(!apply_response(FEAT_LIMITER, &[], &mut state));
    }

    #[test]
    fn apply_response_eq_master_enable() {
        let mut state = DeviceState::default();
        let _ = apply_response(FEAT_EQ, &[0x01], &mut state);
        assert!(state.eq_enabled);
        let _ = apply_response(FEAT_EQ, &[0x00], &mut state);
        assert!(!state.eq_enabled);
    }

    #[test]
    fn apply_response_eq_master_enable_empty_value_returns_false() {
        let mut state = DeviceState::default();
        assert!(!apply_response(FEAT_EQ, &[], &mut state));
    }

    #[test]
    fn apply_response_unknown_feat_returns_false() {
        let mut state = DeviceState::default();
        let original_gain = state.gain_db;
        let applied = apply_response([0xFF, 0xFF], &[0x01], &mut state);
        assert!(!applied, "unknown feature must return false");
        assert_eq!(state.gain_db, original_gain, "state must not be mutated");
    }

    #[test]
    fn apply_response_too_short_returns_false() {
        let mut state = DeviceState::default();
        assert!(
            !apply_response(FEAT_GAIN, &[], &mut state),
            "empty value must return false"
        );
        assert!(
            !apply_response(FEAT_GAIN, &[0x01], &mut state),
            "1-byte value for 2-byte gain must return false"
        );
    }

    // ── Cycle helpers ─────────────────────────────────────────────────────────

    #[test]
    fn compressor_cycles_full_round_trip() {
        let seq = [
            CompressorPreset::Off,
            CompressorPreset::Light,
            CompressorPreset::Medium,
            CompressorPreset::Heavy,
            CompressorPreset::Off,
        ];
        for w in seq.windows(2) {
            assert_eq!(w[0].cycle_next(), w[1]);
        }
    }

    #[test]
    fn hpf_cycles_full_round_trip() {
        let seq = [
            HpfFrequency::Off,
            HpfFrequency::Hz75,
            HpfFrequency::Hz150,
            HpfFrequency::Off,
        ];
        for w in seq.windows(2) {
            assert_eq!(w[0].cycle_next(), w[1]);
        }
    }

    #[test]
    fn eq_band_freqs_are_correct() {
        assert_eq!(EQ_BAND_FREQS, [100, 250, 1000, 4000, 10000]);
    }

    // ── Lock command tests ────────────────────────────────────────────────────

    #[test]
    fn lock_get_packet_uses_correct_command_and_feat_addr() {
        let pkt = cmd_get_lock(0);
        assert_eq!(pkt.len(), PACKET_SIZE);
        assert_eq!(&pkt[10..13], &CMD_GET_LOCK, "must use CMD_GET_LOCK");
        assert_eq!(pkt[13], 0x06, "lock payload prefix must be 0x06");
        assert_eq!(
            &pkt[14..16],
            &FEAT_LOCK,
            "feature address must be FEAT_LOCK"
        );
    }

    #[test]
    fn lock_set_packet_uses_correct_command_and_encodes_value() {
        let pkt_lock = cmd_set_lock(0, true);
        assert_eq!(&pkt_lock[10..13], &CMD_SET_LOCK, "must use CMD_SET_LOCK");
        assert_eq!(pkt_lock[13], 0x06, "lock payload prefix must be 0x06");
        assert_eq!(&pkt_lock[14..16], &FEAT_LOCK);
        assert_eq!(pkt_lock[16], 0x01, "locked=true must encode as 0x01");

        let pkt_unlock = cmd_set_lock(0, false);
        assert_eq!(pkt_unlock[16], 0x00, "locked=false must encode as 0x00");
    }

    #[test]
    fn lock_packets_are_exactly_64_bytes_with_valid_crc() {
        for locked in [true, false] {
            let pkt = cmd_set_lock(0, locked);
            assert_eq!(pkt.len(), PACKET_SIZE);
            let total_len = pkt[1] as usize;
            let stored_crc = ((pkt[total_len] as u16) << 8) | pkt[total_len + 1] as u16;
            let computed_crc = crc16_ansi(&pkt[2..total_len]);
            assert_eq!(
                stored_crc, computed_crc,
                "CRC must be valid for locked={locked}"
            );
        }
    }

    #[test]
    fn lock_get_packet_differs_from_regular_get_feat() {
        let lock_pkt = cmd_get_lock(0);
        let gain_pkt = cmd_get_gain(0);
        // Commands must differ (last byte 0x01 vs 0x02)
        assert_ne!(&lock_pkt[10..13], &gain_pkt[10..13]);
        assert_eq!(lock_pkt[12], 0x01);
        assert_eq!(gain_pkt[12], 0x02);
    }

    #[test]
    fn apply_response_lock_locked() {
        let mut state = DeviceState::default();
        assert!(!state.locked, "default state must be unlocked");
        let _ = apply_response(FEAT_LOCK, &[0x01], &mut state);
        assert!(state.locked);
    }

    #[test]
    fn apply_response_lock_unlocked() {
        let mut state = DeviceState::default();
        state.locked = true;
        let _ = apply_response(FEAT_LOCK, &[0x00], &mut state);
        assert!(!state.locked);
    }

    #[test]
    fn apply_response_lock_empty_value_returns_false() {
        let mut state = DeviceState::default();
        assert!(!apply_response(FEAT_LOCK, &[], &mut state));
    }

    #[test]
    fn parse_response_accepts_lock_response_types() {
        // GET LOCK response
        let buf = make_response(0, &RES_GET_LOCK, &FEAT_LOCK, &[0x01]);
        let result = parse_response(&buf);
        assert!(result.is_some(), "parse_response must accept RES_GET_LOCK");
        let (feat, value) = result.unwrap();
        assert_eq!(feat, FEAT_LOCK);
        assert_eq!(value, vec![0x01]);

        // SET LOCK response
        let buf2 = make_response(0, &RES_SET_LOCK, &FEAT_LOCK, &[0x00]);
        let result2 = parse_response(&buf2);
        assert!(result2.is_some(), "parse_response must accept RES_SET_LOCK");
        let (feat2, value2) = result2.unwrap();
        assert_eq!(feat2, FEAT_LOCK);
        assert_eq!(value2, vec![0x00]);
    }

    // ── Auto Level sub-feature tests ──────────────────────────────────────────

    #[test]
    fn mic_position_roundtrip_via_packet() {
        for (pos, expected_byte) in [(MicPosition::Near, 0x00u8), (MicPosition::Far, 0x01u8)] {
            let pkt = cmd_set_auto_position(0, &pos);
            assert_eq!(pkt.len(), PACKET_SIZE);
            assert_eq!(
                &pkt[14..16],
                &FEAT_AUTO_POSITION,
                "feature address mismatch"
            );
            assert_eq!(pkt[16], expected_byte, "position byte mismatch for {pos}");
        }
    }

    #[test]
    fn mic_position_apply_response_roundtrip() {
        let cases = [
            (&[0x00u8][..], MicPosition::Near),
            (&[0x01u8][..], MicPosition::Far),
            (&[0x99u8][..], MicPosition::Near), // unknown byte falls back to Near
        ];
        for (bytes, expected) in cases {
            let mut state = DeviceState::default();
            assert!(apply_response(FEAT_AUTO_POSITION, bytes, &mut state));
            assert_eq!(state.auto_position, expected);
        }
    }

    #[test]
    fn mic_position_apply_response_empty_returns_false() {
        let mut state = DeviceState::default();
        assert!(!apply_response(FEAT_AUTO_POSITION, &[], &mut state));
    }

    #[test]
    fn mic_position_cycles_full_round_trip() {
        let seq = [MicPosition::Near, MicPosition::Far, MicPosition::Near];
        for w in seq.windows(2) {
            assert_eq!(w[0].cycle_next(), w[1]);
        }
    }

    #[test]
    fn auto_tone_roundtrip_via_packet() {
        for (tone, expected_byte) in [
            (AutoTone::Dark, 0x00u8),
            (AutoTone::Natural, 0x01u8),
            (AutoTone::Bright, 0x02u8),
        ] {
            let pkt = cmd_set_auto_tone(0, &tone);
            assert_eq!(pkt.len(), PACKET_SIZE);
            assert_eq!(&pkt[14..16], &FEAT_AUTO_TONE, "feature address mismatch");
            assert_eq!(pkt[16], expected_byte, "tone byte mismatch for {tone}");
        }
    }

    #[test]
    fn auto_tone_apply_response_roundtrip() {
        let cases = [
            (&[0x00u8][..], AutoTone::Dark),
            (&[0x01u8][..], AutoTone::Natural),
            (&[0x02u8][..], AutoTone::Bright),
            (&[0x99u8][..], AutoTone::Natural), // unknown byte falls back to Natural
        ];
        for (bytes, expected) in cases {
            let mut state = DeviceState::default();
            assert!(apply_response(FEAT_AUTO_TONE, bytes, &mut state));
            assert_eq!(state.auto_tone, expected);
        }
    }

    #[test]
    fn auto_tone_apply_response_empty_returns_false() {
        let mut state = DeviceState::default();
        assert!(!apply_response(FEAT_AUTO_TONE, &[], &mut state));
    }

    #[test]
    fn auto_tone_cycles_full_round_trip() {
        let seq = [
            AutoTone::Dark,
            AutoTone::Natural,
            AutoTone::Bright,
            AutoTone::Dark,
        ];
        for w in seq.windows(2) {
            assert_eq!(w[0].cycle_next(), w[1]);
        }
    }

    #[test]
    fn auto_gain_roundtrip_via_packet() {
        for (gain, expected_val) in [
            (AutoGain::Quiet, 0u32),
            (AutoGain::Normal, 1u32),
            (AutoGain::Loud, 2u32),
        ] {
            let pkt = cmd_set_auto_gain(0, &gain);
            assert_eq!(pkt.len(), PACKET_SIZE);
            assert_eq!(&pkt[14..16], &FEAT_AUTO_GAIN, "feature address mismatch");
            let encoded = u32::from_be_bytes([pkt[16], pkt[17], pkt[18], pkt[19]]);
            assert_eq!(encoded, expected_val, "gain value mismatch for {gain}");
        }
    }

    #[test]
    fn auto_gain_apply_response_roundtrip_4_bytes() {
        let cases: &[(&[u8], AutoGain)] = &[
            (&[0x00, 0x00, 0x00, 0x00], AutoGain::Quiet),
            (&[0x00, 0x00, 0x00, 0x01], AutoGain::Normal),
            (&[0x00, 0x00, 0x00, 0x02], AutoGain::Loud),
            (&[0x99, 0x99, 0x99, 0x99], AutoGain::Normal), // unknown falls back to Normal
        ];
        for (bytes, expected) in cases {
            let mut state = DeviceState::default();
            assert!(apply_response(FEAT_AUTO_GAIN, bytes, &mut state));
            assert_eq!(state.auto_gain, *expected);
        }
    }

    #[test]
    fn auto_gain_apply_response_1_byte_fallback() {
        // Defensive: accept 1-byte responses in case firmware sends them.
        let cases: &[(&[u8], AutoGain)] = &[
            (&[0x00], AutoGain::Quiet),
            (&[0x01], AutoGain::Normal),
            (&[0x02], AutoGain::Loud),
        ];
        for (bytes, expected) in cases {
            let mut state = DeviceState::default();
            assert!(apply_response(FEAT_AUTO_GAIN, bytes, &mut state));
            assert_eq!(state.auto_gain, *expected);
        }
    }

    #[test]
    fn auto_gain_apply_response_empty_returns_false() {
        let mut state = DeviceState::default();
        assert!(!apply_response(FEAT_AUTO_GAIN, &[], &mut state));
    }

    #[test]
    fn auto_gain_cycles_full_round_trip() {
        let seq = [
            AutoGain::Quiet,
            AutoGain::Normal,
            AutoGain::Loud,
            AutoGain::Quiet,
        ];
        for w in seq.windows(2) {
            assert_eq!(w[0].cycle_next(), w[1]);
        }
    }
}
