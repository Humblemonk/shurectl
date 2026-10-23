//! `--json` and `--set KEY=VALUE`: the whole device as JSON, and every setting
//! changeable from a script, for status bars and desktop widgets.
//!
//! `--json` prints one JSON object and exits. `--set` applies each KEY=VALUE in
//! order through the same `apply_action()` the TUI uses, then prints the JSON.
//! `--json --follow [MS]` keeps the device open, prints the JSON again whenever
//! a setting changes (the mic's own mute button, the MOTIV app), and reads
//! KEY=VALUE lines from stdin so a widget can drive one process instead of
//! racing a second one for the same HID device.

use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::app::{App, DeviceAction};
use crate::device::ShureDevice;
use crate::presets;
use crate::protocol::{
    AutoGain, AutoTone, CompressorPreset, DeviceModel, DeviceState, EqPreset, HpfFrequency,
    InputMode, LedBehavior, LedBrightness, LedLiveTheme, LedPulsingTheme, LedSolidTheme,
    MicPosition, MicPreset, ReverbType,
};

// ── Names used on both sides: `--set KEY=VALUE` input and JSON output ──────────

const MODES: &[(InputMode, &str)] = &[(InputMode::Auto, "auto"), (InputMode::Manual, "manual")];
const POSITIONS: &[(MicPosition, &str)] = &[(MicPosition::Near, "near"), (MicPosition::Far, "far")];
const AUTO_TONES: &[(AutoTone, &str)] = &[
    (AutoTone::Dark, "dark"),
    (AutoTone::Natural, "natural"),
    (AutoTone::Bright, "bright"),
];
const AUTO_GAINS: &[(AutoGain, &str)] = &[
    (AutoGain::Quiet, "quiet"),
    (AutoGain::Normal, "normal"),
    (AutoGain::Loud, "loud"),
];
const MIC_PRESETS: &[(MicPreset, &str)] = &[
    (MicPreset::Speech, "speech"),
    (MicPreset::Singing, "singing"),
    (MicPreset::Instrument, "instrument"),
];
const COMPRESSORS: &[(CompressorPreset, &str)] = &[
    (CompressorPreset::Off, "off"),
    (CompressorPreset::Light, "light"),
    (CompressorPreset::Medium, "medium"),
    (CompressorPreset::Heavy, "heavy"),
];
const HPFS: &[(HpfFrequency, &str)] = &[
    (HpfFrequency::Off, "off"),
    (HpfFrequency::Hz75, "75"),
    (HpfFrequency::Hz150, "150"),
];
const EQ_PRESETS: &[(EqPreset, &str)] = &[
    (EqPreset::Flat, "flat"),
    (EqPreset::HighPass, "high_pass"),
    (EqPreset::PresenceBoost, "presence_boost"),
    (EqPreset::HighPassPresenceBoost, "high_pass_presence_boost"),
];
const REVERBS: &[(ReverbType, &str)] = &[
    (ReverbType::Plate, "plate"),
    (ReverbType::Hall, "hall"),
    (ReverbType::Studio, "studio"),
];
const LED_BEHAVIORS: &[(LedBehavior, &str)] = &[
    (LedBehavior::Live, "live"),
    (LedBehavior::Pulsing, "pulsing"),
    (LedBehavior::Solid, "solid"),
];
const LED_BRIGHTNESSES: &[(LedBrightness, &str)] = &[
    (LedBrightness::Low, "low"),
    (LedBrightness::Med, "med"),
    (LedBrightness::High, "high"),
    (LedBrightness::Max, "max"),
];
const LED_LIVE_THEMES: &[(LedLiveTheme, &str)] = &[
    (LedLiveTheme::Default, "default"),
    (LedLiveTheme::Seaside, "seaside"),
    (LedLiveTheme::Space, "space"),
    (LedLiveTheme::Fruity, "fruity"),
    (LedLiveTheme::Custom, "custom"),
];
const LED_SOLID_THEMES: &[(LedSolidTheme, &str)] = &[
    (LedSolidTheme::Shure, "shure"),
    (LedSolidTheme::Custom, "custom"),
];
const LED_PULSING_THEMES: &[(LedPulsingTheme, &str)] = &[
    (LedPulsingTheme::Shure, "shure"),
    (LedPulsingTheme::Custom, "custom"),
];

/// The keys `--set` accepts, for `--set help` and error messages.
pub const KEYS: &str = "\
gain=<dB>  mode=auto|manual  auto_position=near|far  auto_tone=dark|natural|bright
auto_gain=quiet|normal|loud  mic_preset=speech|singing|instrument
mute=on|off|toggle  gain_lock=<bool>  lock=<bool>  mute_button_disabled=<bool>
monitor_mix=0-100  playback_mix=0-100
hpf=off|75|150  compressor=off|light|medium|heavy  limiter=<bool>
denoiser=<bool>  denoiser_level=1-100  popper_stopper=<bool>  tone=-10..10
tone_mode=simple|advanced  adaptation_rate=slow|fast  phantom=<bool>
eq=<bool>  eq_band_1..5=<dB>  eq_band_1..5_enabled=<bool>  eq_flatten=1
eq_preset=flat|high_pass|presence_boost|high_pass_presence_boost
led_live_meter=<bool>  led_night_mode=<bool>
reverb_output=<bool>  reverb_monitoring=<bool>  reverb_type=plate|hall|studio  reverb_intensity=0-100
led_behavior=live|pulsing|solid  led_brightness=low|med|high|max
led_live_theme=default|seaside|space|fruity|custom  led_solid_theme=shure|custom  led_pulsing_theme=shure|custom
led_solid_rgb=#rrggbb  led_pulsing_rgb=#rrggbb  led_live_edge_rgb=#rrggbb  led_live_middle_rgb=#rrggbb  led_live_interior_rgb=#rrggbb
device_name=<name>  preset_save=<1-4>[:name]  preset_load=<1-4>  preset_delete=<1-4>  preset_name=<1-4>:<name>
factory_reset=yes  refresh=1
<bool> is on|off|true|false|yes|no|1|0|toggle";

fn name_of<T: PartialEq + Copy>(value: T, table: &'static [(T, &'static str)]) -> &'static str {
    table
        .iter()
        .find(|(v, _)| *v == value)
        .map(|(_, n)| *n)
        .unwrap_or("?")
}

fn pick<T: Copy>(key: &str, value: &str, table: &[(T, &str)]) -> Result<T> {
    let wanted = normalize(value);
    table
        .iter()
        .find(|(_, n)| normalize(n) == wanted)
        .map(|(v, _)| *v)
        .ok_or_else(|| {
            let names: Vec<&str> = table.iter().map(|(_, n)| *n).collect();
            anyhow!("{key}: expected one of {}, got {value:?}", names.join("|"))
        })
}

/// "75 Hz", "75hz", "high-pass" and "high_pass" all compare equal.
fn normalize(s: &str) -> String {
    s.trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+')
        .collect::<String>()
        .trim_end_matches("hz")
        .to_string()
}

fn parse_bool(key: &str, value: &str, current: bool) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        "toggle" => Ok(!current),
        other => bail!("{key}: expected on|off|toggle, got {other:?}"),
    }
}

fn parse_int(key: &str, value: &str, min: i64, max: i64) -> Result<i64> {
    let n: i64 = value
        .trim()
        .parse()
        .with_context(|| format!("{key}: expected a number {min}..{max}, got {value:?}"))?;
    if n < min || n > max {
        bail!("{key}: {n} is outside {min}..{max}");
    }
    Ok(n)
}

fn parse_slot(key: &str, value: &str) -> Result<(usize, Option<String>)> {
    let (num, name) = match value.split_once(':') {
        Some((n, name)) => (n, Some(name.trim().to_string()).filter(|s| !s.is_empty())),
        None => (value, None),
    };
    let slot = parse_int(key, num, 1, presets::PRESET_COUNT as i64)? as usize;
    Ok((slot - 1, name))
}

fn parse_rgb(key: &str, value: &str) -> Result<[u8; 3]> {
    let v = value.trim();
    if let Some(hex) = v.strip_prefix('#') {
        if hex.len() == 6 {
            let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16);
            if let (Ok(r), Ok(g), Ok(b)) = (byte(0), byte(2), byte(4)) {
                return Ok([r, g, b]);
            }
        }
    } else {
        let parts: Vec<Result<u8, _>> = v.split(',').map(|p| p.trim().parse::<u8>()).collect();
        if let [Ok(r), Ok(g), Ok(b)] = parts.as_slice() {
            return Ok([*r, *g, *b]);
        }
    }
    bail!("{key}: expected #rrggbb or r,g,b, got {value:?}")
}

fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

/// One `KEY=VALUE` as the actions it means, computed against the shown state
/// (for `toggle`, and for presets that need a name set after saving).
fn parse_set(app: &App, arg: &str) -> Result<Vec<DeviceAction>> {
    let (key, value) = match arg.split_once('=') {
        Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim()),
        None => (arg.trim().to_ascii_lowercase(), "1"),
    };
    let key = key.as_str();
    let st = &app.device_state;
    let model = app.device_model;
    let b = |cur: bool| parse_bool(key, value, cur);

    // eq_band_N and eq_band_N_enabled
    if let Some(rest) = key.strip_prefix("eq_band_") {
        let (num, enabled) = match rest.strip_suffix("_enabled") {
            Some(n) => (n, true),
            None => (rest, false),
        };
        let band = parse_int(key, num, 1, 5)? as usize - 1;
        return Ok(vec![if enabled {
            DeviceAction::SetEqBandEnable(band, b(st.eq_bands[band].enabled)?)
        } else {
            let db: f32 = value
                .parse()
                .with_context(|| format!("{key}: expected a gain in dB, got {value:?}"))?;
            let tenths = (db * 10.0).round().clamp(-80.0, 60.0) as i16;
            DeviceAction::SetEqBandGain(band, tenths)
        }]);
    }

    let action = match key {
        "gain" => {
            let db: f32 = value
                .parse()
                .with_context(|| format!("gain: expected dB, got {value:?}"))?;
            let tenths = (db * 10.0).round().max(0.0) as u16;
            DeviceAction::SetGain(model.snap_gain_tenths(tenths))
        }
        "mode" => DeviceAction::SetMode(pick(key, value, MODES)?),
        "auto_position" => DeviceAction::SetAutoPosition(pick(key, value, POSITIONS)?),
        "auto_tone" => DeviceAction::SetAutoTone(pick(key, value, AUTO_TONES)?),
        "auto_gain" => DeviceAction::SetAutoGain(pick(key, value, AUTO_GAINS)?),
        "mic_preset" => DeviceAction::SetMicPreset(pick(key, value, MIC_PRESETS)?),
        "mute" => DeviceAction::SetMute(b(st.muted)?),
        "gain_lock" => DeviceAction::SetMv6GainLock(b(st.mv6_gain_locked)?),
        "lock" => DeviceAction::SetLock(b(st.locked)?),
        "mute_button_disabled" => {
            DeviceAction::SetMv6MuteBtnDisable(b(st.mute_btn_disabled.unwrap_or(false))?)
        }
        "monitor_mix" => DeviceAction::SetMonitorMix(parse_int(key, value, 0, 100)? as u8),
        "playback_mix" => DeviceAction::SetMv7PlaybackMix(parse_int(key, value, 0, 100)? as u8),
        "hpf" => DeviceAction::SetHpf(pick(key, value, HPFS)?),
        "compressor" => DeviceAction::SetCompressor(pick(key, value, COMPRESSORS)?),
        "limiter" => DeviceAction::SetLimiter(b(st.limiter_enabled)?),
        "denoiser" => DeviceAction::SetMv6Denoiser(b(st.denoiser_enabled)?),
        "denoiser_level" => DeviceAction::SetDenoiserLevel(parse_int(key, value, 1, 100)? as u8),
        "popper_stopper" => DeviceAction::SetMv6PopperStopper(b(st.popper_stopper_enabled)?),
        "tone" => DeviceAction::SetMv6Tone(parse_int(key, value, -10, 10)? as i8),
        "tone_mode" => DeviceAction::SetToneMode(match normalize(value).as_str() {
            "simple" => false,
            "advanced" => true,
            _ => bail!("tone_mode: expected simple|advanced, got {value:?}"),
        }),
        "adaptation_rate" => DeviceAction::SetAdaptationRate(match normalize(value).as_str() {
            "slow" => false,
            "fast" => true,
            _ => bail!("adaptation_rate: expected slow|fast, got {value:?}"),
        }),
        "phantom" => DeviceAction::SetPhantom(b(st.phantom_power)?),
        "eq" => DeviceAction::SetEqEnable(b(st.eq_enabled)?),
        "eq_flatten" => DeviceAction::FlattenEq,
        "eq_preset" => DeviceAction::SetEqPreset(pick(key, value, EQ_PRESETS)?),
        "led_live_meter" => DeviceAction::SetLedLiveMeter(b(st.led_live_meter)?),
        "led_night_mode" => DeviceAction::SetLedNightMode(b(st.led_night_mode)?),
        "reverb_output" => DeviceAction::SetMv7ReverbOutput(b(st.reverb_on_output)?),
        "reverb_monitoring" => DeviceAction::SetMv7ReverbMonitor(b(st.reverb_monitoring)?),
        "reverb_type" => DeviceAction::SetMv7ReverbPreset(pick(key, value, REVERBS)?),
        "reverb_intensity" => {
            DeviceAction::SetMv7ReverbIntensity(parse_int(key, value, 0, 100)? as u8)
        }
        "led_behavior" => DeviceAction::SetMv7LedBehavior(pick(key, value, LED_BEHAVIORS)?),
        "led_brightness" => DeviceAction::SetMv7LedBrightness(pick(key, value, LED_BRIGHTNESSES)?),
        "led_live_theme" => DeviceAction::SetMv7LedLiveTheme(pick(key, value, LED_LIVE_THEMES)?),
        "led_solid_theme" => DeviceAction::SetMv7LedSolidTheme(pick(key, value, LED_SOLID_THEMES)?),
        "led_pulsing_theme" => {
            DeviceAction::SetMv7LedPulsingTheme(pick(key, value, LED_PULSING_THEMES)?)
        }
        "led_solid_rgb" => DeviceAction::SetMv7LedSolidRgb(parse_rgb(key, value)?),
        "led_pulsing_rgb" => DeviceAction::SetMv7LedPulsingRgb(parse_rgb(key, value)?),
        "led_live_edge_rgb" => DeviceAction::SetMv7LedLiveEdgeRgb(parse_rgb(key, value)?),
        "led_live_middle_rgb" => DeviceAction::SetMv7LedLiveMiddleRgb(parse_rgb(key, value)?),
        "led_live_interior_rgb" => DeviceAction::SetMv7LedLiveInteriorRgb(parse_rgb(key, value)?),
        "device_name" => {
            if value.is_empty() {
                bail!("device_name: a name is needed");
            }
            DeviceAction::RenameDevice(value.to_string())
        }
        "preset_save" => {
            let (slot, name) = parse_slot(key, value)?;
            let mut actions = vec![DeviceAction::SavePreset(slot)];
            if name.is_some() {
                // SavePreset keeps the slot's old name; the rename is applied by
                // run_actions(), which sees the Rename marker below.
                actions.push(DeviceAction::PersistPresetName(slot));
            }
            return Ok(actions);
        }
        "preset_load" => DeviceAction::LoadPreset(parse_slot(key, value)?.0),
        "preset_delete" => DeviceAction::DeletePreset(parse_slot(key, value)?.0),
        "preset_name" => {
            let (slot, name) = parse_slot(key, value)?;
            if name.is_none() {
                bail!("preset_name: expected <slot>:<name>");
            }
            DeviceAction::PersistPresetName(slot)
        }
        "factory_reset" => {
            if normalize(value) != "yes" {
                bail!("factory_reset: pass factory_reset=yes to confirm");
            }
            DeviceAction::FactoryReset
        }
        "refresh" => DeviceAction::Refresh,
        other => bail!("unknown setting {other:?}. Settings:\n{KEYS}"),
    };
    Ok(vec![action])
}

/// The TUI changes the shown state before it sends an action; `apply_action()`
/// only sends it (and undoes the shown change on failure). Do the same here.
fn mirror(state: &mut DeviceState, action: &DeviceAction) {
    use DeviceAction::*;
    match action {
        SetGain(t) => state.gain_tenths = *t,
        SetMode(m) => state.mode = *m,
        SetAutoPosition(p) => state.auto_position = *p,
        SetAutoTone(t) => state.auto_tone = *t,
        SetAutoGain(g) => state.auto_gain = *g,
        SetMute(m) => state.muted = *m,
        SetPhantom(p) => state.phantom_power = *p,
        SetLock(l) => state.locked = *l,
        SetLimiter(l) => state.limiter_enabled = *l,
        SetCompressor(c) => state.compressor = *c,
        SetHpf(h) => state.hpf = *h,
        SetEqEnable(e) => state.eq_enabled = *e,
        SetEqBandEnable(i, e) => state.eq_bands[*i].enabled = *e,
        SetEqBandGain(i, g) => state.eq_bands[*i].gain_db = *g,
        SetMv6Denoiser(d) => state.denoiser_enabled = *d,
        SetMv6PopperStopper(p) => state.popper_stopper_enabled = *p,
        SetMv6MuteBtnDisable(d) => state.mute_btn_disabled = Some(*d),
        SetMv6Tone(t) => state.tone = *t,
        SetMv6GainLock(l) => state.mv6_gain_locked = *l,
        SetMonitorMix(m) => state.monitor_mix = *m,
        SetMv7PlaybackMix(m) => state.playback_mix = *m,
        SetAdaptationRate(f) => state.adaptation_rate_fast = *f,
        SetDenoiserLevel(l) => state.denoiser_level = *l,
        SetToneMode(a) => state.tone_advanced = *a,
        SetMv7ReverbOutput(r) => state.reverb_on_output = *r,
        SetMv7ReverbMonitor(r) => state.reverb_monitoring = *r,
        SetMv7ReverbPreset(r) => state.reverb_type = *r,
        SetMv7ReverbIntensity(i) => state.reverb_intensity = *i,
        SetMv7LedBehavior(b) => state.led_behavior = *b,
        SetMv7LedBrightness(b) => state.led_brightness = *b,
        SetMv7LedLiveTheme(t) => state.led_live_theme = *t,
        SetMv7LedSolidTheme(t) => state.led_solid_theme = *t,
        SetMv7LedPulsingTheme(t) => state.led_pulsing_theme = *t,
        SetMv7LedSolidRgb(c) => state.led_solid_rgb = *c,
        SetMv7LedPulsingRgb(c) => state.led_pulsing_rgb = *c,
        SetMv7LedLiveEdgeRgb(c) => state.led_live_edge_rgb = *c,
        SetMv7LedLiveMiddleRgb(c) => state.led_live_middle_rgb = *c,
        SetMv7LedLiveInteriorRgb(c) => state.led_live_interior_rgb = *c,
        SetEqPreset(p) => state.eq_preset = *p,
        SetLedLiveMeter(l) => state.led_live_meter = *l,
        SetLedNightMode(n) => state.led_night_mode = *n,
        // SetMicPreset: the mic applies its own settings; the refresh after the
        // writes picks them up. The rest change files or re-read the device.
        SetMicPreset(_) | SavePreset(_) | LoadPreset(_) | DeletePreset(_)
        | PersistPresetName(_) | RenameDevice(_) | FlattenEq | Refresh | Reconnect
        | FactoryReset => {}
    }
}

/// Apply every `--set` in order. Stops at the first failure and reports it.
/// Returns whether all of them succeeded.
fn run_sets(app: &mut App, device: &mut Option<ShureDevice>, sets: &[String]) -> bool {
    for arg in sets {
        let actions = match parse_set(app, arg) {
            Ok(a) => a,
            Err(e) => {
                app.set_err(e.to_string());
                return false;
            }
        };
        // preset_save=N:name and preset_name=N:name carry the name here.
        let new_name = arg
            .split_once('=')
            .and_then(|(k, v)| {
                let k = k.trim().to_ascii_lowercase();
                (k == "preset_save" || k == "preset_name").then_some(v)
            })
            .and_then(|v| v.split_once(':'))
            .map(|(_, name)| name.trim().to_string())
            .filter(|n| !n.is_empty());
        for action in actions {
            if let (DeviceAction::PersistPresetName(slot), Some(name)) = (&action, &new_name) {
                match app.presets[*slot].as_mut() {
                    Some(s) => s.name.clone_from(name),
                    None => {
                        app.set_err(format!("Preset slot {} is empty.", slot + 1));
                        return false;
                    }
                }
            }
            mirror(&mut app.device_state, &action);
            let before = app.device_state.clone();
            crate::apply_action(app, device, action, Some(before));
            if app.status_is_error {
                return false;
            }
        }
    }
    true
}

fn state_json(app: &App, connected: bool) -> Value {
    let s = &app.device_state;
    let m = app.device_model;
    let is_mv7plus = m == DeviceModel::Mv7Plus;
    let is_mv7 = m == DeviceModel::Mv7;
    let presets: Vec<Value> = app
        .presets
        .iter()
        .enumerate()
        .map(|(i, slot)| match slot {
            Some(p) => json!({ "slot": i + 1, "name": p.name, "summary": p.summary(m) }),
            None => json!({ "slot": i + 1, "name": Value::Null, "summary": Value::Null }),
        })
        .collect();
    let eq_bands: Vec<Value> = s
        .eq_bands
        .iter()
        .map(|b| json!({ "enabled": b.enabled, "gain_db": f32::from(b.gain_db) / 10.0 }))
        .collect();
    let info = json!({
        "device_name": s.device_name,
        "firmware_version": s.firmware_version,
        "factory_serial": s.factory_serial,
        "serial_number": s.serial_number,
    });
    let features = json!({
        "playback_mix": m.has_playback_mix(),
        "gain_lock": m.has_gain_lock(),
        "hpf": m.has_hpf(),
        "denoiser_level": m.has_denoiser_level(),
        "phantom": m.has_phantom_power(),
        "mic_presets": m.has_mic_presets(),
        "device_rename": m.has_device_rename(),
        "mute_button": m.has_mute_button(),
        "reverb": is_mv7plus,
        "led": is_mv7plus,
        "mv7_led": is_mv7,
        "factory_reset": app.supports_factory_reset(),
        "gain_max_db": f32::from(m.max_gain_tenths()) / 10.0,
        "gain_step_db": f32::from(m.gain_step_tenths()) / 10.0,
    });
    // One json! per object: the macro hits the recursion limit when the
    // whole thing is one literal.
    let state = json!({
        "gain_db": f32::from(s.gain_tenths) / 10.0,
        "mode": name_of(s.mode, MODES),
        "auto_position": name_of(s.auto_position, POSITIONS),
        "auto_tone": name_of(s.auto_tone, AUTO_TONES),
        "auto_gain": name_of(s.auto_gain, AUTO_GAINS),
        "muted": s.muted,
        "mute_button_disabled": s.mute_btn_disabled,
        "gain_lock": s.mv6_gain_locked,
        "lock": s.locked,
        "monitor_mix": s.monitor_mix,
        "playback_mix": s.playback_mix,
        "hpf": name_of(s.hpf, HPFS),
        "compressor": name_of(s.compressor, COMPRESSORS),
        "limiter": s.limiter_enabled,
        "denoiser": s.denoiser_enabled,
        "denoiser_level": s.denoiser_level,
        "popper_stopper": s.popper_stopper_enabled,
        "tone": s.tone,
        "tone_mode": if s.tone_advanced { "advanced" } else { "simple" },
        "adaptation_rate": if s.adaptation_rate_fast { "fast" } else { "slow" },
        "phantom": s.phantom_power,
        "eq": s.eq_enabled,
        "eq_bands": eq_bands,
        "eq_preset": name_of(s.eq_preset, EQ_PRESETS),
        "led_live_meter": s.led_live_meter,
        "led_night_mode": s.led_night_mode,
        "reverb_output": s.reverb_on_output,
        "reverb_monitoring": s.reverb_monitoring,
        "reverb_type": name_of(s.reverb_type, REVERBS),
        "reverb_intensity": s.reverb_intensity,
        "led_behavior": name_of(s.led_behavior, LED_BEHAVIORS),
        "led_brightness": name_of(s.led_brightness, LED_BRIGHTNESSES),
        "led_live_theme": name_of(s.led_live_theme, LED_LIVE_THEMES),
        "led_solid_theme": name_of(s.led_solid_theme, LED_SOLID_THEMES),
        "led_pulsing_theme": name_of(s.led_pulsing_theme, LED_PULSING_THEMES),
        "led_solid_rgb": hex(s.led_solid_rgb),
        "led_pulsing_rgb": hex(s.led_pulsing_rgb),
        "led_live_edge_rgb": hex(s.led_live_edge_rgb),
        "led_live_middle_rgb": hex(s.led_live_middle_rgb),
        "led_live_interior_rgb": hex(s.led_live_interior_rgb),
    });
    json!({
        "model": m.display_name(),
        "connected": connected,
        "status": app.status_message,
        "error": app.status_is_error,
        "info": info,
        "features": features,
        "state": state,
        "presets": presets,
    })
}

/// What `--follow` compares between polls: the settings, presets and
/// connection, not the status line, which changes with every write.
fn fingerprint(value: &Value) -> String {
    format!(
        "{}|{}|{}",
        value["connected"], value["state"], value["presets"]
    )
}

fn print_json(value: &Value) {
    let mut out = std::io::stdout().lock();
    // A closed pipe (the widget went away) is the only error here; nothing to
    // say about it and nowhere to say it.
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn disconnected_json(message: &str) -> Value {
    json!({ "connected": false, "error": true, "status": message, "model": Value::Null })
}

/// Entry point for `--json`, `--set` and `--follow`.
pub fn run(device_path: Option<&str>, sets: &[String], follow_ms: Option<u64>) -> Result<()> {
    if sets.iter().any(|s| s.eq_ignore_ascii_case("help")) {
        println!("{KEYS}");
        return Ok(());
    }

    let mut device = match crate::open_cli_device(device_path) {
        Ok(d) => Some(d),
        Err(e) => {
            print_json(&disconnected_json(&format!("{e:#}")));
            if follow_ms.is_none() {
                return Ok(());
            }
            None
        }
    };

    let mut app = App {
        device_model: device
            .as_ref()
            .map(|d| d.model)
            .unwrap_or(DeviceModel::Mv7Plus),
        ..App::default()
    };
    let mut last = String::new();
    if device.is_some() {
        crate::load_initial_state(&mut app, device.as_ref());
        app.presets = presets::load_all_presets();
        run_sets(&mut app, &mut device, sets);
        if !sets.is_empty() && !app.status_is_error {
            // The device may have snapped or refused something; show what it has.
            let msg = app.status_message.clone();
            crate::apply_action(&mut app, &mut device, DeviceAction::Refresh, None);
            if !app.status_is_error {
                app.set_ok(msg);
            }
        }
        let initial = state_json(&app, app.device_connected);
        print_json(&initial);
        last = fingerprint(&initial);
    }

    let Some(interval) = follow_ms else {
        return Ok(());
    };
    follow(
        app,
        device,
        device_path,
        Duration::from_millis(interval.max(100)),
        last,
    )
}

/// Keep the device open: re-read it every `interval`, print when something
/// changed, and apply KEY=VALUE lines from stdin as they arrive.
fn follow(
    mut app: App,
    mut device: Option<ShureDevice>,
    device_path: Option<&str>,
    interval: Duration,
    mut last: String,
) -> Result<()> {
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    loop {
        // Writes first, so a widget's change shows in the very next line.
        let mut wrote = false;
        while let Ok(line) = rx.try_recv() {
            if device.is_none() {
                app.set_err("Not connected");
                print_json(&state_json(&app, false));
                continue;
            }
            let sets: Vec<String> = vec![line];
            let ok = run_sets(&mut app, &mut device, &sets);
            wrote = true;
            if !ok {
                print_json(&state_json(&app, app.device_connected));
                last.clear();
            }
        }

        match device.as_ref() {
            Some(dev) => {
                let present = dev.is_present().unwrap_or(false);
                let read = if present {
                    dev.get_state()
                } else {
                    Err(anyhow!("unplugged"))
                };
                match read {
                    Ok(readback) => {
                        // Keep what a readback never reports (nothing today), and
                        // the status message, unless the device said something new.
                        app.device_state = readback.state;
                        app.state_unread = false;
                        app.device_connected = true;
                        let value = state_json(&app, true);
                        let text = fingerprint(&value);
                        if text != last || wrote {
                            print_json(&value);
                            last = text;
                            if !app.status_is_error {
                                app.set_ok("");
                            }
                        }
                    }
                    Err(e) => {
                        device = None;
                        app.device_connected = false;
                        print_json(&disconnected_json(&format!("{e:#}")));
                        last.clear();
                    }
                }
            }
            None => {
                if let Ok(d) = crate::open_cli_device(device_path) {
                    app.device_model = d.model;
                    device = Some(d);
                    crate::load_initial_state(&mut app, device.as_ref());
                    app.presets = presets::load_all_presets();
                    last.clear();
                }
            }
        }
        std::thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv7plus() -> App {
        App {
            device_model: DeviceModel::Mv7Plus,
            ..App::default()
        }
    }

    fn one(app: &App, arg: &str) -> DeviceAction {
        let mut actions = parse_set(app, arg).expect(arg);
        assert_eq!(actions.len(), 1, "{arg}");
        actions.remove(0)
    }

    #[test]
    fn names_compare_loosely() {
        assert_eq!(normalize("75 Hz"), "75");
        assert_eq!(normalize("High-Pass"), "highpass");
        assert_eq!(normalize("high_pass"), "highpass");
    }

    #[test]
    fn gain_is_db_snapped_to_the_models_step() {
        assert!(matches!(
            one(&mv7plus(), "gain=12"),
            DeviceAction::SetGain(120)
        ));
        assert!(matches!(
            one(&mv7plus(), "gain=12.4"),
            DeviceAction::SetGain(120)
        ));
        assert!(matches!(
            one(&mv7plus(), "gain=99"),
            DeviceAction::SetGain(360)
        ));
    }

    #[test]
    fn enums_accept_their_names_and_reject_others() {
        assert!(matches!(
            one(&mv7plus(), "hpf=75hz"),
            DeviceAction::SetHpf(HpfFrequency::Hz75)
        ));
        assert!(matches!(
            one(&mv7plus(), "compressor=Heavy"),
            DeviceAction::SetCompressor(CompressorPreset::Heavy)
        ));
        assert!(matches!(
            one(&mv7plus(), "led_live_theme=seaside"),
            DeviceAction::SetMv7LedLiveTheme(LedLiveTheme::Seaside)
        ));
        let err = parse_set(&mv7plus(), "hpf=99").unwrap_err().to_string();
        assert!(err.contains("off|75|150"), "{err}");
    }

    #[test]
    fn toggle_flips_the_shown_state() {
        let mut app = mv7plus();
        assert!(matches!(
            one(&app, "mute=toggle"),
            DeviceAction::SetMute(true)
        ));
        app.device_state.muted = true;
        assert!(matches!(
            one(&app, "mute=toggle"),
            DeviceAction::SetMute(false)
        ));
        assert!(matches!(one(&app, "mute"), DeviceAction::SetMute(true)));
    }

    #[test]
    fn rgb_takes_hex_or_decimal() {
        assert_eq!(parse_rgb("x", "#ff8000").unwrap(), [255, 128, 0]);
        assert_eq!(parse_rgb("x", "1, 2, 3").unwrap(), [1, 2, 3]);
        assert!(parse_rgb("x", "#ff80").is_err());
        assert_eq!(hex([255, 128, 0]), "#ff8000");
    }

    #[test]
    fn preset_save_with_a_name_also_renames() {
        let actions = parse_set(&mv7plus(), "preset_save=2:Call").unwrap();
        assert!(matches!(actions[0], DeviceAction::SavePreset(1)));
        assert!(matches!(actions[1], DeviceAction::PersistPresetName(1)));
        assert!(parse_set(&mv7plus(), "preset_load=5").is_err());
        assert!(parse_set(&mv7plus(), "preset_name=1").is_err());
    }

    #[test]
    fn factory_reset_needs_yes() {
        assert!(parse_set(&mv7plus(), "factory_reset=1").is_err());
        assert!(matches!(
            one(&mv7plus(), "factory_reset=yes"),
            DeviceAction::FactoryReset
        ));
    }

    #[test]
    fn mirror_changes_the_shown_state_like_the_tui() {
        let mut state = DeviceState::default();
        mirror(&mut state, &DeviceAction::SetMonitorMix(40));
        mirror(&mut state, &DeviceAction::SetMv7LedSolidRgb([1, 2, 3]));
        mirror(&mut state, &DeviceAction::SetEqBandGain(2, -40));
        assert_eq!(state.monitor_mix, 40);
        assert_eq!(state.led_solid_rgb, [1, 2, 3]);
        assert_eq!(state.eq_bands[2].gain_db, -40);
    }

    #[test]
    fn json_names_round_trip_through_set() {
        let app = mv7plus();
        let json = state_json(&app, true);
        let state = &json["state"];
        for key in [
            "hpf",
            "compressor",
            "reverb_type",
            "led_behavior",
            "led_brightness",
            "led_live_theme",
            "led_solid_theme",
            "led_pulsing_theme",
            "mode",
            "auto_position",
            "auto_tone",
            "auto_gain",
            "eq_preset",
        ] {
            let value = state[key]
                .as_str()
                .unwrap_or_else(|| panic!("{key} is a string"));
            parse_set(&app, &format!("{key}={value}")).unwrap_or_else(|e| panic!("{key}: {e}"));
        }
        assert!(fingerprint(&json).contains("\"monitor_mix\""));
    }
}
