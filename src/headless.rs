//! Headless JSON interface for scripting and automation.
//!
//! When a subcommand is given on the command line, `main()` routes here instead
//! of launching the TUI. Every command produces a single JSON object on stdout;
//! on error a `{"error": ...}` object is printed and the process exits non-zero.
//!
//! This is a sibling consumer of `device.rs` alongside the TUI's `apply_action`:
//! it opens the device, calls the same typed `get_*`/`set_*` methods, and never
//! touches the terminal. The accepted enum tokens for `set` are the exact
//! snake_case strings the `Ser*` mirror types emit in `get` output, so reads and
//! writes are symmetric.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::device::ShureDevice;
use crate::presets::{
    self, PRESET_COUNT, PresetSlot, SerAutoGain, SerAutoTone, SerCompressorPreset, SerHpfFrequency,
    SerInputMode, SerLedBehavior, SerLedBrightness, SerLedLiveTheme, SerLedPulsingTheme,
    SerLedSolidTheme, SerMicPosition, SerReverbType,
};
use crate::protocol::{
    AutoGain, AutoTone, CompressorPreset, DeviceModel, DeviceState, HpfFrequency, InputMode,
    LedBehavior, LedBrightness, LedLiveTheme, LedPulsingTheme, LedSolidTheme, MicPosition,
    ReverbType,
};
use crate::{Command, PresetCommand};

use DeviceModel::{Mv6, Mv7Plus, Mvx2u, Mvx2uGen2};

/// Run a headless command. Prints a JSON result on stdout, or a JSON error and
/// `exit(1)` on failure.
pub(crate) fn run(command: Command, device_path: Option<&str>) -> Result<()> {
    match dispatch(command, device_path) {
        Ok(value) => {
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
        Err(e) => {
            let value = json!({ "error": e.to_string() });
            println!("{}", serde_json::to_string_pretty(&value)?);
            std::process::exit(1);
        }
    }
}

fn dispatch(command: Command, device_path: Option<&str>) -> Result<Value> {
    match command {
        Command::Get => {
            let dev = open(device_path)?;
            let state = read_state(&dev)?;
            state_json(dev.model, &state)
        }
        Command::Set { setting, value } => set_command(device_path, setting, value),
        Command::Preset { action } => match action {
            PresetCommand::List => preset_list(),
            PresetCommand::Save { slot } => preset_save(&open(device_path)?, slot),
            PresetCommand::Load { slot } => preset_load(&open(device_path)?, slot),
            PresetCommand::Delete { slot } => preset_delete(slot),
        },
    }
}

// ── Device helpers ────────────────────────────────────────────────────────────

fn open(device_path: Option<&str>) -> Result<ShureDevice> {
    match device_path {
        Some(path) => ShureDevice::open_path(path),
        None => ShureDevice::open(),
    }
}

fn read_state(dev: &ShureDevice) -> Result<DeviceState> {
    let mut state = dev.get_state()?;
    state.serial_number = dev.serial_number.clone();
    Ok(state)
}

/// Build the `{model, serial, settings}` JSON object for a device state.
///
/// Reuses `PresetSlot` for the settings body so the field set and value tokens
/// match the on-disk preset format. The preset `name` field is dropped — it is
/// host-side metadata with no meaning for live state.
fn state_json(model: DeviceModel, state: &DeviceState) -> Result<Value> {
    let slot = PresetSlot::from_device_state("", state);
    let mut settings = serde_json::to_value(&slot)?;
    if let Some(obj) = settings.as_object_mut() {
        obj.remove("name");
    }
    Ok(json!({
        "model": model.display_name(),
        "serial": state.serial_number,
        "settings": settings,
    }))
}

// ── set ───────────────────────────────────────────────────────────────────────

fn set_command(device_path: Option<&str>, setting: String, value: Option<String>) -> Result<Value> {
    if setting.eq_ignore_ascii_case("help") {
        return Ok(help_json());
    }
    let value = value.ok_or_else(|| {
        anyhow!("a value is required: `set {setting} <value>` (run `set help` for options)")
    })?;

    let dev = open(device_path)?;
    apply_setting(&dev, &setting, &value)?;

    let state = read_state(&dev)?;
    let mut out = state_json(dev.model, &state)?;
    if let Some(obj) = out.as_object_mut() {
        obj.insert(
            "applied".to_string(),
            json!({ "setting": normalize(&setting), "value": value }),
        );
    }
    Ok(out)
}

fn apply_setting(dev: &ShureDevice, setting: &str, value: &str) -> Result<()> {
    let model = dev.model;
    let key = normalize(setting);
    ensure_supported(model, &key)?;
    dispatch_set(dev, model, &key, value)
}

fn dispatch_set(dev: &ShureDevice, model: DeviceModel, key: &str, value: &str) -> Result<()> {
    match key {
        "gain" => dev.set_gain(parse_u8_max(value, model.max_gain_db())?),
        "mode" => {
            let m: SerInputMode = parse_token(value)?;
            dev.set_mode(InputMode::from(m) == InputMode::Auto)
        }
        "mute" => dev.set_mute(parse_bool(value)?),
        "hpf" => {
            let h: SerHpfFrequency = parse_token(value)?;
            dev.set_hpf(&HpfFrequency::from(h))
        }
        "monitor-mix" => {
            let mix = parse_u8_max(value, 100)?;
            match model {
                Mvx2u => dev.set_monitor_mix(mix),
                _ => dev.set_mv6_monitor_mix(mix),
            }
        }
        "phantom" => dev.set_phantom(parse_bool(value)?),
        "limiter" => dev.set_limiter(parse_bool(value)?),
        "compressor" => {
            let c: SerCompressorPreset = parse_token(value)?;
            dev.set_compressor(&CompressorPreset::from(c))
        }
        "eq" => dev.set_eq_enable(parse_bool(value)?),
        "lock" => dev.set_lock(parse_bool(value)?),
        "auto-position" => {
            let p: SerMicPosition = parse_token(value)?;
            dev.set_auto_position(&MicPosition::from(p))
        }
        "auto-tone" => {
            let t: SerAutoTone = parse_token(value)?;
            dev.set_auto_tone(&AutoTone::from(t))
        }
        "auto-gain" => {
            let g: SerAutoGain = parse_token(value)?;
            dev.set_auto_gain(&AutoGain::from(g))
        }
        "denoiser" => dev.set_mv6_denoiser(parse_bool(value)?),
        "popper-stopper" => dev.set_mv6_popper_stopper(parse_bool(value)?),
        "tone" => dev.set_mv6_tone(parse_i8_range(value, -10, 10)?),
        "gain-lock" => dev.set_mv6_gain_lock(parse_bool(value)?),
        "mute-button-disable" => dev.set_mv6_mute_btn_disable(parse_bool(value)?),
        "playback-mix" => dev.set_mv7_playback_mix(parse_u8_max(value, 100)?),
        "reverb-output" => dev.set_mv7_reverb_output(parse_bool(value)?),
        "reverb-monitor" => dev.set_mv7_reverb_monitor(parse_bool(value)?),
        "reverb-type" => {
            let r: SerReverbType = parse_token(value)?;
            dev.set_mv7_reverb_type(&ReverbType::from(r))
        }
        "reverb-intensity" => dev.set_mv7_reverb_intensity(parse_u8_max(value, 100)?),
        "led-behavior" => {
            let b: SerLedBehavior = parse_token(value)?;
            dev.set_mv7_led_behavior(LedBehavior::from(b))
        }
        "led-brightness" => {
            let b: SerLedBrightness = parse_token(value)?;
            dev.set_mv7_led_brightness(LedBrightness::from(b))
        }
        "led-live-theme" => {
            let t: SerLedLiveTheme = parse_token(value)?;
            dev.set_mv7_led_live_theme(LedLiveTheme::from(t))
        }
        "led-solid-theme" => {
            let t: SerLedSolidTheme = parse_token(value)?;
            dev.set_mv7_led_solid_theme(LedSolidTheme::from(t))
        }
        "led-pulsing-theme" => {
            let t: SerLedPulsingTheme = parse_token(value)?;
            dev.set_mv7_led_pulsing_theme(LedPulsingTheme::from(t))
        }
        "led-solid-rgb" => dev.set_mv7_led_solid_color(parse_rgb(value)?),
        "led-pulsing-rgb" => dev.set_mv7_led_pulsing_color(parse_rgb(value)?),
        "led-live-edge-rgb" => dev.set_mv7_led_live_edge(parse_rgb(value)?),
        "led-live-middle-rgb" => dev.set_mv7_led_live_middle(parse_rgb(value)?),
        "led-live-interior-rgb" => dev.set_mv7_led_live_interior(parse_rgb(value)?),
        other => {
            if let Some((band, is_enable)) = eq_band(other) {
                if is_enable {
                    dev.set_eq_band_enable(band, parse_bool(value)?)
                } else {
                    dev.set_eq_band_gain(band, parse_db_tenths(value)?)
                }
            } else {
                Err(anyhow!("internal: no handler for setting '{other}'"))
            }
        }
    }
}

/// Verify a setting exists and applies to this device model.
fn ensure_supported(model: DeviceModel, key: &str) -> Result<()> {
    match catalog().iter().find(|spec| spec.name == key) {
        None => Err(anyhow!(
            "unknown setting '{key}'. Run `shurectl set help` to list settings."
        )),
        Some(spec) if !spec.models.contains(&model) => Err(anyhow!(
            "setting '{key}' is not supported on {}",
            model.display_name()
        )),
        Some(_) => Ok(()),
    }
}

// ── Presets ───────────────────────────────────────────────────────────────────

fn slot_index(slot: usize) -> Result<usize> {
    if (1..=PRESET_COUNT).contains(&slot) {
        Ok(slot - 1)
    } else {
        Err(anyhow!("slot must be 1..={PRESET_COUNT}, got {slot}"))
    }
}

fn preset_list() -> Result<Value> {
    let all = presets::load_all_presets();
    let mut slots = Vec::with_capacity(PRESET_COUNT);
    for (i, opt) in all.iter().enumerate() {
        match opt {
            Some(slot) => slots.push(json!({
                "slot": i + 1,
                "filled": true,
                "preset": serde_json::to_value(slot)?,
            })),
            None => slots.push(json!({ "slot": i + 1, "filled": false })),
        }
    }
    Ok(json!({ "presets": slots }))
}

fn preset_save(dev: &ShureDevice, slot: usize) -> Result<Value> {
    let idx = slot_index(slot)?;
    let state = read_state(dev)?;
    let name = presets::load_preset(idx)?
        .map(|s| s.name)
        .unwrap_or_else(|| format!("Preset {slot}"));
    let data = PresetSlot::from_device_state(name, &state);
    presets::save_preset(idx, &data)?;
    Ok(json!({
        "saved": { "slot": slot, "name": data.name },
        "preset": serde_json::to_value(&data)?,
    }))
}

fn preset_load(dev: &ShureDevice, slot: usize) -> Result<Value> {
    let idx = slot_index(slot)?;
    let data = presets::load_preset(idx)?.ok_or_else(|| anyhow!("preset slot {slot} is empty"))?;
    let mut state = read_state(dev)?;
    data.apply_to_device_state(&mut state);
    crate::apply_preset_to_device(dev, &state, dev.model)?;

    let state = read_state(dev)?;
    let mut out = state_json(dev.model, &state)?;
    if let Some(obj) = out.as_object_mut() {
        obj.insert(
            "loaded".to_string(),
            json!({ "slot": slot, "name": data.name }),
        );
    }
    Ok(out)
}

fn preset_delete(slot: usize) -> Result<Value> {
    let idx = slot_index(slot)?;
    presets::delete_preset(idx)?;
    Ok(json!({ "deleted": { "slot": slot } }))
}

// ── Settings catalog ──────────────────────────────────────────────────────────

const ALL: &[DeviceModel] = &[Mvx2u, Mvx2uGen2, Mv6, Mv7Plus];
const XLR: &[DeviceModel] = &[Mvx2u, Mvx2uGen2];
const COMP: &[DeviceModel] = &[Mvx2u, Mvx2uGen2, Mv7Plus];
const EQ: &[DeviceModel] = &[Mvx2u, Mvx2uGen2];
const GEN1: &[DeviceModel] = &[Mvx2u];
const DSP3: &[DeviceModel] = &[Mvx2uGen2, Mv6, Mv7Plus];
const GAINLOCK: &[DeviceModel] = &[Mvx2uGen2, Mv6];
const MUTEBTN: &[DeviceModel] = &[Mv6, Mv7Plus];
const MV7: &[DeviceModel] = &[Mv7Plus];

struct SettingSpec {
    name: String,
    values: &'static str,
    models: &'static [DeviceModel],
}

fn spec(name: &str, values: &'static str, models: &'static [DeviceModel]) -> SettingSpec {
    SettingSpec {
        name: name.to_string(),
        values,
        models,
    }
}

/// Every settable name, its accepted values, and the models it applies to.
/// Single source of truth for `set help` output and applicability checks.
fn catalog() -> Vec<SettingSpec> {
    let mut v = vec![
        spec("gain", "0-60 dB (MVX2U) or 0-36 dB (MV6, MV7+)", ALL),
        spec("mode", "auto | manual", ALL),
        spec("mute", "on | off", ALL),
        spec("hpf", "off | hz75 | hz150", ALL),
        spec("monitor-mix", "0-100 (percent playback)", ALL),
        spec("phantom", "on | off (48V)", XLR),
        spec("limiter", "on | off", COMP),
        spec("compressor", "off | light | medium | heavy", COMP),
        spec("eq", "on | off (master enable)", GEN1),
        spec("lock", "on | off (panel lock)", GEN1),
        spec("auto-position", "near | far", GEN1),
        spec("auto-tone", "dark | natural | bright", GEN1),
        spec("auto-gain", "quiet | normal | loud", GEN1),
        spec("denoiser", "on | off", DSP3),
        spec("popper-stopper", "on | off", DSP3),
        spec(
            "tone",
            "-10 to 10 (negative = dark, positive = bright)",
            DSP3,
        ),
        spec("gain-lock", "on | off", GAINLOCK),
        spec("mute-button-disable", "on | off", MUTEBTN),
        spec("playback-mix", "0-100 (percent playback)", MV7),
        spec("reverb-output", "on | off", MV7),
        spec("reverb-monitor", "on | off", MV7),
        spec("reverb-type", "plate | hall | studio", MV7),
        spec("reverb-intensity", "0-100", MV7),
        spec("led-behavior", "live | pulsing | solid", MV7),
        spec("led-brightness", "low | med | high | max", MV7),
        spec(
            "led-live-theme",
            "default | seaside | space | fruity | custom",
            MV7,
        ),
        spec("led-solid-theme", "shure | custom", MV7),
        spec("led-pulsing-theme", "shure | custom", MV7),
        spec("led-solid-rgb", "hex RRGGBB or r,g,b", MV7),
        spec("led-pulsing-rgb", "hex RRGGBB or r,g,b", MV7),
        spec("led-live-edge-rgb", "hex RRGGBB or r,g,b", MV7),
        spec("led-live-middle-rgb", "hex RRGGBB or r,g,b", MV7),
        spec("led-live-interior-rgb", "hex RRGGBB or r,g,b", MV7),
    ];
    for band in 1..=5 {
        v.push(spec(&format!("eq{band}"), "gain in dB, -8.0 to +6.0", EQ));
        v.push(spec(&format!("eq{band}-enable"), "on | off", GEN1));
    }
    v
}

fn help_json() -> Value {
    let entries: Vec<Value> = catalog()
        .iter()
        .map(|spec| {
            json!({
                "setting": spec.name,
                "values": spec.values,
                "models": spec.models.iter().map(|m| m.display_name()).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "settings": entries })
}

// ── Value parsing ─────────────────────────────────────────────────────────────

/// Lowercase the setting name and unify separators so `monitor_mix`,
/// `Monitor-Mix`, and `monitor-mix` all resolve to the same key.
fn normalize(setting: &str) -> String {
    setting.trim().to_ascii_lowercase().replace('_', "-")
}

/// Parse an enum token by deserializing into its `Ser*` mirror type. The serde
/// error names the accepted variants, which is what a caller needs on a miss.
fn parse_token<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_value(Value::String(value.trim().to_ascii_lowercase()))
        .map_err(|e| anyhow!("invalid value '{value}' ({e})"))
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" | "enabled" => Ok(true),
        "off" | "false" | "0" | "no" | "disabled" => Ok(false),
        _ => Err(anyhow!("expected on/off, got '{value}'")),
    }
}

fn parse_u8_max(value: &str, max: u8) -> Result<u8> {
    let n: u8 = value
        .trim()
        .parse()
        .map_err(|_| anyhow!("expected an integer 0..={max}, got '{value}'"))?;
    if n > max {
        return Err(anyhow!("value {n} out of range 0..={max}"));
    }
    Ok(n)
}

fn parse_i8_range(value: &str, lo: i8, hi: i8) -> Result<i8> {
    let n: i8 = value
        .trim()
        .parse()
        .map_err(|_| anyhow!("expected an integer {lo}..={hi}, got '{value}'"))?;
    if n < lo || n > hi {
        return Err(anyhow!("value {n} out of range {lo}..={hi}"));
    }
    Ok(n)
}

/// Parse a dB value into tenths, validated to the EQ range -8.0..=+6.0 dB.
fn parse_db_tenths(value: &str) -> Result<i16> {
    let db: f32 = value
        .trim()
        .parse()
        .map_err(|_| anyhow!("expected a number in dB, got '{value}'"))?;
    let tenths = (db * 10.0).round() as i16;
    if !(-80..=60).contains(&tenths) {
        return Err(anyhow!("EQ gain {db} dB out of range -8.0..=6.0"));
    }
    Ok(tenths)
}

/// Parse an RGB color as hex (`RRGGBB`, optional leading `#`) or `r,g,b` decimal.
fn parse_rgb(value: &str) -> Result<[u8; 3]> {
    let v = value.trim().trim_start_matches('#');
    if v.len() == 6 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
        let r = u8::from_str_radix(&v[0..2], 16)?;
        let g = u8::from_str_radix(&v[2..4], 16)?;
        let b = u8::from_str_radix(&v[4..6], 16)?;
        return Ok([r, g, b]);
    }
    let parts: Vec<&str> = v.split(',').collect();
    if parts.len() == 3 {
        let r = parts[0].trim().parse()?;
        let g = parts[1].trim().parse()?;
        let b = parts[2].trim().parse()?;
        return Ok([r, g, b]);
    }
    Err(anyhow!(
        "invalid color '{value}'; expected hex RRGGBB or r,g,b"
    ))
}

/// Parse an `eq<N>` or `eq<N>-enable` key into `(band_index, is_enable)`.
fn eq_band(key: &str) -> Option<(usize, bool)> {
    let rest = key.strip_prefix("eq")?;
    let (digits, is_enable) = match rest.strip_suffix("-enable") {
        Some(d) => (d, true),
        None => (rest, false),
    };
    let band: usize = digits.parse().ok()?;
    if (1..=5).contains(&band) {
        Some((band - 1, is_enable))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bool_accepts_common_forms() {
        for s in ["on", "ON", "true", "1", "yes", "enabled"] {
            assert!(parse_bool(s).unwrap());
        }
        for s in ["off", "OFF", "false", "0", "no", "disabled"] {
            assert!(!parse_bool(s).unwrap());
        }
        assert!(parse_bool("maybe").is_err());
    }

    #[test]
    fn parse_u8_max_enforces_bound() {
        assert_eq!(parse_u8_max("24", 36).unwrap(), 24);
        assert_eq!(parse_u8_max("0", 36).unwrap(), 0);
        assert!(parse_u8_max("37", 36).is_err());
        assert!(parse_u8_max("-1", 36).is_err());
        assert!(parse_u8_max("x", 36).is_err());
    }

    #[test]
    fn parse_i8_range_enforces_bounds() {
        assert_eq!(parse_i8_range("-10", -10, 10).unwrap(), -10);
        assert_eq!(parse_i8_range("10", -10, 10).unwrap(), 10);
        assert!(parse_i8_range("11", -10, 10).is_err());
        assert!(parse_i8_range("-11", -10, 10).is_err());
    }

    #[test]
    fn parse_db_tenths_rounds_and_bounds() {
        let cases = vec![
            ("0", 0i16),
            ("2", 20),
            ("2.5", 25),
            ("-8", -80),
            ("6", 60),
            ("-7.95", -80),
        ];
        for (input, expected) in cases {
            assert_eq!(parse_db_tenths(input).unwrap(), expected, "input {input}");
        }
        assert!(parse_db_tenths("6.1").is_err());
        assert!(parse_db_tenths("-8.1").is_err());
    }

    #[test]
    fn parse_rgb_accepts_hex_and_decimal() {
        assert_eq!(parse_rgb("B2FF33").unwrap(), [0xB2, 0xFF, 0x33]);
        assert_eq!(parse_rgb("#b2ff33").unwrap(), [0xB2, 0xFF, 0x33]);
        assert_eq!(parse_rgb("178,255,51").unwrap(), [178, 255, 51]);
        assert!(parse_rgb("178,255").is_err());
        assert!(parse_rgb("GGGGGG").is_err());
    }

    #[test]
    fn parse_token_maps_enum_variants() {
        let c: SerCompressorPreset = parse_token("medium").unwrap();
        assert_eq!(c, SerCompressorPreset::Medium);
        let h: SerHpfFrequency = parse_token("HZ75").unwrap();
        assert_eq!(h, SerHpfFrequency::Hz75);
        let r: Result<SerReverbType> = parse_token("nope");
        assert!(r.is_err());
    }

    #[test]
    fn eq_band_parses_gain_and_enable() {
        assert_eq!(eq_band("eq1"), Some((0, false)));
        assert_eq!(eq_band("eq5"), Some((4, false)));
        assert_eq!(eq_band("eq3-enable"), Some((2, true)));
        assert_eq!(eq_band("eq6"), None);
        assert_eq!(eq_band("eq0"), None);
        assert_eq!(eq_band("gain"), None);
    }

    #[test]
    fn normalize_unifies_separators_and_case() {
        assert_eq!(normalize("Monitor_Mix"), "monitor-mix");
        assert_eq!(normalize("  GAIN  "), "gain");
    }

    #[test]
    fn ensure_supported_respects_model_applicability() {
        assert!(ensure_supported(Mvx2u, "gain").is_ok());
        assert!(ensure_supported(Mv6, "gain").is_ok());
        // phantom is XLR-only.
        assert!(ensure_supported(Mvx2u, "phantom").is_ok());
        assert!(ensure_supported(Mv6, "phantom").is_err());
        // LED is MV7+ only.
        assert!(ensure_supported(Mv7Plus, "led-behavior").is_ok());
        assert!(ensure_supported(Mv6, "led-behavior").is_err());
        // EQ bands are MVX2U gen1/gen2; enables are gen1 only.
        assert!(ensure_supported(Mvx2uGen2, "eq3").is_ok());
        assert!(ensure_supported(Mv6, "eq3").is_err());
        assert!(ensure_supported(Mvx2uGen2, "eq3-enable").is_err());
        assert!(ensure_supported(Mvx2u, "eq3-enable").is_ok());
        // Unknown setting.
        assert!(ensure_supported(Mvx2u, "nonsense").is_err());
    }

    #[test]
    fn every_catalog_entry_has_a_dispatch_arm() {
        // Guard against catalog/dispatch drift: each name must be either a
        // literal arm or an eq pattern. We can't call dispatch_set without a
        // device, so check the static name set instead.
        let handled: std::collections::HashSet<&str> = [
            "gain",
            "mode",
            "mute",
            "hpf",
            "monitor-mix",
            "phantom",
            "limiter",
            "compressor",
            "eq",
            "lock",
            "auto-position",
            "auto-tone",
            "auto-gain",
            "denoiser",
            "popper-stopper",
            "tone",
            "gain-lock",
            "mute-button-disable",
            "playback-mix",
            "reverb-output",
            "reverb-monitor",
            "reverb-type",
            "reverb-intensity",
            "led-behavior",
            "led-brightness",
            "led-live-theme",
            "led-solid-theme",
            "led-pulsing-theme",
            "led-solid-rgb",
            "led-pulsing-rgb",
            "led-live-edge-rgb",
            "led-live-middle-rgb",
            "led-live-interior-rgb",
        ]
        .into_iter()
        .collect();

        for spec in catalog() {
            let known = handled.contains(spec.name.as_str()) || eq_band(&spec.name).is_some();
            assert!(known, "catalog entry '{}' has no dispatch arm", spec.name);
        }
    }
}
