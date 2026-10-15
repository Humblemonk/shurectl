//! Host-side preset storage for shurectl.
//!
//! Presets are TOML files in `shurectl/presets/` under the platform config
//! directory (`~/.config` on Linux; the README lists the others).
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
    AutoGain, AutoTone, CompressorPreset, DeviceModel, DeviceState, EqBand, EqPreset, HpfFrequency,
    InputMode, LedBehavior, LedBrightness, LedLiveTheme, LedPulsingTheme, LedSolidTheme,
    MicPosition, ReverbType, format_gain,
};

pub const PRESET_COUNT: usize = 4;

/// A serializable snapshot of all configurable device settings.
///
/// Fields from every model are included. Fields irrelevant to a given
/// device model remain at their serde defaults and are not applied to the device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresetSlot {
    /// Human-readable name shown in the TUI (editable in-app or by hand in TOML).
    pub name: String,

    // ── Shared ───────────────────────────────────────────────────────────────
    /// Manual gain in dB, e.g. `19.5` (the MV7 steps 1.5 dB). Presets saved
    /// before fractional gain stored a whole number here, which still loads.
    pub gain_db: f32,
    pub mode: SerInputMode,
    pub muted: bool,
    pub hpf: SerHpfFrequency,

    // ── MVX2U-first (several shared with other models; see DeviceState) ─────
    #[serde(default)]
    pub auto_position: SerMicPosition,
    #[serde(default)]
    pub auto_tone: SerAutoTone,
    #[serde(default)]
    pub auto_gain: SerAutoGain,
    /// 48V phantom power, saved only on models with `has_phantom_power()`.
    /// `None` keeps the device's current setting on load, so a preset from a
    /// mic without phantom power can't switch it off under a condenser mic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phantom_power: Option<bool>,
    /// Monitor Mix, as `DeviceState::monitor_mix`: one mic ↔ playback balance,
    /// or the Mic Level on models with `has_playback_mix()`.
    #[serde(default)]
    pub monitor_mix: u8,
    /// Which of those `monitor_mix` is: `Some(true)` when saved on a model with
    /// `has_playback_mix()`. Loading skips a Monitor Mix of the other kind, since
    /// a balance of 0 (all mic) would mute the mic as a Mic Level. `None` in
    /// presets saved before this field, which load as they always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor_mix_is_mic_level: Option<bool>,
    /// Playback Level in the headphones, 1–100%, on models with
    /// `has_playback_mix()`. Loading treats 0 as "not saved" and keeps the
    /// device's current level.
    #[serde(default)]
    pub playback_mix: u8,
    #[serde(default)]
    pub limiter_enabled: bool,
    #[serde(default)]
    pub compressor: SerCompressorPreset,
    #[serde(default)]
    pub eq_enabled: bool,
    #[serde(default)]
    pub eq_bands: [SerEqBand; 5],

    // ── MV6-first (also MVX2U Gen 2, MV6 Gen 2, MV7+; see DeviceState) ───────
    #[serde(default)]
    pub denoiser_enabled: bool,
    #[serde(default = "default_popper_stopper")]
    pub popper_stopper_enabled: bool,
    #[serde(default)]
    pub mute_btn_disabled: bool,
    /// Tone: -10 (100% Dark) to +10 (100% Bright), 0 = Natural.
    #[serde(default)]
    pub tone: i8,
    /// Gain lock (Manual mode only), on models with `has_gain_lock()`.
    #[serde(default)]
    pub mv6_gain_locked: bool,

    // ── MV6 Gen 2-specific (Target Level is `auto_gain`) ─────────────────────
    #[serde(default)]
    pub adaptation_rate_fast: bool,
    /// Denoiser level, 1–100%.
    #[serde(default = "default_denoiser_level")]
    pub denoiser_level: u8,
    /// Advanced tone (the 5-band `eq_bands`) instead of the Tone slider.
    #[serde(default)]
    pub tone_advanced: bool,

    // ── MV7+-specific ────────────────────────────────────────────────────────
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

    // ── MV7-specific ─────────────────────────────────────────────────────────
    #[serde(default)]
    pub eq_preset: SerEqPreset,
    #[serde(default = "default_led_live_meter")]
    pub led_live_meter: bool,
    #[serde(default)]
    pub led_night_mode: bool,
}

fn default_led_live_meter() -> bool {
    true
}

/// Preset gain (dB) → tenths of a dB, rounded and clamped to the `u16` range.
/// Model-specific ceilings are applied when the gain is sent to the device.
fn gain_db_to_tenths(gain_db: f32) -> u16 {
    (gain_db * 10.0).round().clamp(0.0, f32::from(u16::MAX)) as u16
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

fn default_denoiser_level() -> u8 {
    50
}

fn default_led_live_middle_rgb() -> [u8; 3] {
    [0x1F, 0x1F, 0x1F]
}

fn default_led_live_interior_rgb() -> [u8; 3] {
    [0x00, 0x00, 0x00]
}

impl PresetSlot {
    /// Build a preset snapshot from a live `DeviceState` on `model`, which
    /// decides what kind of Monitor Mix it records.
    pub fn from_device_state(
        name: impl Into<String>,
        state: &DeviceState,
        model: DeviceModel,
    ) -> Self {
        Self {
            name: name.into(),
            gain_db: f32::from(state.gain_tenths) / 10.0,
            mode: SerInputMode::from(state.mode),
            muted: state.muted,
            hpf: SerHpfFrequency::from(state.hpf),
            auto_position: SerMicPosition::from(state.auto_position),
            auto_tone: SerAutoTone::from(state.auto_tone),
            auto_gain: SerAutoGain::from(state.auto_gain),
            phantom_power: model.has_phantom_power().then_some(state.phantom_power),
            monitor_mix: state.monitor_mix,
            monitor_mix_is_mic_level: Some(model.has_playback_mix()),
            limiter_enabled: state.limiter_enabled,
            compressor: SerCompressorPreset::from(state.compressor),
            eq_enabled: state.eq_enabled,
            eq_bands: state.eq_bands.map(SerEqBand::from),
            denoiser_enabled: state.denoiser_enabled,
            popper_stopper_enabled: state.popper_stopper_enabled,
            mute_btn_disabled: state.mute_btn_disabled,
            tone: state.tone,
            mv6_gain_locked: state.mv6_gain_locked,
            adaptation_rate_fast: state.adaptation_rate_fast,
            denoiser_level: state.denoiser_level,
            tone_advanced: state.tone_advanced,
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
            eq_preset: SerEqPreset::from(state.eq_preset),
            led_live_meter: state.led_live_meter,
            led_night_mode: state.led_night_mode,
        }
    }

    /// Apply this preset's settings onto a `DeviceState`, preserving
    /// hardware-identity fields (`serial_number`). Presets are hand-editable, so
    /// bounded values are clamped to the range the controls work in.
    pub fn apply_to_device_state(&self, state: &mut DeviceState) {
        state.gain_tenths = gain_db_to_tenths(self.gain_db);
        state.mode = InputMode::from(self.mode);
        state.muted = self.muted;
        state.hpf = HpfFrequency::from(self.hpf);
        state.auto_position = MicPosition::from(self.auto_position);
        state.auto_tone = AutoTone::from(self.auto_tone);
        state.auto_gain = AutoGain::from(self.auto_gain);
        if let Some(phantom_power) = self.phantom_power {
            state.phantom_power = phantom_power;
        }
        state.monitor_mix = self.monitor_mix.min(100);
        state.limiter_enabled = self.limiter_enabled;
        state.compressor = CompressorPreset::from(self.compressor);
        state.eq_enabled = self.eq_enabled;
        state.eq_bands = self.eq_bands.map(|band| EqBand {
            gain_db: band.gain_db.clamp(-80, 60),
            ..EqBand::from(band)
        });
        state.denoiser_enabled = self.denoiser_enabled;
        state.popper_stopper_enabled = self.popper_stopper_enabled;
        state.mute_btn_disabled = self.mute_btn_disabled;
        state.tone = self.tone.clamp(-10, 10);
        state.mv6_gain_locked = self.mv6_gain_locked;
        state.adaptation_rate_fast = self.adaptation_rate_fast;
        state.denoiser_level = self.denoiser_level.clamp(1, 100);
        state.tone_advanced = self.tone_advanced;
        // 0 is what every preset holds that wasn't saved on a model reading
        // Playback Mix (and MV6 presets from before it did), so it means "not
        // saved": keep the current level rather than silence headphone playback.
        if self.playback_mix > 0 {
            state.playback_mix = self.playback_mix.min(100);
        }
        state.reverb_on_output = self.reverb_on_output;
        state.reverb_monitoring = self.reverb_monitoring;
        state.reverb_type = ReverbType::from(self.reverb_type);
        state.reverb_intensity = self.reverb_intensity.min(100);
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
        state.eq_preset = EqPreset::from(self.eq_preset);
        state.led_live_meter = self.led_live_meter;
        state.led_night_mode = self.led_night_mode;
    }

    /// The preset's gain formatted the same way as everywhere else in the UI.
    fn gain_str(&self) -> String {
        format_gain(gain_db_to_tenths(self.gain_db))
    }

    /// The phantom power part of a summary, `" · 48V on"`, or nothing when the
    /// preset leaves phantom power as it is.
    fn phantom_part(&self) -> String {
        match self.phantom_power {
            Some(true) => " · 48V on".to_string(),
            Some(false) => " · 48V off".to_string(),
            None => String::new(),
        }
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
    /// The model is needed because models have different configurable fields —
    /// showing phantom power on an MV6 (or reverb on an MVX2U) would be
    /// misleading.
    pub fn summary(&self, model: DeviceModel) -> String {
        let hpf_str = match HpfFrequency::from(self.hpf) {
            HpfFrequency::Off => "HPF off".to_string(),
            freq => format!("HPF {freq}"),
        };

        match model {
            DeviceModel::Mv6Gen2 => {
                // Auto shows the Auto Level settings in place of the unused gain.
                let level_str = match InputMode::from(self.mode) {
                    InputMode::Auto => format!(
                        "Auto · Target {} · {}",
                        AutoGain::from(self.auto_gain).target_level_label(),
                        if self.adaptation_rate_fast {
                            "Fast"
                        } else {
                            "Slow"
                        }
                    ),
                    InputMode::Manual => self.gain_str(),
                };
                let denoiser_str = if self.denoiser_enabled {
                    format!("Denoiser {}%", self.denoiser_level)
                } else {
                    "Denoiser off".to_string()
                };
                let tone_str = if self.tone_advanced {
                    "Tone: 5-band EQ".to_string()
                } else {
                    format!("Tone: {}", self.tone_str())
                };
                format!(
                    "{level_str} · {denoiser_str} · {} · {hpf_str} · {tone_str}",
                    self.popper_str()
                )
            }
            DeviceModel::Mv6 => {
                let denoiser_str = self.denoiser_str();
                let popper_str = self.popper_str();
                let tone_str = self.tone_str();
                format!(
                    "{} · {denoiser_str} · {popper_str} · {hpf_str} · Tone: {tone_str}",
                    self.gain_str()
                )
            }
            DeviceModel::Mv7 => match InputMode::from(self.mode) {
                InputMode::Auto => {
                    let pos = MicPosition::from(self.auto_position);
                    let tone = AutoTone::from(self.auto_tone);
                    format!("Auto · {pos} · {tone}")
                }
                InputMode::Manual => {
                    let comp_str = CompressorPreset::from(self.compressor).to_string();
                    let eq_str = EqPreset::from(self.eq_preset).to_string();
                    format!(
                        "Manual · {} · Comp: {comp_str} · EQ: {eq_str}",
                        self.gain_str()
                    )
                }
            },
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
                    "{} · {denoiser_str} · {popper_str} · {hpf_str} · Tone: {tone_str} · {reverb_str}",
                    self.gain_str()
                )
            }
            DeviceModel::Mvx2uGen2 => {
                let phantom = self.phantom_part();
                let denoiser_str = self.denoiser_str();
                let popper_str = self.popper_str();
                let tone_str = self.tone_str();
                match InputMode::from(self.mode) {
                    InputMode::Auto => {
                        format!("Auto{phantom} · {denoiser_str} · {popper_str} · Tone: {tone_str}")
                    }
                    InputMode::Manual => {
                        let comp_str = CompressorPreset::from(self.compressor).to_string();
                        let limiter_str = if self.limiter_enabled {
                            "Limiter on"
                        } else {
                            "Limiter off"
                        };
                        format!(
                            "Manual · {} · {limiter_str} · Comp: {comp_str}{phantom} · {denoiser_str} · {popper_str} · {hpf_str}",
                            self.gain_str()
                        )
                    }
                }
            }
            DeviceModel::Mvx2u => {
                let phantom = self.phantom_part();
                match InputMode::from(self.mode) {
                    InputMode::Auto => {
                        let tone = AutoTone::from(self.auto_tone);
                        let gain = AutoGain::from(self.auto_gain);
                        let pos = MicPosition::from(self.auto_position);
                        format!("Auto · {tone} · {gain} · {pos}{phantom}")
                    }
                    InputMode::Manual => {
                        let eq_str = if self.eq_enabled { "EQ on" } else { "EQ off" };
                        let comp_str = CompressorPreset::from(self.compressor).to_string();
                        format!(
                            "Manual · {} · {eq_str} · Comp: {comp_str}{phantom} · {hpf_str}",
                            self.gain_str()
                        )
                    }
                }
            }
        }
    }
}

// ── File I/O ──────────────────────────────────────────────────────────────────

/// Returns `shurectl/` under the platform config directory, creating it if absent.
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

/// Returns the `presets/` directory inside `config_dir()`, creating it if absent.
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
pub enum SerEqPreset {
    #[default]
    Flat,
    HighPass,
    PresenceBoost,
    HighPassPresenceBoost,
}

impl From<EqPreset> for SerEqPreset {
    fn from(v: EqPreset) -> Self {
        match v {
            EqPreset::Flat => Self::Flat,
            EqPreset::HighPass => Self::HighPass,
            EqPreset::PresenceBoost => Self::PresenceBoost,
            EqPreset::HighPassPresenceBoost => Self::HighPassPresenceBoost,
        }
    }
}

impl From<SerEqPreset> for EqPreset {
    fn from(v: SerEqPreset) -> Self {
        match v {
            SerEqPreset::Flat => Self::Flat,
            SerEqPreset::HighPass => Self::HighPass,
            SerEqPreset::PresenceBoost => Self::PresenceBoost,
            SerEqPreset::HighPassPresenceBoost => Self::HighPassPresenceBoost,
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

    /// A fully populated state whose every field differs from `DeviceState::default()`
    /// where the roundtrip tests care, so a dropped field shows up as a mismatch.
    fn example_state() -> DeviceState {
        DeviceState {
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
            denoiser_enabled: true,
            popper_stopper_enabled: false,
            mute_btn_disabled: true,
            tone: -5,
            serial_number: String::from("TEST001"),
            ..DeviceState::default()
        }
    }

    /// Serialise `slot` to TOML and deserialise it again, returning the decoded copy.
    fn toml_roundtrip(slot: &PresetSlot) -> PresetSlot {
        let toml_str = toml::to_string_pretty(slot).expect("serialise");
        toml::from_str(&toml_str).expect("deserialise")
    }

    #[test]
    fn monitor_mix_kind_is_recorded_and_absent_in_older_files() {
        let state = DeviceState::default();
        let balance = PresetSlot::from_device_state("S", &state, DeviceModel::Mvx2u);
        assert_eq!(
            toml_roundtrip(&balance).monitor_mix_is_mic_level,
            Some(false)
        );
        let mic_level = PresetSlot::from_device_state("S", &state, DeviceModel::Mv7Plus);
        assert_eq!(
            toml_roundtrip(&mic_level).monitor_mix_is_mic_level,
            Some(true)
        );
        let older = PresetSlot {
            monitor_mix_is_mic_level: None,
            ..balance
        };
        assert_eq!(toml_roundtrip(&older).monitor_mix_is_mic_level, None);
    }

    #[test]
    fn phantom_power_is_saved_only_where_it_exists_and_kept_otherwise() {
        let state = example_state();
        let interface = PresetSlot::from_device_state("S", &state, DeviceModel::Mvx2uGen2);
        assert_eq!(toml_roundtrip(&interface).phantom_power, Some(true));

        let mic = PresetSlot::from_device_state("S", &state, DeviceModel::Mv7Plus);
        let text = toml::to_string_pretty(&mic).expect("serialise");
        assert!(!text.contains("phantom_power"), "{text}");
        assert_eq!(toml_roundtrip(&mic).phantom_power, None);
        assert!(!mic.summary(DeviceModel::Mvx2u).contains("48V"));

        // Loading a mic's preset onto an interface leaves 48V on.
        let mut target = DeviceState {
            phantom_power: true,
            ..DeviceState::default()
        };
        mic.apply_to_device_state(&mut target);
        assert!(target.phantom_power);

        // Files saved before this still carry the setting and apply it.
        let older = text.replacen("muted = true", "muted = true\nphantom_power = false", 1);
        let older: PresetSlot = toml::from_str(&older).expect("deserialise");
        assert_eq!(older.phantom_power, Some(false));
        older.apply_to_device_state(&mut target);
        assert!(!target.phantom_power);
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
        let slot = PresetSlot::from_device_state("My Preset", &state, DeviceModel::Mvx2u);

        let decoded = toml_roundtrip(&slot);

        assert_eq!(slot, decoded);
        assert_eq!(decoded.name, "My Preset");
        assert_eq!(decoded.gain_db, 36.0);
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
        let slot = PresetSlot::from_device_state("Test", &original, DeviceModel::Mvx2u);

        let mut target = DeviceState::default();
        target.serial_number = String::from("OTHER");

        slot.apply_to_device_state(&mut target);

        assert_eq!(target.gain_tenths, 360);
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
        let slot = PresetSlot::from_device_state("Roundtrip", &state, DeviceModel::Mvx2u);

        let loaded = write_and_reload(&slot);

        assert_eq!(slot, loaded);
    }

    #[test]
    fn summary_manual_mode_contains_gain_eq_dynamics_phantom_hpf() {
        let mut state = example_state();
        state.mode = InputMode::Manual;
        state.gain_tenths = 360;
        state.phantom_power = true;
        state.hpf = HpfFrequency::Hz75;
        let slot = PresetSlot::from_device_state("S", &state, DeviceModel::Mvx2u);
        let s = slot.summary(DeviceModel::Mvx2u);
        assert!(s.contains("Manual"), "summary: {s}");
        assert!(s.contains("36 dB"), "summary: {s}");
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
        let slot = PresetSlot::from_device_state("S", &state, DeviceModel::Mvx2u);
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
            gain_tenths: 240,
            mode: InputMode::Manual,
            monitor_mix: 62,
            hpf: HpfFrequency::Hz75,
            denoiser_enabled: true,
            mute_btn_disabled: true,
            tone: 5,
            serial_number: String::from("MV6TEST"),
            ..DeviceState::default()
        }
    }

    #[test]
    fn mv6_preset_roundtrip_toml() {
        let state = mv6_example_state();
        let slot = PresetSlot::from_device_state("MV6 Preset", &state, DeviceModel::Mv6);

        let decoded = toml_roundtrip(&slot);

        assert_eq!(slot, decoded);
        assert_eq!(decoded.name, "MV6 Preset");
        assert_eq!(decoded.gain_db, 24.0);
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
        let slot = PresetSlot::from_device_state("MV6 Test", &original, DeviceModel::Mv6);

        let mut target = DeviceState::default();
        target.serial_number = String::from("OTHER");

        slot.apply_to_device_state(&mut target);

        assert_eq!(target.gain_tenths, 240);
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
        let slot = PresetSlot::from_device_state("HPF test", &state, DeviceModel::Mv6);

        let mut target = DeviceState::default();
        // target.hpf starts as HpfFrequency::Off (default).
        slot.apply_to_device_state(&mut target);

        assert_eq!(target.hpf, HpfFrequency::Hz150);
    }

    #[test]
    fn mv6_mute_btn_disabled_roundtrip_file() {
        let state = mv6_example_state(); // mute_btn_disabled = true
        let slot = PresetSlot::from_device_state("MV6 File", &state, DeviceModel::Mv6);

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
        let slot = PresetSlot::from_device_state("S", &state, DeviceModel::Mv6);
        let s = slot.summary(DeviceModel::Mv6);
        assert!(s.contains("24 dB"), "summary: {s}");
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
    fn mv6_gen2_settings_roundtrip_and_old_presets_get_defaults() {
        let state = DeviceState {
            auto_gain: AutoGain::Loud,
            adaptation_rate_fast: true,
            denoiser_level: 37,
            tone_advanced: true,
            ..mv6_example_state()
        };
        let slot = PresetSlot::from_device_state("G2", &state, DeviceModel::Mv6Gen2);
        let parsed: PresetSlot =
            toml::from_str(&toml::to_string(&slot).expect("serialize")).expect("parse");
        let mut restored = DeviceState::default();
        parsed.apply_to_device_state(&mut restored);
        assert_eq!(restored.auto_gain, AutoGain::Loud);
        assert!(restored.adaptation_rate_fast);
        assert_eq!(restored.denoiser_level, 37);
        assert!(restored.tone_advanced);
        assert!(
            parsed
                .summary(DeviceModel::Mv6Gen2)
                .ends_with("Tone: 5-band EQ"),
            "{}",
            parsed.summary(DeviceModel::Mv6Gen2)
        );

        // A preset saved before the MV6 Gen 2 fields existed loads the defaults.
        let old = toml::to_string(&slot)
            .expect("serialize")
            .lines()
            .filter(|line| {
                !["adaptation_rate_fast", "denoiser_level", "tone_advanced"]
                    .iter()
                    .any(|field| line.starts_with(field))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let parsed: PresetSlot = toml::from_str(&old).expect("parse old preset");
        assert!(!parsed.adaptation_rate_fast);
        assert_eq!(parsed.denoiser_level, 50);
        assert!(!parsed.tone_advanced, "older presets keep the Tone slider");

        // A hand-edited level of 0 is clamped onto the 1–100% slider.
        let mut zero = parsed;
        zero.denoiser_level = 0;
        zero.apply_to_device_state(&mut restored);
        assert_eq!(restored.denoiser_level, 1);
    }

    #[test]
    fn summary_mv6_gen2_shows_denoiser_level_and_auto_level_settings() {
        let mut state = mv6_example_state();
        state.denoiser_level = 64;
        let manual = PresetSlot::from_device_state("S", &state, DeviceModel::Mv6Gen2)
            .summary(DeviceModel::Mv6Gen2);
        assert_eq!(
            manual,
            "24 dB · Denoiser 64% · Popper on · HPF 75 Hz · Tone: 50% Bright"
        );

        state.mode = InputMode::Auto;
        state.auto_gain = AutoGain::Quiet;
        state.denoiser_enabled = false;
        let auto = PresetSlot::from_device_state("S", &state, DeviceModel::Mv6Gen2)
            .summary(DeviceModel::Mv6Gen2);
        assert!(
            auto.starts_with("Auto · Target Low · Slow · Denoiser off"),
            "{auto}"
        );
    }

    #[test]
    fn summary_mv6_tone_natural_and_dark() {
        let mut state = mv6_example_state();
        state.tone = 0;
        let slot = PresetSlot::from_device_state("S", &state, DeviceModel::Mv6);
        assert!(
            slot.summary(DeviceModel::Mv6).contains("Natural"),
            "zero tone should be Natural"
        );

        let mut state2 = mv6_example_state();
        state2.tone = -3; // -30% Dark
        let slot2 = PresetSlot::from_device_state("S", &state2, DeviceModel::Mv6);
        assert!(
            slot2.summary(DeviceModel::Mv6).contains("30% Dark"),
            "negative tone should be Dark"
        );
    }

    #[test]
    fn fractional_gain_roundtrips_through_toml() {
        // The MV7's 1.5 dB grid produces gains like 19.5 dB.
        let state = DeviceState {
            gain_tenths: 195,
            ..DeviceState::default()
        };
        let slot = PresetSlot::from_device_state("MV7", &state, DeviceModel::Mv7);
        let decoded = toml_roundtrip(&slot);
        assert_eq!(decoded.gain_db, 19.5);

        let mut target = DeviceState::default();
        decoded.apply_to_device_state(&mut target);
        assert_eq!(target.gain_tenths, 195);
    }

    #[test]
    fn preset_saved_with_whole_db_gain_still_loads() {
        let slot =
            PresetSlot::from_device_state("Old", &DeviceState::default(), DeviceModel::Mvx2u);
        let text = toml::to_string_pretty(&slot)
            .expect("serialise")
            .replace("gain_db = 36.0", "gain_db = 36");
        assert!(text.contains("gain_db = 36\n"), "fixture: {text}");
        let loaded: PresetSlot = toml::from_str(&text).expect("old format must parse");
        let mut target = DeviceState::default();
        loaded.apply_to_device_state(&mut target);
        assert_eq!(target.gain_tenths, 360);
    }

    #[test]
    fn preset_without_mv7_fields_loads_with_defaults() {
        let slot =
            PresetSlot::from_device_state("Pre-MV7", &DeviceState::default(), DeviceModel::Mv7);
        let text: String = toml::to_string_pretty(&slot)
            .expect("serialise")
            .lines()
            .filter(|line| {
                !line.starts_with("eq_preset")
                    && !line.starts_with("led_live_meter")
                    && !line.starts_with("led_night_mode")
            })
            .map(|line| format!("{line}\n"))
            .collect();
        let loaded: PresetSlot = toml::from_str(&text).expect("parse");
        assert_eq!(loaded.eq_preset, SerEqPreset::Flat);
        assert!(loaded.led_live_meter);
        assert!(!loaded.led_night_mode);
    }

    fn mv7_example_state() -> DeviceState {
        DeviceState {
            gain_tenths: 195,
            mode: InputMode::Manual,
            monitor_mix: 74,
            compressor: CompressorPreset::Light,
            eq_preset: EqPreset::HighPassPresenceBoost,
            led_live_meter: false,
            led_night_mode: true,
            ..DeviceState::default()
        }
    }

    #[test]
    fn mv7_preset_roundtrips_mv7_fields() {
        let original = mv7_example_state();
        let slot = PresetSlot::from_device_state("MV7", &original, DeviceModel::Mv7);
        let loaded = write_and_reload(&slot);
        assert_eq!(slot, loaded);

        let mut target = DeviceState::default();
        loaded.apply_to_device_state(&mut target);
        assert_eq!(target.gain_tenths, 195);
        assert_eq!(target.monitor_mix, 74);
        assert_eq!(target.compressor, CompressorPreset::Light);
        assert_eq!(target.eq_preset, EqPreset::HighPassPresenceBoost);
        assert!(!target.led_live_meter);
        assert!(target.led_night_mode);
    }

    #[test]
    fn summary_mv7_manual_shows_gain_compressor_and_eq() {
        let slot = PresetSlot::from_device_state("S", &mv7_example_state(), DeviceModel::Mv7);
        let s = slot.summary(DeviceModel::Mv7);
        assert!(s.contains("Manual"), "summary: {s}");
        assert!(s.contains("19.5 dB"), "summary: {s}");
        assert!(s.contains("Comp: Light"), "summary: {s}");
        assert!(s.contains("EQ: High Pass + Presence Boost"), "summary: {s}");
        assert!(!s.contains("HPF"), "MV7 has no HPF: {s}");
        assert!(!s.contains("Denoiser"), "MV7 has no denoiser: {s}");
    }

    #[test]
    fn summary_mv7_auto_shows_position_and_tone() {
        let mut state = mv7_example_state();
        state.mode = InputMode::Auto;
        state.auto_position = MicPosition::Far;
        state.auto_tone = AutoTone::Dark;
        let s =
            PresetSlot::from_device_state("S", &state, DeviceModel::Mv7).summary(DeviceModel::Mv7);
        assert_eq!(s, "Auto · Far · Dark");
    }

    #[test]
    fn apply_to_device_state_keeps_playback_mix_for_zero() {
        let mut slot =
            PresetSlot::from_device_state("old", &DeviceState::default(), DeviceModel::Mvx2u);
        let mut state = DeviceState {
            playback_mix: 60,
            ..DeviceState::default()
        };
        slot.apply_to_device_state(&mut state);
        assert_eq!(
            state.playback_mix, 60,
            "0 must not overwrite the current level"
        );

        slot.playback_mix = 25;
        slot.apply_to_device_state(&mut state);
        assert_eq!(state.playback_mix, 25);
    }

    /// Presets are hand-editable; out-of-range values must not reach the UI,
    /// where `monitor_mix = 255` overflowed on the next ← / →.
    #[test]
    fn apply_to_device_state_clamps_hand_edited_values() {
        let mut slot =
            PresetSlot::from_device_state("edited", &DeviceState::default(), DeviceModel::Mvx2u);
        slot.monitor_mix = 255;
        slot.playback_mix = 150;
        slot.reverb_intensity = 101;
        slot.tone = 100;
        slot.eq_bands[0].gain_db = 1000;
        slot.eq_bands[1].gain_db = -1000;

        let mut state = DeviceState::default();
        slot.apply_to_device_state(&mut state);

        assert_eq!(state.monitor_mix, 100);
        assert_eq!(state.playback_mix, 100);
        assert_eq!(state.reverb_intensity, 100);
        assert_eq!(state.tone, 10);
        assert_eq!(state.eq_bands[0].gain_db, 60);
        assert_eq!(state.eq_bands[1].gain_db, -80);
    }
}
