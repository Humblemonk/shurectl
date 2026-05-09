//! Host-side preset storage for shurectl.
//!
//! Presets are TOML files stored in `~/.config/shurectl/presets/`.
//! There are 4 fixed slots, numbered 0–3, stored as `preset_1.toml`–`preset_4.toml`.
//!
//! Each file is human-readable and hand-editable. The preset captures all
//! configurable DSP settings from `DeviceState` — everything that can be sent
//! to the device over HID. Hardware-identity fields `serial_number` are intentionally excluded.
//!
//! This mirrors how MOTIV Desktop saves presets: the app sends a batch of SET
//! commands when a preset is loaded, with no device-side preset bank involved.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::protocol::{
    AutoGain, AutoTone, CompressorPreset, DeviceModel, DeviceState, EqBand, HpfFrequency,
    InputMode, LedBehavior, LedBrightness, LedLiveTheme, LedPulsingTheme, LedSolidTheme,
    MicPosition, ReverbType,
};

pub const PRESET_COUNT: usize = 4;

/// A serializable snapshot of all configurable device settings.
///
/// Fields from both MVX2U and MV6 are included. Fields irrelevant to a given
/// device model remain at their serde defaults and are not applied to the device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresetSlot {
    /// Human-readable name shown in the TUI (editable in-app or by hand in TOML).
    pub name: String,

    // ── Shared ───────────────────────────────────────────────────────────────
    pub gain_db: u8,
    pub mode: SerInputMode,
    pub muted: bool,
    pub hpf: SerHpfFrequency,

    // ── MVX2U-specific ───────────────────────────────────────────────────────
    #[serde(default)]
    pub auto_position: SerMicPosition,
    #[serde(default)]
    pub auto_tone: SerAutoTone,
    #[serde(default)]
    pub auto_gain: SerAutoGain,
    #[serde(default)]
    pub phantom_power: bool,
    /// Monitor mix: 0 = 100% mic, 100 = 100% playback.
    #[serde(default)]
    pub monitor_mix: u8,
    #[serde(default)]
    pub limiter_enabled: bool,
    #[serde(default)]
    pub compressor: SerCompressorPreset,
    #[serde(default)]
    pub eq_enabled: bool,
    #[serde(default)]
    pub eq_bands: [SerEqBand; 5],

    // ── MV6-specific ─────────────────────────────────────────────────────────
    #[serde(default)]
    pub denoiser_enabled: bool,
    #[serde(default = "default_popper_stopper")]
    pub popper_stopper_enabled: bool,
    #[serde(default)]
    pub mute_btn_disabled: bool,
    /// Tone: -10 (100% Dark) to +10 (100% Bright), 0 = Natural.
    #[serde(default)]
    pub tone: i8,
    /// Gain lock (Manual mode only). MV6 only.
    #[serde(default)]
    pub mv6_gain_locked: bool,

    // ── MV7+-specific ────────────────────────────────────────────────────────
    /// Independent playback mix channel: 0 = full mic, 100 = full playback.
    #[serde(default)]
    pub playback_mix: u8,
    #[serde(default)]
    pub reverb_on_output: bool,
    #[serde(default)]
    pub reverb_monitoring: bool,
    #[serde(default)]
    pub reverb_type: SerReverbType,
    #[serde(default = "default_reverb_intensity")]
    pub reverb_intensity: u8,
    // ── MV7+ LED ─────────────────────────────────────────────────────────────
    #[serde(default)]
    pub led_behavior: SerLedBehavior,
    #[serde(default)]
    pub led_brightness: SerLedBrightness,
    #[serde(default)]
    pub led_live_theme: SerLedLiveTheme,
    #[serde(default)]
    pub led_solid_theme: SerLedSolidTheme,
    #[serde(default)]
    pub led_pulsing_theme: SerLedPulsingTheme,
    #[serde(default = "default_led_solid_rgb")]
    pub led_solid_rgb: [u8; 3],
    #[serde(default = "default_led_pulsing_rgb")]
    pub led_pulsing_rgb: [u8; 3],
    #[serde(default = "default_led_live_edge_rgb")]
    pub led_live_edge_rgb: [u8; 3],
    #[serde(default = "default_led_live_middle_rgb")]
    pub led_live_middle_rgb: [u8; 3],
    #[serde(default = "default_led_live_interior_rgb")]
    pub led_live_interior_rgb: [u8; 3],
}

fn default_popper_stopper() -> bool {
    true
}

fn default_reverb_intensity() -> u8 {
    50
}

fn default_led_solid_rgb() -> [u8; 3] {
    [0xB2, 0xFF, 0x33]
}

fn default_led_pulsing_rgb() -> [u8; 3] {
    [0x10, 0x3F, 0xFB]
}

fn default_led_live_edge_rgb() -> [u8; 3] {
    [0xFF, 0xFF, 0xFF]
}

fn default_led_live_middle_rgb() -> [u8; 3] {
    [0x1F, 0x1F, 0x1F]
}

fn default_led_live_interior_rgb() -> [u8; 3] {
    [0x00, 0x00, 0x00]
}

impl PresetSlot {
    /// Build a preset snapshot from a live `DeviceState`.
    pub fn from_device_state(name: impl Into<String>, state: &DeviceState) -> Self {
        Self {
            name: name.into(),
            gain_db: state.gain_db,
            mode: SerInputMode::from(state.mode),
            muted: state.muted,
            hpf: SerHpfFrequency::from(state.hpf),
            auto_position: SerMicPosition::from(state.auto_position),
            auto_tone: SerAutoTone::from(state.auto_tone),
            auto_gain: SerAutoGain::from(state.auto_gain),
            phantom_power: state.phantom_power,
            monitor_mix: state.monitor_mix,
            limiter_enabled: state.limiter_enabled,
            compressor: SerCompressorPreset::from(state.compressor),
            eq_enabled: state.eq_enabled,
            eq_bands: state.eq_bands.map(SerEqBand::from),
            denoiser_enabled: state.denoiser_enabled,
            popper_stopper_enabled: state.popper_stopper_enabled,
            mute_btn_disabled: state.mute_btn_disabled,
            tone: state.tone,
            mv6_gain_locked: state.mv6_gain_locked,
            playback_mix: state.playback_mix,
            reverb_on_output: state.reverb_on_output,
            reverb_monitoring: state.reverb_monitoring,
            reverb_type: SerReverbType::from(state.reverb_type),
            reverb_intensity: state.reverb_intensity,
            led_behavior: SerLedBehavior::from(state.led_behavior),
            led_brightness: SerLedBrightness::from(state.led_brightness),
            led_live_theme: SerLedLiveTheme::from(state.led_live_theme),
            led_solid_theme: SerLedSolidTheme::from(state.led_solid_theme),
            led_pulsing_theme: SerLedPulsingTheme::from(state.led_pulsing_theme),
            led_solid_rgb: state.led_solid_rgb,
            led_pulsing_rgb: state.led_pulsing_rgb,
            led_live_edge_rgb: state.led_live_edge_rgb,
            led_live_middle_rgb: state.led_live_middle_rgb,
            led_live_interior_rgb: state.led_live_interior_rgb,
        }
    }

    /// Apply this preset's settings onto a `DeviceState`, preserving
    /// hardware-identity fields (`serial_number`).
    pub fn apply_to_device_state(&self, state: &mut DeviceState) {
        state.gain_db = self.gain_db;
        state.mode = InputMode::from(self.mode);
        state.muted = self.muted;
        state.hpf = HpfFrequency::from(self.hpf);
        state.auto_position = MicPosition::from(self.auto_position);
        state.auto_tone = AutoTone::from(self.auto_tone);
        state.auto_gain = AutoGain::from(self.auto_gain);
        state.phantom_power = self.phantom_power;
        state.monitor_mix = self.monitor_mix;
        state.limiter_enabled = self.limiter_enabled;
        state.compressor = CompressorPreset::from(self.compressor);
        state.eq_enabled = self.eq_enabled;
        state.eq_bands = self.eq_bands.map(EqBand::from);
        state.denoiser_enabled = self.denoiser_enabled;
        state.popper_stopper_enabled = self.popper_stopper_enabled;
        state.mute_btn_disabled = self.mute_btn_disabled;
        state.tone = self.tone;
        state.mv6_gain_locked = self.mv6_gain_locked;
        state.playback_mix = self.playback_mix;
        state.reverb_on_output = self.reverb_on_output;
        state.reverb_monitoring = self.reverb_monitoring;
        state.reverb_type = ReverbType::from(self.reverb_type);
        state.reverb_intensity = self.reverb_intensity;
        state.led_behavior = LedBehavior::from(self.led_behavior);
        state.led_brightness = LedBrightness::from(self.led_brightness);
        state.led_live_theme = LedLiveTheme::from(self.led_live_theme);
        state.led_solid_theme = LedSolidTheme::from(self.led_solid_theme);
        state.led_pulsing_theme = LedPulsingTheme::from(self.led_pulsing_theme);
        state.led_solid_rgb = self.led_solid_rgb;
        state.led_pulsing_rgb = self.led_pulsing_rgb;
        state.led_live_edge_rgb = self.led_live_edge_rgb;
        state.led_live_middle_rgb = self.led_live_middle_rgb;
        state.led_live_interior_rgb = self.led_live_interior_rgb;
    }

    /// Format the denoiser state as a display string.
    fn denoiser_str(&self) -> &'static str {
        if self.denoiser_enabled {
            "Denoiser on"
        } else {
            "Denoiser off"
        }
    }

    /// Format the popper stopper state as a display string.
    fn popper_str(&self) -> &'static str {
        if self.popper_stopper_enabled {
            "Popper on"
        } else {
            "Popper off"
        }
    }

    /// Format the tone value as a display string.
    fn tone_str(&self) -> String {
        match self.tone {
            0 => "Natural".to_string(),
            t if t > 0 => format!("{}% Bright", t as i32 * 10),
            t => format!("{}% Dark", (t as i32 * 10).abs()),
        }
    }

    /// One-line summary of the key settings for display in the TUI.
    ///
    /// The model is needed because MVX2U and MV6 have different configurable
    /// fields — showing EQ/phantom for an MV6 preset (or denoiser for an MVX2U
    /// preset) would be misleading.
    pub fn summary(&self, model: DeviceModel) -> String {
        let hpf_str = match HpfFrequency::from(self.hpf) {
            HpfFrequency::Off => "HPF off".to_string(),
            freq => format!("HPF {freq}"),
        };

        match model {
            DeviceModel::Mv6 => {
                let denoiser_str = self.denoiser_str();
                let popper_str = self.popper_str();
                let tone_str = self.tone_str();
                format!(
                    "{}dB · {denoiser_str} · {popper_str} · {hpf_str} · Tone: {tone_str}",
                    self.gain_db
                )
            }
            DeviceModel::Mv7Plus => {
                let denoiser_str = self.denoiser_str();
                let popper_str = self.popper_str();
                let tone_str = self.tone_str();
                let reverb_str = if self.reverb_on_output {
                    format!("Reverb: {} {}%", self.reverb_type, self.reverb_intensity)
                } else {
                    "Reverb: off".to_string()
                };
                format!(
                    "{}dB · {denoiser_str} · {popper_str} · {hpf_str} · Tone: {tone_str} · {reverb_str}",
                    self.gain_db
                )
            }
            DeviceModel::Mvx2uGen2 => {
                let phantom_str = if self.phantom_power {
                    "48V on"
                } else {
                    "48V off"
                };
                let denoiser_str = self.denoiser_str();
                let popper_str = self.popper_str();
                let tone_str = self.tone_str();
                match InputMode::from(self.mode) {
                    InputMode::Auto => {
                        format!(
                            "Auto · {phantom_str} · {denoiser_str} · {popper_str} · Tone: {tone_str}"
                        )
                    }
                    InputMode::Manual => {
                        let comp_str = CompressorPreset::from(self.compressor).to_string();
                        let limiter_str = if self.limiter_enabled {
                            "Limiter on"
                        } else {
                            "Limiter off"
                        };
                        format!(
                            "Manual · {}dB · {limiter_str} · Comp: {comp_str} · {phantom_str} · {denoiser_str} · {popper_str} · {hpf_str}",
                            self.gain_db
                        )
                    }
                }
            }
            DeviceModel::Mvx2u => {
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
                        format!(
                            "Manual · {}dB · {eq_str} · Comp: {comp_str} · {phantom_str} · {hpf_str}",
                            self.gain_db
                        )
                    }
                }
            }
        }
    }
}

// ── File I/O ──────────────────────────────────────────────────────────────────

/// Returns `~/.config/shurectl/`, creating it if absent.
fn config_dir() -> Result<PathBuf> {
    let base = dirs_next::config_dir()
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .context("Cannot determine config directory; set $HOME")?;
    let dir = base.join("shurectl");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create config directory: {}", dir.display()))?;
    Ok(dir)
}

/// Returns `~/.config/shurectl/presets/`, creating it if absent.
fn presets_dir() -> Result<PathBuf> {
    let dir = config_dir()?.join("presets");
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
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("Failed to read preset file: {}", path.display()));
        }
    };
    let slot: PresetSlot = toml::from_str(&text)
        .with_context(|| format!("Failed to parse preset file: {}", path.display()))?;
    Ok(Some(slot))
}

/// Save a preset to slot `index`, overwriting any existing file.
///
/// Uses a write-to-temp-then-rename pattern so a crash or power loss mid-write
/// cannot leave a corrupt or empty preset file behind.
pub fn save_preset(index: usize, slot: &PresetSlot) -> Result<()> {
    let path = preset_path(index)?;
    let text = toml::to_string_pretty(slot).context("Failed to serialise preset")?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &text)
        .with_context(|| format!("Failed to write preset file: {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("Failed to rename preset file: {}", path.display()))?;
    Ok(())
}

/// Delete the preset file for slot `index`. No-op if the slot is already empty.
pub fn delete_preset(index: usize) -> Result<()> {
    let path = preset_path(index)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            Err(e).with_context(|| format!("Failed to delete preset file: {}", path.display()))
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerMicPosition {
    #[default]
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

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerAutoTone {
    Dark,
    #[default]
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

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerAutoGain {
    Quiet,
    #[default]
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

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerCompressorPreset {
    #[default]
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

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct SerEqBand {
    pub enabled: bool,
    /// Gain in tenths of dB, range −80..+60.
    /// Stored as i16 so presets round-trip at 0.5 dB resolution for Gen 2.
    pub gain_db: i16,
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

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerReverbType {
    #[default]
    Plate,
    Hall,
    Studio,
}

impl std::fmt::Display for SerReverbType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SerReverbType::Plate => write!(f, "Plate"),
            SerReverbType::Hall => write!(f, "Hall"),
            SerReverbType::Studio => write!(f, "Studio"),
        }
    }
}

impl From<ReverbType> for SerReverbType {
    fn from(v: ReverbType) -> Self {
        match v {
            ReverbType::Plate => Self::Plate,
            ReverbType::Hall => Self::Hall,
            ReverbType::Studio => Self::Studio,
        }
    }
}

impl From<SerReverbType> for ReverbType {
    fn from(v: SerReverbType) -> Self {
        match v {
            SerReverbType::Plate => Self::Plate,
            SerReverbType::Hall => Self::Hall,
            SerReverbType::Studio => Self::Studio,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerLedBehavior {
    #[default]
    Live,
    Pulsing,
    Solid,
}

impl From<LedBehavior> for SerLedBehavior {
    fn from(v: LedBehavior) -> Self {
        match v {
            LedBehavior::Live => Self::Live,
            LedBehavior::Pulsing => Self::Pulsing,
            LedBehavior::Solid => Self::Solid,
        }
    }
}

impl From<SerLedBehavior> for LedBehavior {
    fn from(v: SerLedBehavior) -> Self {
        match v {
            SerLedBehavior::Live => Self::Live,
            SerLedBehavior::Pulsing => Self::Pulsing,
            SerLedBehavior::Solid => Self::Solid,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerLedBrightness {
    Low,
    Med,
    #[default]
    High,
    Max,
}

impl From<LedBrightness> for SerLedBrightness {
    fn from(v: LedBrightness) -> Self {
        match v {
            LedBrightness::Low => Self::Low,
            LedBrightness::Med => Self::Med,
            LedBrightness::High => Self::High,
            LedBrightness::Max => Self::Max,
        }
    }
}

impl From<SerLedBrightness> for LedBrightness {
    fn from(v: SerLedBrightness) -> Self {
        match v {
            SerLedBrightness::Low => Self::Low,
            SerLedBrightness::Med => Self::Med,
            SerLedBrightness::High => Self::High,
            SerLedBrightness::Max => Self::Max,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerLedLiveTheme {
    #[default]
    Default,
    Seaside,
    Space,
    Fruity,
    Custom,
}

impl From<LedLiveTheme> for SerLedLiveTheme {
    fn from(v: LedLiveTheme) -> Self {
        match v {
            LedLiveTheme::Default => Self::Default,
            LedLiveTheme::Seaside => Self::Seaside,
            LedLiveTheme::Space => Self::Space,
            LedLiveTheme::Fruity => Self::Fruity,
            LedLiveTheme::Custom => Self::Custom,
        }
    }
}

impl From<SerLedLiveTheme> for LedLiveTheme {
    fn from(v: SerLedLiveTheme) -> Self {
        match v {
            SerLedLiveTheme::Default => Self::Default,
            SerLedLiveTheme::Seaside => Self::Seaside,
            SerLedLiveTheme::Space => Self::Space,
            SerLedLiveTheme::Fruity => Self::Fruity,
            SerLedLiveTheme::Custom => Self::Custom,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerLedSolidTheme {
    #[default]
    Shure,
    Custom,
}

impl From<LedSolidTheme> for SerLedSolidTheme {
    fn from(v: LedSolidTheme) -> Self {
        match v {
            LedSolidTheme::Shure => Self::Shure,
            LedSolidTheme::Custom => Self::Custom,
        }
    }
}

impl From<SerLedSolidTheme> for LedSolidTheme {
    fn from(v: SerLedSolidTheme) -> Self {
        match v {
            SerLedSolidTheme::Shure => Self::Shure,
            SerLedSolidTheme::Custom => Self::Custom,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerLedPulsingTheme {
    #[default]
    Shure,
    Custom,
}

impl From<LedPulsingTheme> for SerLedPulsingTheme {
    fn from(v: LedPulsingTheme) -> Self {
        match v {
            LedPulsingTheme::Shure => Self::Shure,
            LedPulsingTheme::Custom => Self::Custom,
        }
    }
}

impl From<SerLedPulsingTheme> for LedPulsingTheme {
    fn from(v: SerLedPulsingTheme) -> Self {
        match v {
            SerLedPulsingTheme::Shure => Self::Shure,
            SerLedPulsingTheme::Custom => Self::Custom,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        LedBehavior, LedBrightness, LedLiveTheme, LedPulsingTheme, LedSolidTheme,
    };

    fn example_state() -> DeviceState {
        DeviceState {
            gain_db: 36,
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
                    gain_db: 40, // +4.0 dB in tenths
                },
                EqBand {
                    enabled: false,
                    gain_db: -20, // -2.0 dB in tenths
                },
                EqBand {
                    enabled: true,
                    gain_db: 0,
                },
                EqBand {
                    enabled: false,
                    gain_db: 60, // +6.0 dB in tenths
                },
                EqBand {
                    enabled: true,
                    gain_db: -80, // -8.0 dB in tenths
                },
            ],
            locked: false,
            denoiser_enabled: true,
            popper_stopper_enabled: false,
            mute_btn_disabled: true,
            tone: -5,
            mv6_gain_locked: false,
            playback_mix: 0,
            reverb_on_output: false,
            reverb_monitoring: false,
            reverb_type: ReverbType::Plate,
            reverb_intensity: 50,
            led_behavior: LedBehavior::Live,
            led_brightness: LedBrightness::High,
            led_live_theme: LedLiveTheme::Default,
            led_solid_theme: LedSolidTheme::Shure,
            led_pulsing_theme: LedPulsingTheme::Shure,
            led_solid_rgb: [0xB2, 0xFF, 0x33],
            led_pulsing_rgb: [0x10, 0x3F, 0xFB],
            led_live_edge_rgb: [0xFF, 0xFF, 0xFF],
            led_live_middle_rgb: [0x1F, 0x1F, 0x1F],
            led_live_interior_rgb: [0x00, 0x00, 0x00],
            serial_number: String::from("TEST001"),
        }
    }

    /// Serialise `slot` to TOML and deserialise it again, returning the decoded copy.
    fn toml_roundtrip(slot: &PresetSlot) -> PresetSlot {
        let toml_str = toml::to_string_pretty(slot).expect("serialise");
        toml::from_str(&toml_str).expect("deserialise")
    }

    /// Write `slot` to a temp file and reload it, returning the loaded copy.
    fn write_and_reload(slot: &PresetSlot) -> PresetSlot {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("preset_1.toml");
        let text = toml::to_string_pretty(slot).expect("serialise");
        std::fs::write(&path, &text).expect("write");
        toml::from_str(&std::fs::read_to_string(&path).expect("read")).expect("deserialise")
    }

    #[test]
    fn preset_slot_roundtrip_toml() {
        let state = example_state();
        let slot = PresetSlot::from_device_state("My Preset", &state);

        let decoded = toml_roundtrip(&slot);

        assert_eq!(slot, decoded);
        assert_eq!(decoded.name, "My Preset");
        assert_eq!(decoded.gain_db, 36);
        assert!(decoded.muted);
        assert_eq!(decoded.eq_bands[0].gain_db, 40); // +4.0 dB in tenths
        assert_eq!(decoded.eq_bands[4].gain_db, -80); // -8.0 dB in tenths
        assert!(decoded.denoiser_enabled);
        assert!(!decoded.popper_stopper_enabled);
        assert_eq!(decoded.tone, -5);
    }

    #[test]
    fn apply_to_device_state_restores_all_fields() {
        let original = example_state();
        let slot = PresetSlot::from_device_state("Test", &original);

        let mut target = DeviceState::default();
        target.serial_number = String::from("OTHER");

        slot.apply_to_device_state(&mut target);

        assert_eq!(target.gain_db, 36);
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
        assert_eq!(target.eq_bands[0].gain_db, 40); // +4.0 dB in tenths
        assert!(target.denoiser_enabled);
        assert!(!target.popper_stopper_enabled);
        assert!(target.mute_btn_disabled);
        assert_eq!(target.tone, -5);
        // Identity fields must be untouched.
        assert_eq!(target.serial_number, "OTHER");
    }

    #[test]
    fn save_and_load_preset_roundtrip() {
        let state = example_state();
        let slot = PresetSlot::from_device_state("Roundtrip", &state);

        let loaded = write_and_reload(&slot);

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
        let s = slot.summary(DeviceModel::Mvx2u);
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
        let s = slot.summary(DeviceModel::Mvx2u);
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

    fn mv6_example_state() -> DeviceState {
        DeviceState {
            gain_db: 24,
            mode: InputMode::Manual,
            auto_position: MicPosition::Near,
            auto_tone: AutoTone::Natural,
            auto_gain: AutoGain::Normal,
            muted: false,
            phantom_power: false,
            monitor_mix: 62,
            limiter_enabled: false,
            compressor: CompressorPreset::Off,
            hpf: HpfFrequency::Hz75,
            eq_enabled: false,
            eq_bands: [EqBand::default(); 5],
            locked: false,
            denoiser_enabled: true,
            popper_stopper_enabled: true,
            mute_btn_disabled: true,
            tone: 5,
            mv6_gain_locked: false,
            playback_mix: 0,
            reverb_on_output: false,
            reverb_monitoring: false,
            reverb_type: ReverbType::Plate,
            reverb_intensity: 50,
            led_behavior: LedBehavior::Live,
            led_brightness: LedBrightness::High,
            led_live_theme: LedLiveTheme::Default,
            led_solid_theme: LedSolidTheme::Shure,
            led_pulsing_theme: LedPulsingTheme::Shure,
            led_solid_rgb: [0xB2, 0xFF, 0x33],
            led_pulsing_rgb: [0x10, 0x3F, 0xFB],
            led_live_edge_rgb: [0xFF, 0xFF, 0xFF],
            led_live_middle_rgb: [0x1F, 0x1F, 0x1F],
            led_live_interior_rgb: [0x00, 0x00, 0x00],
            serial_number: String::from("MV6TEST"),
        }
    }

    #[test]
    fn mv6_preset_roundtrip_toml() {
        let state = mv6_example_state();
        let slot = PresetSlot::from_device_state("MV6 Preset", &state);

        let decoded = toml_roundtrip(&slot);

        assert_eq!(slot, decoded);
        assert_eq!(decoded.name, "MV6 Preset");
        assert_eq!(decoded.gain_db, 24);
        assert!(decoded.denoiser_enabled);
        assert!(decoded.popper_stopper_enabled);
        assert!(decoded.mute_btn_disabled);
        assert_eq!(decoded.tone, 5);
        assert_eq!(decoded.hpf, SerHpfFrequency::Hz75);
        assert_eq!(
            decoded.monitor_mix, 62,
            "monitor_mix must survive TOML roundtrip"
        );
    }

    #[test]
    fn mv6_apply_to_device_state_restores_mv6_fields() {
        let original = mv6_example_state();
        let slot = PresetSlot::from_device_state("MV6 Test", &original);

        let mut target = DeviceState::default();
        target.serial_number = String::from("OTHER");

        slot.apply_to_device_state(&mut target);

        assert_eq!(target.gain_db, 24);
        assert!(target.denoiser_enabled);
        assert!(target.popper_stopper_enabled);
        assert!(target.mute_btn_disabled);
        assert_eq!(target.tone, 5);
        assert_eq!(target.hpf, HpfFrequency::Hz75);
        // Identity fields must be untouched.
        assert_eq!(target.serial_number, "OTHER");
    }

    #[test]
    fn mv6_apply_to_device_state_no_duplicate_hpf() {
        // Verifies the duplicate hpf assignment is gone: applying a preset with
        // Hz150 must result in Hz150, not whatever was there before.
        let mut state = mv6_example_state();
        state.hpf = HpfFrequency::Hz150;
        let slot = PresetSlot::from_device_state("HPF test", &state);

        let mut target = DeviceState::default();
        // target.hpf starts as HpfFrequency::Off (default).
        slot.apply_to_device_state(&mut target);

        assert_eq!(target.hpf, HpfFrequency::Hz150);
    }

    #[test]
    fn mv6_mute_btn_disabled_roundtrip_file() {
        let state = mv6_example_state(); // mute_btn_disabled = true
        let slot = PresetSlot::from_device_state("MV6 File", &state);

        let loaded = write_and_reload(&slot);

        assert!(
            loaded.mute_btn_disabled,
            "mute_btn_disabled must survive a file roundtrip"
        );
        assert_eq!(loaded.tone, 5, "tone must survive a file roundtrip");
        assert!(
            loaded.denoiser_enabled,
            "denoiser_enabled must survive a file roundtrip"
        );
        assert!(
            loaded.popper_stopper_enabled,
            "popper_stopper_enabled must survive a file roundtrip"
        );
    }

    #[test]
    fn summary_mv6_shows_denoiser_popper_tone_hpf_not_phantom_eq_comp() {
        let mut state = mv6_example_state();
        state.denoiser_enabled = true;
        state.popper_stopper_enabled = false;
        state.tone = 5; // +50% Bright
        state.hpf = HpfFrequency::Hz75;
        let slot = PresetSlot::from_device_state("S", &state);
        let s = slot.summary(DeviceModel::Mv6);
        assert!(s.contains("24dB"), "summary: {s}");
        assert!(s.contains("Denoiser on"), "summary: {s}");
        assert!(s.contains("Popper off"), "summary: {s}");
        assert!(s.contains("HPF 75 Hz"), "summary: {s}");
        assert!(s.contains("50% Bright"), "summary: {s}");
        // MVX2U-specific fields must not appear
        assert!(
            !s.contains("48V"),
            "MV6 summary must not mention phantom: {s}"
        );
        assert!(!s.contains("EQ"), "MV6 summary must not mention EQ: {s}");
        assert!(
            !s.contains("Comp"),
            "MV6 summary must not mention Comp: {s}"
        );
    }

    #[test]
    fn summary_mv6_tone_natural_and_dark() {
        let mut state = mv6_example_state();
        state.tone = 0;
        let slot = PresetSlot::from_device_state("S", &state);
        assert!(
            slot.summary(DeviceModel::Mv6).contains("Natural"),
            "zero tone should be Natural"
        );

        let mut state2 = mv6_example_state();
        state2.tone = -3; // -30% Dark
        let slot2 = PresetSlot::from_device_state("S", &state2);
        assert!(
            slot2.summary(DeviceModel::Mv6).contains("30% Dark"),
            "negative tone should be Dark"
        );
    }
}
