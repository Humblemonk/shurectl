//! Configuration protocol of the original MV7 (PID 0x1012).
//!
//! Unlike every other supported model, the MV7 does not speak the binary
//! `0x11 0x22` framing. Its vendor HID interface (usage page 0xFF00, 64-byte
//! reports, no report IDs) runs an ASCII command shell:
//!
//! - Output report: report ID `0x00`, one command line ending in `\n`, zero padded.
//! - Input reports: the reply text, NUL padded, one or more `\n`-terminated lines.
//! - GET is the bare command (`micMute`), answered with `micMute=off`.
//! - SET appends the value (`micMute on`), answered with the applied value. No
//!   CONFIRM packet is needed.
//! - `[Failed]` means the command was rejected or changed nothing (for example a
//!   gain that rounds to the current value). `Locked` means the setting is not
//!   adjustable right now (gain in Auto Level).
//! - A mode change answers after ~200 ms and first reports the new gain on a
//!   line of its own.
//! - DSP settings live in blocks addressed by hex ID: `getBlock 19` answers
//!   `block 19 00000000`; `setBlock 19 00000001` writes it. The firmware mirrors
//!   each block into pages 0x1xx and 0x2xx by itself, so only the base ID is written.
//!
//! Confirmed against firmware 0.0.52.0 (package 1.2.19) by probing the shell and
//! against the command strings MOTIV Mix sends. The Limiter block (`1F`) is not:
//! its ID and values come from Shure's `DSP-Settings-MV7.xml` and the block table
//! decoded by schlarpc/shure-mv7-firmware-re, and nobody has read or written it
//! on an MV7 (unconfirmed). Toggling MOTIV's limiter changed no block shurectl
//! reads (2026-09-25), so MOTIV may limit in the app instead. `bootDSP` and
//! firmware commands are intentionally absent.

use super::{AutoTone, CompressorPreset, DeviceState, EqPreset, InputMode, MicPosition};

/// Output report size: report ID byte + 64 bytes of text.
pub const REPORT_SIZE: usize = 65;
const REPORT_ID: u8 = 0x00;
/// Reply to a rejected command, or to a SET that changed nothing.
pub const FAILED_REPLY: &str = "[Failed]";
/// Reply to a SET the device will not take right now, e.g. gain in Auto Level.
pub const LOCKED_REPLY: &str = "Locked";

/// Hardware gain step. The MV7 rounds any other value down to a multiple of 1.5 dB.
pub const GAIN_STEP_TENTHS: u16 = 15;
const MIC_MUTE: &str = "micMute";
const INPUT_GAIN: &str = "inputGain";
const DSP_MODE: &str = "dspMode";
const LOCK: &str = "lock";
const DIM_MODE: &str = "dimMode";
const METER_MODE: &str = "meterMode";
const FW_VERSION: &str = "fwVersion";
const SERIAL_NUM: &str = "serialNum";
const GET_BLOCK: &str = "getBlock";
const SET_BLOCK: &str = "setBlock";
const BLOCK_REPLY: &str = "block";
const ON: &str = "on";
const OFF: &str = "off";
const GAIN_UNIT: &str = "dB";

/// Compressor block: 0=Off, 1=Light, 2=Medium, 3=Heavy.
const BLOCK_COMPRESSOR: &str = "19";
/// Limiter block: 0=Off, 1=On (unconfirmed on hardware, see the module docs).
const BLOCK_LIMITER: &str = "1F";
/// Monitor mix block: two 32-bit linear gains, playback then mic.
const BLOCK_MONITOR_MIX: &str = "22";
/// EQ block: bit 0 = High Pass, bit 1 = Presence Boost.
const BLOCK_EQ: &str = "31";
const EQ_BIT_HIGH_PASS: u32 = 0x01;
const EQ_BIT_PRESENCE_BOOST: u32 = 0x02;
/// Hex digits in one 32-bit block word.
const WORD_HEX_LEN: usize = 8;

// dspMode folds Manual/Auto, mic position and tone into one number:
// 1 = Manual; Auto is Near 2–4 or Far 5–7, offset by tone Natural/Dark/Bright.
// Near+Natural (2) and Far 5/6/7 are confirmed against MOTIV. Near+Dark (3) and
// Near+Bright (4) follow the same layout but have not been checked in MOTIV.
const DSP_MODE_MANUAL: u8 = 1;
const DSP_MODE_NEAR: u8 = 2;
const DSP_MODE_FAR: u8 = 5;
const DSP_MODE_MAX: u8 = 7;

/// 0 dB in a monitor-mix gain word: `word = MIX_UNITY × 10^(dB / 20)` (0x004026E7).
const MIX_UNITY: f64 = 4_204_263.0;
/// Level of the fully attenuated side of the mix, in dB.
const MIX_FLOOR_DB: f64 = -54.0;
/// A mic level this far below 0 dB means the slider is past the centre.
const MIX_CENTRE_TOLERANCE_DB: f64 = 0.05;
/// Word rounding puts the floor a hair either side of -54 dB; anything this
/// close counts as the end of the slider.
const MIX_FLOOR_TOLERANCE_DB: f64 = 0.01;
/// MOTIV computes the mix curve on the host. These are the levels it wrote at
/// the slider positions measured on the device: (percent, playback dB, mic dB).
/// Positions in between are interpolated linearly in dB.
const MIX_CURVE: [(u8, f64, f64); 11] = [
    (0, -54.0, 0.0),
    (2, -54.0, 0.0),
    (10, -39.0, 0.0),
    (26, -19.5, 0.0),
    (40, -10.0, 0.0),
    (50, -6.0, 0.0),
    (60, -6.0, -6.0),
    (74, -6.0, -16.5),
    (90, -6.0, -39.0),
    (98, -6.0, -51.0),
    (100, -6.0, -54.0),
];

/// One command line and the prefix that identifies its reply.
#[derive(Debug, Clone, PartialEq)]
pub struct TextCommand {
    /// The command without its trailing newline, e.g. `"micMute on"`.
    pub line: String,
    /// Every reply to this command starts with this, e.g. `"micMute="`.
    pub reply_prefix: String,
}

impl TextCommand {
    fn named(name: &str, value: Option<&str>) -> Self {
        let line = match value {
            Some(value) => format!("{name} {value}"),
            None => name.to_string(),
        };
        Self {
            line,
            reply_prefix: format!("{name}="),
        }
    }

    fn block(id: &str, data: Option<&str>) -> Self {
        let line = match data {
            Some(data) => format!("{SET_BLOCK} {id} {data}"),
            None => format!("{GET_BLOCK} {id}"),
        };
        Self {
            line,
            reply_prefix: format!("{BLOCK_REPLY} {id} "),
        }
    }

    /// The HID Output report: report ID, the line, `\n`, zero padding.
    pub fn packet(&self) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(REPORT_SIZE);
        pkt.push(REPORT_ID);
        pkt.extend_from_slice(self.line.as_bytes());
        pkt.push(b'\n');
        // Lines are built from the constants above and are far shorter than a
        // report; resize() only pads (it would truncate, never panic).
        pkt.resize(REPORT_SIZE, 0x00);
        pkt
    }
}

fn on_off(enabled: bool) -> &'static str {
    if enabled { ON } else { OFF }
}

// ── GET ───────────────────────────────────────────────────────────────────

pub fn get_gain() -> TextCommand {
    TextCommand::named(INPUT_GAIN, None)
}

pub fn get_mute() -> TextCommand {
    TextCommand::named(MIC_MUTE, None)
}

pub fn get_dsp_mode() -> TextCommand {
    TextCommand::named(DSP_MODE, None)
}

pub fn get_monitor_mix() -> TextCommand {
    TextCommand::block(BLOCK_MONITOR_MIX, None)
}

pub fn get_serial() -> TextCommand {
    TextCommand::named(SERIAL_NUM, None)
}

/// Not in [`state_queries`]: an unanswered Limiter read must not fail the
/// readback, so `device.rs` sends it as an optional query.
pub fn get_limiter() -> TextCommand {
    TextCommand::block(BLOCK_LIMITER, None)
}

/// Every query needed for a full state readback.
pub fn state_queries() -> Vec<TextCommand> {
    vec![
        get_mute(),
        get_gain(),
        get_dsp_mode(),
        TextCommand::named(LOCK, None),
        TextCommand::named(METER_MODE, None),
        TextCommand::named(DIM_MODE, None),
        get_monitor_mix(),
        TextCommand::block(BLOCK_COMPRESSOR, None),
        TextCommand::block(BLOCK_EQ, None),
        TextCommand::named(FW_VERSION, None),
        get_serial(),
    ]
}

// ── SET ───────────────────────────────────────────────────────────────────

pub fn set_mute(muted: bool) -> TextCommand {
    TextCommand::named(MIC_MUTE, Some(on_off(muted)))
}

/// Clamp a gain to 0–36 dB and round it down to the 1.5 dB hardware grid,
/// which is what the device itself does with any other value.
pub fn snap_gain(gain_tenths: u16) -> u16 {
    super::DeviceModel::Mv7.snap_gain_tenths(gain_tenths)
}

pub fn set_gain(gain_tenths: u16) -> TextCommand {
    let snapped = snap_gain(gain_tenths);
    let value = match snapped % 10 {
        0 => (snapped / 10).to_string(),
        tenths => format!("{}.{tenths}", snapped / 10),
    };
    TextCommand::named(INPUT_GAIN, Some(&value))
}

/// Mode, mic position and tone are one setting on the MV7, so every change to
/// any of them sends all three.
pub fn set_dsp_mode(mode: InputMode, position: MicPosition, tone: AutoTone) -> TextCommand {
    // MOTIV zero-pads the mode number; the shell accepts it either way.
    let number = dsp_mode_number(mode, position, tone);
    TextCommand::named(DSP_MODE, Some(&format!("{number:02}")))
}

pub fn set_lock(locked: bool) -> TextCommand {
    TextCommand::named(LOCK, Some(on_off(locked)))
}

pub fn set_live_meter(enabled: bool) -> TextCommand {
    TextCommand::named(METER_MODE, Some(on_off(enabled)))
}

pub fn set_night_mode(enabled: bool) -> TextCommand {
    TextCommand::named(DIM_MODE, Some(on_off(enabled)))
}

pub fn set_limiter(enabled: bool) -> TextCommand {
    TextCommand::block(BLOCK_LIMITER, Some(&format!("{:08X}", u32::from(enabled))))
}

pub fn set_compressor(preset: CompressorPreset) -> TextCommand {
    let value = u32::from(preset.as_byte());
    TextCommand::block(BLOCK_COMPRESSOR, Some(&format!("{value:08X}")))
}

pub fn set_eq(preset: EqPreset) -> TextCommand {
    TextCommand::block(BLOCK_EQ, Some(&format!("{:08X}", eq_value(preset))))
}

/// The Monitor Mix the MV7 reads back after `mix` is set: 1% and 2% sit
/// on the 0% floor of `MIX_CURVE`.
pub fn monitor_mix_as_stored(mix: u8) -> u8 {
    let (playback, mic) = mix_words(mix);
    mix_from_words(playback, mic)
}

/// Monitor mix: 0 = full mic, 100 = full playback. Values above 100 are clamped.
pub fn set_monitor_mix(mix: u8) -> TextCommand {
    let (playback, mic) = mix_words(mix);
    TextCommand::block(BLOCK_MONITOR_MIX, Some(&format!("{playback:08X}{mic:08X}")))
}

// ── Preset loading ────────────────────────────────────────────────────────
//
// The MV7 answers [Failed] to a SET that changes nothing (seen for gain and
// Monitor Mix, assumed for the rest), so a preset load only sends what
// differs from the mic's current state.

/// The `dspMode` SET that takes the mic from `current` to `target`'s mode,
/// mic position and tone, or `None` if they already match. Changing it has
/// side effects (see the module docs), so read the state again after it.
pub fn mode_change(current: &DeviceState, target: &DeviceState) -> Option<TextCommand> {
    let number =
        |state: &DeviceState| dsp_mode_number(state.mode, state.auto_position, state.auto_tone);
    (number(current) != number(target))
        .then(|| set_dsp_mode(target.mode, target.auto_position, target.auto_tone))
}

/// The SETs, after the mode, that take the mic from `current` (read back
/// after any mode change) to `target`, each with the setting's label for a
/// refusal message. Gain, compressor and EQ only apply in Manual, and the
/// Limiter is assumed to as well (unconfirmed), so they are left out unless
/// both are in Manual. Monitor Mix goes last and only if `monitor_mix`.
/// Values are compared as the commands that would set them, so two gains
/// or mixes the mic can't tell apart count as equal.
pub fn setting_changes(
    current: &DeviceState,
    target: &DeviceState,
    monitor_mix: bool,
) -> Vec<(&'static str, TextCommand)> {
    let mut changes = Vec::new();
    let mut add = |label: &'static str, set: fn(&DeviceState) -> TextCommand| {
        let to = set(target);
        if set(current) != to {
            changes.push((label, to));
        }
    };
    if target.mode == InputMode::Manual && current.mode == InputMode::Manual {
        add("Gain", |state| set_gain(state.gain_tenths));
        // Only when the mic's Limiter was read, so an unknown one isn't written.
        if current.limiter_enabled.is_some() && target.limiter_enabled.is_some() {
            add("Limiter", |state| {
                set_limiter(state.limiter_enabled.unwrap_or_default())
            });
        }
        add("Compressor", |state| set_compressor(state.compressor));
        add("EQ", |state| set_eq(state.eq_preset));
    }
    add("Live Meter", |state| set_live_meter(state.led_live_meter));
    add("Night Mode", |state| set_night_mode(state.led_night_mode));
    if monitor_mix {
        add("Monitor Mix", |state| set_monitor_mix(state.monitor_mix));
    }
    changes
}

// ── Encoding helpers ──────────────────────────────────────────────────────

fn dsp_mode_number(mode: InputMode, position: MicPosition, tone: AutoTone) -> u8 {
    match mode {
        InputMode::Manual => DSP_MODE_MANUAL,
        InputMode::Auto => {
            let base = match position {
                MicPosition::Near => DSP_MODE_NEAR,
                MicPosition::Far => DSP_MODE_FAR,
            };
            let offset = match tone {
                AutoTone::Natural => 0,
                AutoTone::Dark => 1,
                AutoTone::Bright => 2,
            };
            base + offset
        }
    }
}

fn apply_dsp_mode(number: u8, state: &mut DeviceState) -> bool {
    if number == DSP_MODE_MANUAL {
        state.mode = InputMode::Manual;
        return true;
    }
    if !(DSP_MODE_NEAR..=DSP_MODE_MAX).contains(&number) {
        return false;
    }
    let (position, offset) = if number >= DSP_MODE_FAR {
        (MicPosition::Far, number - DSP_MODE_FAR)
    } else {
        (MicPosition::Near, number - DSP_MODE_NEAR)
    };
    let tone = match offset {
        0 => AutoTone::Natural,
        1 => AutoTone::Dark,
        2 => AutoTone::Bright,
        _ => return false,
    };
    state.mode = InputMode::Auto;
    state.auto_position = position;
    state.auto_tone = tone;
    true
}

fn eq_value(preset: EqPreset) -> u32 {
    match preset {
        EqPreset::Flat => 0,
        EqPreset::HighPass => EQ_BIT_HIGH_PASS,
        EqPreset::PresenceBoost => EQ_BIT_PRESENCE_BOOST,
        EqPreset::HighPassPresenceBoost => EQ_BIT_HIGH_PASS | EQ_BIT_PRESENCE_BOOST,
    }
}

fn eq_from_value(value: u32) -> EqPreset {
    match (
        value & EQ_BIT_HIGH_PASS != 0,
        value & EQ_BIT_PRESENCE_BOOST != 0,
    ) {
        (false, false) => EqPreset::Flat,
        (true, false) => EqPreset::HighPass,
        (false, true) => EqPreset::PresenceBoost,
        (true, true) => EqPreset::HighPassPresenceBoost,
    }
}

fn db_to_word(db: f64) -> u32 {
    (MIX_UNITY * 10f64.powf(db / 20.0)).round() as u32
}

fn word_to_db(word: u32) -> f64 {
    if word == 0 {
        f64::NEG_INFINITY
    } else {
        20.0 * (f64::from(word) / MIX_UNITY).log10()
    }
}

/// Interpolated (playback dB, mic dB) for a mix percentage.
fn mix_levels_db(mix: u8) -> (f64, f64) {
    let mix = mix.min(100);
    for pair in MIX_CURVE.windows(2) {
        let (p0, playback0, mic0) = pair[0];
        let (p1, playback1, mic1) = pair[1];
        if mix <= p1 {
            let t = f64::from(mix - p0) / f64::from(p1 - p0);
            return (
                playback0 + t * (playback1 - playback0),
                mic0 + t * (mic1 - mic0),
            );
        }
    }
    let (_, playback, mic) = MIX_CURVE[MIX_CURVE.len() - 1];
    (playback, mic)
}

fn mix_words(mix: u8) -> (u32, u32) {
    let (playback_db, mic_db) = mix_levels_db(mix);
    (db_to_word(playback_db), db_to_word(mic_db))
}

/// The slider position at which `level` (one channel of the curve) equals
/// `level_db`, or `None` if the curve never reaches it.
fn invert_curve(level_db: f64, level: fn(&(u8, f64, f64)) -> f64) -> Option<u8> {
    MIX_CURVE.windows(2).find_map(|pair| {
        let (y0, y1) = (level(&pair[0]), level(&pair[1]));
        let (low, high) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
        if y0 == y1 || level_db < low || level_db > high {
            return None;
        }
        let t = (level_db - y0) / (y1 - y0);
        let position = f64::from(pair[0].0) + t * f64::from(pair[1].0 - pair[0].0);
        Some(position.round() as u8)
    })
}

/// Decode a monitor-mix block back to the nearest slider percentage.
fn mix_from_words(playback: u32, mic: u32) -> u8 {
    let at_floor = |db: f64| db <= MIX_FLOOR_DB + MIX_FLOOR_TOLERANCE_DB;
    let mic_db = word_to_db(mic);
    if mic_db < -MIX_CENTRE_TOLERANCE_DB {
        // Mic is attenuated: the slider is between the centre and Playback.
        if at_floor(mic_db) {
            return 100;
        }
        return invert_curve(mic_db, |point| point.2).unwrap_or(50);
    }
    let playback_db = word_to_db(playback);
    if at_floor(playback_db) {
        return 0;
    }
    invert_curve(playback_db, |point| point.1).unwrap_or(50)
}

// ── Reply decoding ────────────────────────────────────────────────────────

/// Find the reply to a command in the text read so far: the first complete
/// line that starts with `reply_prefix` or is [`FAILED_REPLY`] or
/// [`LOCKED_REPLY`]. Other lines (help text, the gain line a mode change
/// sends first) are skipped. Returns `None` until it arrives.
pub fn find_reply<'a>(text: &'a str, reply_prefix: &str) -> Option<&'a str> {
    let complete = &text[..text.rfind('\n')?];
    complete
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .find(|line| {
            line.starts_with(reply_prefix) || *line == FAILED_REPLY || *line == LOCKED_REPLY
        })
}

/// Decode one reply line into `state`. Returns `false` if the line is not a
/// recognised reply or its value does not parse.
#[must_use]
pub fn apply_reply(line: &str, state: &mut DeviceState) -> bool {
    if let Some(block) = line
        .strip_prefix(BLOCK_REPLY)
        .and_then(|rest| rest.strip_prefix(' '))
    {
        return match block.split_once(' ') {
            Some((id, data)) => apply_block(id, data, state),
            None => false,
        };
    }
    let Some((key, value)) = line.split_once('=') else {
        return false;
    };
    if value.is_empty() {
        return false;
    }
    match key {
        MIC_MUTE => apply_on_off(value, &mut state.muted),
        INPUT_GAIN => match parse_gain(value) {
            Some(gain_tenths) => {
                state.gain_tenths = gain_tenths;
                true
            }
            None => false,
        },
        DSP_MODE => value
            .parse::<u8>()
            .is_ok_and(|number| apply_dsp_mode(number, state)),
        LOCK => apply_on_off(value, &mut state.locked),
        METER_MODE => apply_on_off(value, &mut state.led_live_meter),
        DIM_MODE => apply_on_off(value, &mut state.led_night_mode),
        FW_VERSION => {
            state.firmware_version = value.to_string();
            true
        }
        SERIAL_NUM => {
            state.factory_serial = value.to_string();
            true
        }
        _ => false,
    }
}

fn apply_on_off(value: &str, field: &mut bool) -> bool {
    match value {
        ON => *field = true,
        OFF => *field = false,
        _ => return false,
    }
    true
}

/// Parse `"19.50dB"` or `"36dB"` into tenths of a dB.
fn parse_gain(value: &str) -> Option<u16> {
    let number = value.strip_suffix(GAIN_UNIT)?;
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    let tenths = match fraction.chars().next() {
        Some(digit) => u16::try_from(digit.to_digit(10)?).ok()?,
        None => 0,
    };
    whole
        .parse::<u16>()
        .ok()?
        .checked_mul(10)?
        .checked_add(tenths)
}

fn parse_word(hex: &str) -> Option<u32> {
    if hex.len() != WORD_HEX_LEN {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

fn apply_block(id: &str, data: &str, state: &mut DeviceState) -> bool {
    match id {
        BLOCK_COMPRESSOR => match parse_word(data).and_then(|v| u8::try_from(v).ok()) {
            Some(value) if value <= CompressorPreset::Heavy.as_byte() => {
                state.compressor = CompressorPreset::from_byte(value);
                true
            }
            _ => false,
        },
        BLOCK_EQ => match parse_word(data) {
            Some(value) => {
                state.eq_preset = eq_from_value(value);
                true
            }
            None => false,
        },
        BLOCK_LIMITER => match parse_word(data) {
            Some(value @ (0 | 1)) => {
                state.limiter_enabled = Some(value == 1);
                true
            }
            _ => false,
        },
        BLOCK_MONITOR_MIX => {
            let words = (
                data.get(..WORD_HEX_LEN).and_then(parse_word),
                data.get(WORD_HEX_LEN..).and_then(parse_word),
            );
            match words {
                (Some(playback), Some(mic)) => {
                    state.monitor_mix = mix_from_words(playback, mic);
                    true
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// A stand-in for the MV7 command shell, for the fake device in `fake_hid.rs`.
/// It answers the commands shurectl sends the way the protocol notes describe:
/// `name=value`, `block <id> <data>`, `[Failed]` for an unknown command or a
/// gain that changes nothing, and `Locked` for a gain in Auto Level. It does
/// not model the side effects of `dspMode` (EQ and compressor resets).
#[cfg(any(test, feature = "qa"))]
pub mod shell {
    use std::collections::BTreeMap;

    use super::{
        BLOCK_COMPRESSOR, BLOCK_EQ, BLOCK_LIMITER, BLOCK_MONITOR_MIX, BLOCK_REPLY, DIM_MODE,
        DSP_MODE, DSP_MODE_MANUAL, DSP_MODE_MAX, FAILED_REPLY, FW_VERSION, GAIN_UNIT, GET_BLOCK,
        INPUT_GAIN, LOCK, LOCKED_REPLY, METER_MODE, MIC_MUTE, OFF, ON, SERIAL_NUM, SET_BLOCK,
        parse_gain, set_compressor, set_dsp_mode, set_eq, set_gain, set_limiter, set_live_meter,
        set_lock, set_monitor_mix, set_mute, set_night_mode, snap_gain,
    };
    use crate::protocol::DeviceState;

    pub struct Shell {
        /// Reply values by command name (`micMute`) or block (`block 19`).
        values: BTreeMap<String, String>,
    }

    impl Shell {
        /// A shell holding `state`, stored through the same SET commands
        /// `device.rs` sends.
        pub fn new(state: &DeviceState) -> Self {
            let mut shell = Self {
                values: BTreeMap::new(),
            };
            for command in [
                set_mute(state.muted),
                set_gain(state.gain_tenths),
                set_dsp_mode(state.mode, state.auto_position, state.auto_tone),
                set_lock(state.locked),
                set_live_meter(state.led_live_meter),
                set_night_mode(state.led_night_mode),
                set_compressor(state.compressor),
                set_eq(state.eq_preset),
                set_monitor_mix(state.monitor_mix),
            ] {
                shell.store(&command.line);
            }
            // An unread Limiter is a mic that doesn't answer its block.
            if let Some(enabled) = state.limiter_enabled {
                shell.store(&set_limiter(enabled).line);
            }
            shell
                .values
                .insert(FW_VERSION.into(), state.firmware_version.clone());
            shell
                .values
                .insert(SERIAL_NUM.into(), state.factory_serial.clone());
            shell
        }

        /// Store a SET line without the checks `answer()` makes.
        fn store(&mut self, line: &str) {
            // Seeding a locked Auto Level state must still store the gain.
            let mode = self.values.remove(DSP_MODE);
            self.answer(line);
            if let Some(mode) = mode {
                self.values.entry(DSP_MODE.into()).or_insert(mode);
            }
        }

        /// The reply line to one command line (without its newline).
        pub fn answer(&mut self, line: &str) -> String {
            let (name, arg) = match line.split_once(' ') {
                Some((name, arg)) => (name, Some(arg)),
                None => (line, None),
            };
            match (name, arg) {
                (GET_BLOCK, Some(id)) => self.block_reply(id),
                (SET_BLOCK, Some(arg)) => match arg.split_once(' ') {
                    // As for gain, the mic answers [Failed] to a block SET
                    // that changes nothing (seen for Monitor Mix, assumed for
                    // the rest), such as Monitor Mix's 0–2% floor.
                    Some((id, data))
                        if self.values.get(&format!("{BLOCK_REPLY} {id}"))
                            == Some(&data.to_string()) =>
                    {
                        FAILED_REPLY.into()
                    }
                    Some((id, data))
                        if [BLOCK_COMPRESSOR, BLOCK_LIMITER, BLOCK_EQ, BLOCK_MONITOR_MIX]
                            .contains(&id) =>
                    {
                        self.values
                            .insert(format!("{BLOCK_REPLY} {id}"), data.into());
                        self.block_reply(id)
                    }
                    // setBlock with unmapped data is not safe on hardware.
                    _ => FAILED_REPLY.into(),
                },
                (name, None) => match self.values.get(name) {
                    Some(value) => format!("{name}={value}"),
                    None => FAILED_REPLY.into(),
                },
                (INPUT_GAIN, Some(value)) => self.set_gain(value),
                (DSP_MODE, Some(value)) => match value.parse::<u8>() {
                    Ok(number) if (DSP_MODE_MANUAL..=DSP_MODE_MAX).contains(&number) => {
                        self.set_value(DSP_MODE, value)
                    }
                    _ => FAILED_REPLY.into(),
                },
                (MIC_MUTE | LOCK | METER_MODE | DIM_MODE, Some(value @ (ON | OFF))) => {
                    self.set_value(name, value)
                }
                (_, Some(_)) => FAILED_REPLY.into(),
            }
        }

        fn set_value(&mut self, name: &str, value: &str) -> String {
            self.values.insert(name.into(), value.into());
            format!("{name}={value}")
        }

        fn block_reply(&self, id: &str) -> String {
            match self.values.get(&format!("{BLOCK_REPLY} {id}")) {
                Some(data) => format!("{BLOCK_REPLY} {id} {data}"),
                None => FAILED_REPLY.into(),
            }
        }

        fn set_gain(&mut self, value: &str) -> String {
            let Some(gain_tenths) = parse_gain(&format!("{value}{GAIN_UNIT}")) else {
                return FAILED_REPLY.into();
            };
            let manual = self
                .values
                .get(DSP_MODE)
                .and_then(|mode| mode.parse::<u8>().ok())
                .is_none_or(|mode| mode == DSP_MODE_MANUAL);
            if !manual {
                return LOCKED_REPLY.into();
            }
            let snapped = snap_gain(gain_tenths);
            let reply = format!("{}.{}0{GAIN_UNIT}", snapped / 10, snapped % 10);
            if self.values.get(INPUT_GAIN) == Some(&reply) {
                return FAILED_REPLY.into();
            }
            self.set_value(INPUT_GAIN, &reply)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every (mode, position, tone) combination the UI can produce.
    fn all_dsp_modes() -> Vec<(InputMode, MicPosition, AutoTone)> {
        let mut modes = vec![(InputMode::Manual, MicPosition::Near, AutoTone::Natural)];
        for position in [MicPosition::Near, MicPosition::Far] {
            for tone in [AutoTone::Dark, AutoTone::Natural, AutoTone::Bright] {
                modes.push((InputMode::Auto, position, tone));
            }
        }
        modes
    }

    #[test]
    fn packet_is_report_id_zero_then_line_then_newline_padded_to_65() {
        let pkt = set_mute(true).packet();
        assert_eq!(pkt.len(), REPORT_SIZE);
        assert_eq!(pkt[0], 0x00);
        assert_eq!(&pkt[1..12], b"micMute on\n");
        assert!(pkt[12..].iter().all(|&b| b == 0));
    }

    #[test]
    fn every_command_fits_in_one_report() {
        let mut commands = state_queries();
        commands.extend([
            set_monitor_mix(37),
            set_gain(360),
            set_eq(EqPreset::HighPassPresenceBoost),
        ]);
        for command in commands {
            assert!(
                command.line.len() + 2 <= REPORT_SIZE,
                "{} is too long",
                command.line
            );
        }
    }

    #[test]
    fn set_lines_match_what_motiv_sends() {
        assert_eq!(set_mute(false).line, "micMute off");
        assert_eq!(set_lock(true).line, "lock on");
        assert_eq!(set_live_meter(false).line, "meterMode off");
        assert_eq!(set_night_mode(true).line, "dimMode on");
        assert_eq!(
            set_compressor(CompressorPreset::Light).line,
            "setBlock 19 00000001"
        );
        assert_eq!(
            set_compressor(CompressorPreset::Heavy).line,
            "setBlock 19 00000003"
        );
        assert_eq!(set_eq(EqPreset::Flat).line, "setBlock 31 00000000");
        assert_eq!(set_eq(EqPreset::PresenceBoost).line, "setBlock 31 00000002");
        assert_eq!(
            set_dsp_mode(InputMode::Manual, MicPosition::Far, AutoTone::Bright).line,
            "dspMode 01"
        );
        assert_eq!(
            set_dsp_mode(InputMode::Auto, MicPosition::Far, AutoTone::Dark).line,
            "dspMode 06"
        );
    }

    /// Block 1F and its values are from Shure's DSP-Settings-MV7.xml, not a
    /// capture: nothing here proves the mic acts on them.
    #[test]
    fn limiter_roundtrip() {
        assert_eq!(set_limiter(true).line, "setBlock 1F 00000001");
        assert_eq!(set_limiter(false).line, "setBlock 1F 00000000");
        assert_eq!(get_limiter().line, "getBlock 1F");
        let mut state = DeviceState {
            limiter_enabled: None,
            ..DeviceState::default()
        };
        assert!(apply_reply("block 1F 00000001", &mut state));
        assert_eq!(state.limiter_enabled, Some(true));
        assert!(apply_reply("block 1F 00000000", &mut state));
        assert_eq!(state.limiter_enabled, Some(false));
        for unusable in ["block 1F 00000002", "block 1F Not valid"] {
            assert!(!apply_reply(unusable, &mut state), "{unusable}");
        }
        assert_eq!(state.limiter_enabled, Some(false));
    }

    #[test]
    fn reply_prefixes_identify_named_and_block_replies() {
        assert_eq!(set_mute(true).reply_prefix, "micMute=");
        assert_eq!(set_eq(EqPreset::Flat).reply_prefix, "block 31 ");
        assert_eq!(get_gain().reply_prefix, "inputGain=");
    }

    #[test]
    fn set_gain_snaps_down_to_the_hardware_grid() {
        // Observed on the device: 25 → 24, 20 → 19.5, 10 → 9, 35.5 → 34.5.
        assert_eq!(set_gain(250).line, "inputGain 24");
        assert_eq!(set_gain(200).line, "inputGain 19.5");
        assert_eq!(set_gain(100).line, "inputGain 9");
        assert_eq!(set_gain(355).line, "inputGain 34.5");
        assert_eq!(set_gain(285).line, "inputGain 28.5");
        assert_eq!(set_gain(0).line, "inputGain 0");
    }

    #[test]
    fn set_gain_clamps_to_36_db() {
        assert_eq!(set_gain(600).line, "inputGain 36");
        assert_eq!(snap_gain(u16::MAX), 360);
    }

    #[test]
    fn monitor_mix_encodes_to_the_words_motiv_wrote() {
        // Captured with getBlock 22 after moving MOTIV's slider.
        let captured = [
            (0, "000020C5004026E7"),
            (10, "0000B845004026E7"),
            (26, "0006CB9A004026E7"),
            (40, "00144961004026E7"),
            (50, "002026F3004026E7"),
            (60, "002026F3002026F3"),
            (74, "002026F300099941"),
            (90, "002026F30000B845"),
            (98, "002026F300002E49"),
            (100, "002026F3000020C5"),
        ];
        for (mix, data) in captured {
            assert_eq!(
                set_monitor_mix(mix).line,
                format!("setBlock 22 {data}"),
                "mix {mix}%"
            );
        }
    }

    #[test]
    fn monitor_mix_clamps_above_100() {
        assert_eq!(set_monitor_mix(250), set_monitor_mix(100));
    }

    #[test]
    fn monitor_mix_decodes_motiv_captures() {
        // Includes MOTIV's 2% value, which it writes one count below the floor.
        let captured = [
            ("000020C5004026E7", 0),
            ("000020C4004026E7", 0),
            ("0000B845004026E7", 10),
            ("0006CB9A004026E7", 26),
            ("00144961004026E7", 40),
            ("002026F3004026E7", 50),
            ("002026F3002026F3", 60),
            ("002026F300099941", 74),
            ("002026F30000B845", 90),
            ("002026F300002E49", 98),
            ("002026F3000020C5", 100),
        ];
        for (data, mix) in captured {
            let mut state = DeviceState::default();
            assert!(apply_reply(&format!("block 22 {data}"), &mut state));
            assert_eq!(state.monitor_mix, mix, "block 22 {data}");
        }
    }

    #[test]
    fn monitor_mix_roundtrips_outside_the_floor_plateau() {
        // 0–2% all sit on the floor, so 1% and 2% read back as 0%.
        for mix in (0..=100u8).filter(|mix| !(1..=2).contains(mix)) {
            let (playback, mic) = mix_words(mix);
            assert_eq!(mix_from_words(playback, mic), mix, "mix {mix}%");
        }
    }

    #[test]
    fn dsp_mode_roundtrips_for_every_combination() {
        for (mode, position, tone) in all_dsp_modes() {
            let command = set_dsp_mode(mode, position, tone);
            let number = command.line.trim_start_matches("dspMode ");
            let mut state = DeviceState::default();
            assert!(apply_reply(
                &format!("dspMode={}", number.trim_start_matches('0')),
                &mut state
            ));
            assert_eq!(state.mode, mode, "{}", command.line);
            if mode == InputMode::Auto {
                assert_eq!(state.auto_position, position, "{}", command.line);
                assert_eq!(state.auto_tone, tone, "{}", command.line);
            }
        }
    }

    #[test]
    fn dsp_mode_decodes_values_confirmed_in_motiv() {
        let confirmed = [
            (2, MicPosition::Near, AutoTone::Natural),
            (5, MicPosition::Far, AutoTone::Natural),
            (6, MicPosition::Far, AutoTone::Dark),
            (7, MicPosition::Far, AutoTone::Bright),
        ];
        for (number, position, tone) in confirmed {
            let mut state = DeviceState::default();
            assert!(apply_reply(&format!("dspMode={number}"), &mut state));
            assert_eq!(state.mode, InputMode::Auto);
            assert_eq!((state.auto_position, state.auto_tone), (position, tone));
        }
        let mut state = DeviceState::default();
        assert!(apply_reply("dspMode=1", &mut state));
        assert_eq!(state.mode, InputMode::Manual);
    }

    #[test]
    fn dsp_mode_rejects_out_of_range_values() {
        let mut state = DeviceState::default();
        for reply in ["dspMode=0", "dspMode=8", "dspMode=x"] {
            assert!(!apply_reply(reply, &mut state), "{reply}");
        }
        assert_eq!(state, DeviceState::default());
    }

    #[test]
    fn apply_reply_decodes_named_values() {
        let mut state = DeviceState::default();
        assert!(apply_reply("micMute=on", &mut state));
        assert!(state.muted);
        assert!(apply_reply("inputGain=19.50dB", &mut state));
        assert_eq!(state.gain_tenths, 195);
        assert!(apply_reply("inputGain=36dB", &mut state));
        assert_eq!(state.gain_tenths, 360);
        assert!(apply_reply("lock=on", &mut state));
        assert!(state.locked);
        assert!(apply_reply("meterMode=off", &mut state));
        assert!(!state.led_live_meter);
        assert!(apply_reply("dimMode=on", &mut state));
        assert!(state.led_night_mode);
        assert!(apply_reply("fwVersion=0.0.52.0", &mut state));
        assert_eq!(state.firmware_version, "0.0.52.0");
        assert!(apply_reply("serialNum=2DDC3002222E2B00", &mut state));
        assert_eq!(state.factory_serial, "2DDC3002222E2B00");
    }

    #[test]
    fn apply_reply_decodes_blocks() {
        let mut state = DeviceState::default();
        assert!(apply_reply("block 19 00000002", &mut state));
        assert_eq!(state.compressor, CompressorPreset::Medium);
        assert!(apply_reply("block 31 00000003", &mut state));
        assert_eq!(state.eq_preset, EqPreset::HighPassPresenceBoost);
        assert!(apply_reply("block 31 00000001", &mut state));
        assert_eq!(state.eq_preset, EqPreset::HighPass);
    }

    #[test]
    fn eq_and_compressor_roundtrip() {
        for preset in [
            EqPreset::Flat,
            EqPreset::HighPass,
            EqPreset::PresenceBoost,
            EqPreset::HighPassPresenceBoost,
        ] {
            let reply = set_eq(preset).line.replacen("setBlock", "block", 1);
            let mut state = DeviceState::default();
            assert!(apply_reply(&reply, &mut state));
            assert_eq!(state.eq_preset, preset);
        }
        for preset in [
            CompressorPreset::Off,
            CompressorPreset::Light,
            CompressorPreset::Medium,
            CompressorPreset::Heavy,
        ] {
            let reply = set_compressor(preset).line.replacen("setBlock", "block", 1);
            let mut state = DeviceState::default();
            assert!(apply_reply(&reply, &mut state));
            assert_eq!(state.compressor, preset);
        }
    }

    #[test]
    fn apply_reply_rejects_malformed_lines_without_mutating() {
        let mut state = DeviceState::default();
        for reply in [
            "",
            "[Failed]",
            "micMute=maybe",
            "micMute=",
            "inputGain=loud",
            "inputGain=12",
            "block 19 00000004",
            "block 19 1",
            "block 22 002026F3",
            "block 22 002026F3004026E7FF",
            "block 22 ééééééééééééééé",
            "block 99 00000000",
            "unknownKey=on",
            "No such command: x",
        ] {
            assert!(!apply_reply(reply, &mut state), "{reply:?}");
        }
        assert_eq!(state, DeviceState::default());
    }

    #[test]
    fn find_reply_skips_unrelated_lines_and_waits_for_a_full_line() {
        assert_eq!(find_reply("micMute=o", "micMute="), None);
        assert_eq!(find_reply("micMute=on\n", "micMute="), Some("micMute=on"));
        assert_eq!(
            find_reply("dimMode=on\nblock 22 00\nmicMute=off\n", "micMute="),
            Some("micMute=off")
        );
        assert_eq!(
            find_reply(
                "No such command: x\nEnter 'help' or '?' for help.\n[Failed]\n",
                "x="
            ),
            Some(FAILED_REPLY)
        );
        assert_eq!(find_reply("Locked\n", "inputGain="), Some(LOCKED_REPLY));
        // A mode change reports the gain first, then the mode.
        assert_eq!(
            find_reply("inputGain=31.50dB\ndspMode=1\n", "dspMode="),
            Some("dspMode=1")
        );
    }

    fn manual_state() -> DeviceState {
        DeviceState {
            mode: InputMode::Manual,
            gain_tenths: 195,
            monitor_mix: 40,
            ..DeviceState::default()
        }
    }

    fn labels(changes: &[(&'static str, TextCommand)]) -> Vec<&'static str> {
        changes.iter().map(|(label, _)| *label).collect()
    }

    #[test]
    fn mode_change_only_when_the_dsp_mode_differs() {
        let manual = manual_state();
        let mut target = manual.clone();
        // Position and tone don't exist in Manual.
        target.auto_position = MicPosition::Far;
        target.auto_tone = AutoTone::Bright;
        assert_eq!(mode_change(&manual, &target), None);

        target.mode = InputMode::Auto;
        assert_eq!(
            mode_change(&manual, &target),
            Some(set_dsp_mode(
                InputMode::Auto,
                MicPosition::Far,
                AutoTone::Bright
            ))
        );
        let mut far_dark = target.clone();
        far_dark.auto_tone = AutoTone::Dark;
        assert!(mode_change(&target, &far_dark).is_some());
        assert_eq!(mode_change(&target, &target), None);
    }

    #[test]
    fn setting_changes_is_empty_for_a_matching_preset() {
        let state = manual_state();
        assert!(setting_changes(&state, &state, true).is_empty());
    }

    #[test]
    fn setting_changes_lists_only_what_differs_in_load_order() {
        let current = manual_state();
        let mut target = current.clone();
        target.monitor_mix = 74;
        // Mute is live state, which a preset load leaves alone.
        target.muted = true;
        target.eq_preset = EqPreset::HighPass;
        target.led_night_mode = true;
        let changes = setting_changes(&current, &target, true);
        assert_eq!(labels(&changes), ["EQ", "Night Mode", "Monitor Mix"]);
        assert_eq!(changes[2].1, set_monitor_mix(74));
        // A Monitor Mix of the other kind is left alone.
        assert_eq!(
            labels(&setting_changes(&current, &target, false)),
            ["EQ", "Night Mode"]
        );
    }

    #[test]
    fn setting_changes_skips_manual_settings_in_auto() {
        let current = manual_state();
        let mut target = current.clone();
        target.mode = InputMode::Auto;
        target.gain_tenths = 300;
        target.compressor = CompressorPreset::Heavy;
        target.eq_preset = EqPreset::PresenceBoost;
        assert!(setting_changes(&current, &target, true).is_empty());
    }

    #[test]
    fn manual_settings_wait_for_a_mic_still_in_auto() {
        // The mode change to Manual was refused, so the mic is still in Auto.
        let mut current = manual_state();
        current.mode = InputMode::Auto;
        let mut target = manual_state();
        target.gain_tenths = 300;
        target.compressor = CompressorPreset::Heavy;
        target.led_night_mode = true;
        assert_eq!(
            labels(&setting_changes(&current, &target, true)),
            ["Night Mode"]
        );
    }

    #[test]
    fn setting_changes_treats_values_the_mic_cannot_tell_apart_as_equal() {
        let mut current = manual_state();
        current.monitor_mix = 0;
        let mut target = current.clone();
        // 20 dB snaps down to the 19.5 dB the mic has; 0–2% all sit on the floor.
        target.gain_tenths = 200;
        target.monitor_mix = 2;
        assert!(setting_changes(&current, &target, true).is_empty());
        target.gain_tenths = 210;
        assert_eq!(labels(&setting_changes(&current, &target, true)), ["Gain"]);
    }
}
