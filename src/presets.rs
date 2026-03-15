//! Host-side preset storage for shurectl.
//!
//! Presets are TOML files stored in `~/.config/shurectl/presets/`.
//! There are 4 fixed slots, numbered 0–3, stored as `preset_1.toml`–`preset_4.toml`.
//!
//! Each file is human-readable and hand-editable. The preset captures all
//! configurable DSP settings from `DeviceState` — everything that can be sent
//! to the MVX2U over HID. Hardware-identity fields `serial_number` are intentionally excluded.
//!
//! This mirrors how MOTIV Desktop saves presets: the app sends a batch of SET
//! commands when a preset is loaded, with no device-side preset bank involved.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::protocol::{
    AutoGain, AutoTone, CompressorPreset, DeviceState, EqBand, HpfFrequency, InputMode, MicPosition,
};

pub const PRESET_COUNT: usize = 4;

/// A serializable snapshot of all configurable device settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresetSlot {
    /// Human-readable name shown in the TUI (editable in-app or by hand in TOML).
    pub name: String,

    // ── Main ─────────────────────────────────────────────────────────────────
    pub gain_db: u8,
    pub mode: SerInputMode,
    pub auto_position: SerMicPosition,
    pub auto_tone: SerAutoTone,
    pub auto_gain: SerAutoGain,
    pub muted: bool,
    pub phantom_power: bool,
    /// Monitor mix: 0 = 100% mic, 100 = 100% playback.
    pub monitor_mix: u8,

    // ── Dynamics ─────────────────────────────────────────────────────────────
    pub limiter_enabled: bool,
    pub compressor: SerCompressorPreset,
    pub hpf: SerHpfFrequency,

    // ── EQ ───────────────────────────────────────────────────────────────────
    pub eq_enabled: bool,
    pub eq_bands: [SerEqBand; 5],
}

impl PresetSlot {
    /// Build a preset snapshot from a live `DeviceState`.
    pub fn from_device_state(name: impl Into<String>, state: &DeviceState) -> Self {
        Self {
            name: name.into(),
            gain_db: state.gain_db,
            mode: SerInputMode::from(state.mode),
            auto_position: SerMicPosition::from(state.auto_position),
            auto_tone: SerAutoTone::from(state.auto_tone),
            auto_gain: SerAutoGain::from(state.auto_gain),
            muted: state.muted,
            phantom_power: state.phantom_power,
            monitor_mix: state.monitor_mix,
            limiter_enabled: state.limiter_enabled,
            compressor: SerCompressorPreset::from(state.compressor),
            hpf: SerHpfFrequency::from(state.hpf),
            eq_enabled: state.eq_enabled,
            eq_bands: state.eq_bands.map(SerEqBand::from),
        }
    }

    /// Apply this preset's settings onto a `DeviceState`, preserving
    /// hardware-identity fields (`serial_number`).
    pub fn apply_to_device_state(&self, state: &mut DeviceState) {
        state.gain_db = self.gain_db;
        state.mode = InputMode::from(self.mode);
        state.auto_position = MicPosition::from(self.auto_position);
        state.auto_tone = AutoTone::from(self.auto_tone);
        state.auto_gain = AutoGain::from(self.auto_gain);
        state.muted = self.muted;
        state.phantom_power = self.phantom_power;
        state.monitor_mix = self.monitor_mix;
        state.limiter_enabled = self.limiter_enabled;
        state.compressor = CompressorPreset::from(self.compressor);
        state.hpf = HpfFrequency::from(self.hpf);
        state.eq_enabled = self.eq_enabled;
        state.eq_bands = self.eq_bands.map(EqBand::from);
    }

    /// One-line summary of the key settings for display in the TUI.
    pub fn summary(&self) -> String {
        let phantom_str = if self.phantom_power {
            "48V on"
        } else {
            "48V off"
        };
        match InputMode::from(self.mode) {
            InputMode::Auto => {
                let tone = AutoTone::from(self.auto_tone);
                let gain = AutoGain::from(self.auto_gain);
                let pos = MicPosition::from(self.auto_position);
                format!("Auto · {tone} · {gain} · {pos} · {phantom_str}")
            }
            InputMode::Manual => {
                let eq_str = if self.eq_enabled { "EQ on" } else { "EQ off" };
                let comp_str = CompressorPreset::from(self.compressor).to_string();
                let hpf_str = match HpfFrequency::from(self.hpf) {
                    HpfFrequency::Off => "HPF off".to_string(),
                    freq => format!("HPF {freq}"),
                };
                format!(
                    "Manual · {}dB · {eq_str} · Comp: {comp_str} · {phantom_str} · {hpf_str}",
                    self.gain_db
                )
            }
        }
    }
}

// ── File I/O ──────────────────────────────────────────────────────────────────

/// Returns `~/.config/shurectl/presets/`, creating it if absent.
fn presets_dir() -> Result<PathBuf> {
    let base = dirs_next::config_dir()
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .context("Cannot determine config directory; set $HOME")?;
    let dir = base.join("shurectl").join("presets");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create preset directory: {}", dir.display()))?;
    Ok(dir)
}

/// Returns the path for slot `index` (0-based). File is named `preset_1.toml`–`preset_4.toml`.
pub fn preset_path(index: usize) -> Result<PathBuf> {
    Ok(presets_dir()?.join(format!("preset_{}.toml", index + 1)))
}

/// Load the preset at `index`. Returns `None` if the slot file does not exist.
pub fn load_preset(index: usize) -> Result<Option<PresetSlot>> {
    let path = preset_path(index)?;
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read preset file: {}", path.display()))?;
    let slot: PresetSlot = toml::from_str(&text)
        .with_context(|| format!("Failed to parse preset file: {}", path.display()))?;
    Ok(Some(slot))
}

/// Save a preset to slot `index`, overwriting any existing file.
pub fn save_preset(index: usize, slot: &PresetSlot) -> Result<()> {
    let path = preset_path(index)?;
    let text = toml::to_string_pretty(slot).context("Failed to serialise preset")?;
    std::fs::write(&path, text)
        .with_context(|| format!("Failed to write preset file: {}", path.display()))?;
    Ok(())
}

/// Delete the preset file for slot `index`. No-op if the slot is already empty.
pub fn delete_preset(index: usize) -> Result<()> {
    let path = preset_path(index)?;
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("Failed to delete preset file: {}", path.display()))?;
    }
    Ok(())
}

/// Load all 4 preset slots. Missing files produce `None` entries.
/// Parse errors are logged to stderr and treated as empty slots.
pub fn load_all_presets() -> [Option<PresetSlot>; PRESET_COUNT] {
    std::array::from_fn(|i| match load_preset(i) {
        Ok(slot) => slot,
        Err(e) => {
            eprintln!("Warning: failed to load preset slot {}: {e}", i + 1);
            None
        }
    })
}

// ── Serialisable mirror types ─────────────────────────────────────────────────
//
// We use separate mirror enums with `#[derive(Serialize, Deserialize)]` rather
// than adding serde derives directly to protocol types. This keeps protocol.rs
// free of serde concerns and ensures the on-disk format is stable even if the
// internal enums evolve.

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerInputMode {
    Auto,
    Manual,
}

impl From<InputMode> for SerInputMode {
    fn from(v: InputMode) -> Self {
        match v {
            InputMode::Auto => Self::Auto,
            InputMode::Manual => Self::Manual,
        }
    }
}

impl From<SerInputMode> for InputMode {
    fn from(v: SerInputMode) -> Self {
        match v {
            SerInputMode::Auto => Self::Auto,
            SerInputMode::Manual => Self::Manual,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerMicPosition {
    Near,
    Far,
}

impl From<MicPosition> for SerMicPosition {
    fn from(v: MicPosition) -> Self {
        match v {
            MicPosition::Near => Self::Near,
            MicPosition::Far => Self::Far,
        }
    }
}

impl From<SerMicPosition> for MicPosition {
    fn from(v: SerMicPosition) -> Self {
        match v {
            SerMicPosition::Near => Self::Near,
            SerMicPosition::Far => Self::Far,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerAutoTone {
    Dark,
    Natural,
    Bright,
}

impl From<AutoTone> for SerAutoTone {
    fn from(v: AutoTone) -> Self {
        match v {
            AutoTone::Dark => Self::Dark,
            AutoTone::Natural => Self::Natural,
            AutoTone::Bright => Self::Bright,
        }
    }
}

impl From<SerAutoTone> for AutoTone {
    fn from(v: SerAutoTone) -> Self {
        match v {
            SerAutoTone::Dark => Self::Dark,
            SerAutoTone::Natural => Self::Natural,
            SerAutoTone::Bright => Self::Bright,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerAutoGain {
    Quiet,
    Normal,
    Loud,
}

impl From<AutoGain> for SerAutoGain {
    fn from(v: AutoGain) -> Self {
        match v {
            AutoGain::Quiet => Self::Quiet,
            AutoGain::Normal => Self::Normal,
            AutoGain::Loud => Self::Loud,
        }
    }
}

impl From<SerAutoGain> for AutoGain {
    fn from(v: SerAutoGain) -> Self {
        match v {
            SerAutoGain::Quiet => Self::Quiet,
            SerAutoGain::Normal => Self::Normal,
            SerAutoGain::Loud => Self::Loud,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerCompressorPreset {
    Off,
    Light,
    Medium,
    Heavy,
}

impl From<CompressorPreset> for SerCompressorPreset {
    fn from(v: CompressorPreset) -> Self {
        match v {
            CompressorPreset::Off => Self::Off,
            CompressorPreset::Light => Self::Light,
            CompressorPreset::Medium => Self::Medium,
            CompressorPreset::Heavy => Self::Heavy,
        }
    }
}

impl From<SerCompressorPreset> for CompressorPreset {
    fn from(v: SerCompressorPreset) -> Self {
        match v {
            SerCompressorPreset::Off => Self::Off,
            SerCompressorPreset::Light => Self::Light,
            SerCompressorPreset::Medium => Self::Medium,
            SerCompressorPreset::Heavy => Self::Heavy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerHpfFrequency {
    Off,
    Hz75,
    Hz150,
}

impl From<HpfFrequency> for SerHpfFrequency {
    fn from(v: HpfFrequency) -> Self {
        match v {
            HpfFrequency::Off => Self::Off,
            HpfFrequency::Hz75 => Self::Hz75,
            HpfFrequency::Hz150 => Self::Hz150,
        }
    }
}

impl From<SerHpfFrequency> for HpfFrequency {
    fn from(v: SerHpfFrequency) -> Self {
        match v {
            SerHpfFrequency::Off => Self::Off,
            SerHpfFrequency::Hz75 => Self::Hz75,
            SerHpfFrequency::Hz150 => Self::Hz150,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SerEqBand {
    pub enabled: bool,
    /// Gain in dB, range −8..+6.
    pub gain_db: i8,
}

impl From<EqBand> for SerEqBand {
    fn from(v: EqBand) -> Self {
        Self {
            enabled: v.enabled,
            gain_db: v.gain_db,
        }
    }
}

impl From<SerEqBand> for EqBand {
    fn from(v: SerEqBand) -> Self {
        Self {
            enabled: v.enabled,
            gain_db: v.gain_db,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn example_state() -> DeviceState {
        DeviceState {
            gain_db: 42,
            mode: InputMode::Manual,
            auto_position: MicPosition::Far,
            auto_tone: AutoTone::Bright,
            auto_gain: AutoGain::Loud,
            muted: true,
            phantom_power: true,
            monitor_mix: 50,
            limiter_enabled: true,
            compressor: CompressorPreset::Medium,
            hpf: HpfFrequency::Hz75,
            eq_enabled: true,
            eq_bands: [
                EqBand {
                    enabled: true,
                    gain_db: 4,
                },
                EqBand {
                    enabled: false,
                    gain_db: -2,
                },
                EqBand {
                    enabled: true,
                    gain_db: 0,
                },
                EqBand {
                    enabled: false,
                    gain_db: 6,
                },
                EqBand {
                    enabled: true,
                    gain_db: -8,
                },
            ],
            locked: false,
            serial_number: String::from("TEST001"),
        }
    }

    #[test]
    fn preset_slot_roundtrip_toml() {
        let state = example_state();
        let slot = PresetSlot::from_device_state("My Preset", &state);

        let toml_str = toml::to_string_pretty(&slot).expect("serialise");
        let decoded: PresetSlot = toml::from_str(&toml_str).expect("deserialise");

        assert_eq!(slot, decoded);
        assert_eq!(decoded.name, "My Preset");
        assert_eq!(decoded.gain_db, 42);
        assert_eq!(decoded.muted, true);
        assert_eq!(decoded.eq_bands[0].gain_db, 4);
        assert_eq!(decoded.eq_bands[4].gain_db, -8);
    }

    #[test]
    fn apply_to_device_state_restores_all_fields() {
        let original = example_state();
        let slot = PresetSlot::from_device_state("Test", &original);

        // Apply onto a default state (different values).
        let mut target = DeviceState::default();
        // Preserve identity fields to confirm they are NOT overwritten.
        target.serial_number = String::from("OTHER");

        slot.apply_to_device_state(&mut target);

        assert_eq!(target.gain_db, 42);
        assert_eq!(target.mode, InputMode::Manual);
        assert_eq!(target.auto_position, MicPosition::Far);
        assert_eq!(target.auto_tone, AutoTone::Bright);
        assert_eq!(target.auto_gain, AutoGain::Loud);
        assert!(target.muted);
        assert!(target.phantom_power);
        assert_eq!(target.monitor_mix, 50);
        assert!(target.limiter_enabled);
        assert_eq!(target.compressor, CompressorPreset::Medium);
        assert_eq!(target.hpf, HpfFrequency::Hz75);
        assert!(target.eq_enabled);
        assert_eq!(target.eq_bands[0].gain_db, 4);
        // Identity fields must be untouched.
        assert_eq!(target.serial_number, "OTHER");
    }

    #[test]
    fn save_and_load_preset_roundtrip() {
        let state = example_state();
        let slot = PresetSlot::from_device_state("Roundtrip", &state);

        // Use a temp dir so tests are hermetic.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("preset_1.toml");

        let text = toml::to_string_pretty(&slot).expect("serialise");
        std::fs::write(&path, &text).expect("write");

        let loaded: PresetSlot =
            toml::from_str(&std::fs::read_to_string(&path).expect("read")).expect("deserialise");

        assert_eq!(slot, loaded);
    }

    #[test]
    fn summary_manual_mode_contains_gain_eq_dynamics_phantom_hpf() {
        let mut state = example_state();
        state.mode = InputMode::Manual;
        state.gain_db = 36;
        state.phantom_power = true;
        state.hpf = HpfFrequency::Hz75;
        let slot = PresetSlot::from_device_state("S", &state);
        let s = slot.summary();
        assert!(s.contains("Manual"), "summary: {s}");
        assert!(s.contains("36dB"), "summary: {s}");
        assert!(s.contains("EQ"), "summary: {s}");
        assert!(s.contains("Comp:"), "summary: {s}");
        assert!(s.contains("48V on"), "summary: {s}");
        assert!(s.contains("HPF 75 Hz"), "summary: {s}");
    }

    #[test]
    fn summary_auto_mode_contains_tone_gain_position_phantom_no_eq_dynamics() {
        let mut state = example_state();
        state.mode = InputMode::Auto;
        state.auto_tone = AutoTone::Natural;
        state.auto_gain = AutoGain::Normal;
        state.auto_position = MicPosition::Near;
        state.phantom_power = false;
        let slot = PresetSlot::from_device_state("S", &state);
        let s = slot.summary();
        assert!(s.contains("Auto"), "summary: {s}");
        assert!(s.contains("Natural"), "summary: {s}");
        assert!(s.contains("Normal"), "summary: {s}");
        assert!(s.contains("Near"), "summary: {s}");
        assert!(s.contains("48V off"), "summary: {s}");
        // EQ and Dynamics are not shown in Auto mode
        assert!(!s.contains("EQ"), "Auto summary must not mention EQ: {s}");
        assert!(
            !s.contains("Comp"),
            "Auto summary must not mention Comp: {s}"
        );
        assert!(!s.contains("HPF"), "Auto summary must not mention HPF: {s}");
    }
}
