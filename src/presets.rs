//! Host-side preset storage for shurectl.
//!
//! Presets are TOML files in `shurectl/presets/` under the platform config
//! directory (`~/.config` on Linux; the README lists the others).
//! There are 4 fixed slots, numbered 0–3, stored as `preset_1.toml`–`preset_4.toml`.
//!
//! Each file is human-readable and hand-editable. The preset captures the
//! configurable DSP settings from `DeviceState` that the saving model has.
//! Identity fields (serial numbers, firmware version, device name) are left
//! out, so a load keeps the device's own.
//!
//! Presets live on the host only: loading one sends a SET for each saved
//! setting, with no device-side preset bank involved.

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
/// Fields from every model are included. A setting only some models have is an
/// `Option`, saved only by a model that has it (see `from_device_state()`).
/// `None` keeps the device's current value on load, so a preset saved on
/// another model can't reset a setting that model doesn't have. Presets saved
/// before 2.13.1 (2.12.0 for phantom power) held every field, and still apply
/// them all.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PresetSlot {
    /// Human-readable name shown in the TUI (editable in-app or by hand in TOML).
    pub name: String,

    // ── Every model ──────────────────────────────────────────────────────────
    /// Manual gain in dB, e.g. `19.5` (the MV7 steps 1.5 dB). Presets saved
    /// before fractional gain stored a whole number here, which still loads.
    pub gain_db: f32,
    pub mode: SerInputMode,
    pub muted: bool,
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

    // ── Only some models (`None` keeps the device's value) ───────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hpf: Option<SerHpfFrequency>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_position: Option<SerMicPosition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_tone: Option<SerAutoTone>,
    /// Auto Gain on the MVX2U Gen 1, Target Level on the MV6 Gen 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_gain: Option<SerAutoGain>,
    /// 48V phantom power, saved only on models with `has_phantom_power()`, so a
    /// preset from a mic without it can't switch it off under a condenser mic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phantom_power: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limiter_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compressor: Option<SerCompressorPreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq_bands: Option<[SerEqBand; 5]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoiser_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popper_stopper_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute_btn_disabled: Option<bool>,
    /// Tone: -10 (100% Dark) to +10 (100% Bright), 0 = Natural.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<i8>,
    /// Gain lock (Manual mode only), on models with `has_gain_lock()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mv6_gain_locked: Option<bool>,

    // ── MV6 Gen 2 ────────────────────────────────────────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptation_rate_fast: Option<bool>,
    /// Denoiser level, 1–100%.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoiser_level: Option<u8>,
    /// Advanced tone (the 5-band `eq_bands`) instead of the Tone slider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone_advanced: Option<bool>,

    // ── MV7+ ─────────────────────────────────────────────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverb_on_output: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverb_monitoring: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverb_type: Option<SerReverbType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverb_intensity: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_behavior: Option<SerLedBehavior>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_brightness: Option<SerLedBrightness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_live_theme: Option<SerLedLiveTheme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_solid_theme: Option<SerLedSolidTheme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_pulsing_theme: Option<SerLedPulsingTheme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_solid_rgb: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_pulsing_rgb: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_live_edge_rgb: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_live_middle_rgb: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_live_interior_rgb: Option<[u8; 3]>,

    // ── MV7 ──────────────────────────────────────────────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq_preset: Option<SerEqPreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_live_meter: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_night_mode: Option<bool>,
}

/// Preset gain (dB) → tenths of a dB, rounded and clamped to the `u16` range.
/// Model-specific ceilings are applied when the gain is sent to the device.
fn gain_db_to_tenths(gain_db: f32) -> u16 {
    (gain_db * 10.0).round().clamp(0.0, f32::from(u16::MAX)) as u16
}

/// Load a saved setting; `None` (not saved) keeps the current value.
fn load_saved<T>(field: &mut T, saved: Option<T>) {
    if let Some(value) = saved {
        *field = value;
    }
}

impl PresetSlot {
    /// Build a preset snapshot from a live `DeviceState` on `model`. Settings
    /// only some models have are saved only where `model` has them (the same
    /// lists `main.rs` writes on load), so the preset can't reset them on
    /// another model.
    pub fn from_device_state(
        name: impl Into<String>,
        state: &DeviceState,
        model: DeviceModel,
    ) -> Self {
        let mut slot = Self {
            name: name.into(),
            gain_db: f32::from(state.gain_tenths) / 10.0,
            mode: SerInputMode::from(state.mode),
            muted: state.muted,
            monitor_mix: state.monitor_mix,
            monitor_mix_is_mic_level: Some(model.has_playback_mix()),
            playback_mix: state.playback_mix,
            hpf: model.has_hpf().then_some(SerHpfFrequency::from(state.hpf)),
            phantom_power: model.has_phantom_power().then_some(state.phantom_power),
            mv6_gain_locked: model.has_gain_lock().then_some(state.mv6_gain_locked),
            denoiser_level: model.has_denoiser_level().then_some(state.denoiser_level),
            ..Self::default()
        };
        match model {
            DeviceModel::Mvx2u => {
                slot.save_auto_position_and_tone(state);
                slot.auto_gain = Some(SerAutoGain::from(state.auto_gain));
                slot.save_limiter_and_compressor(state);
                slot.eq_enabled = Some(state.eq_enabled);
                slot.eq_bands = Some(state.eq_bands.map(SerEqBand::from));
            }
            DeviceModel::Mvx2uGen2 => {
                slot.save_limiter_and_compressor(state);
                slot.save_voice_settings(state);
                slot.eq_bands = Some(state.eq_bands.map(SerEqBand::gain_only));
            }
            DeviceModel::Mv6 => {
                slot.save_voice_settings(state);
                slot.mute_btn_disabled = state.mute_btn_disabled;
            }
            DeviceModel::Mv6Gen2 => {
                slot.save_voice_settings(state);
                slot.mute_btn_disabled = state.mute_btn_disabled;
                slot.auto_gain = Some(SerAutoGain::from(state.auto_gain));
                slot.adaptation_rate_fast = Some(state.adaptation_rate_fast);
                slot.eq_bands = Some(state.eq_bands.map(SerEqBand::gain_only));
                slot.tone_advanced = Some(state.tone_advanced);
            }
            DeviceModel::Mv7 => {
                slot.save_auto_position_and_tone(state);
                slot.compressor = Some(SerCompressorPreset::from(state.compressor));
                slot.eq_preset = Some(SerEqPreset::from(state.eq_preset));
                slot.led_live_meter = Some(state.led_live_meter);
                slot.led_night_mode = Some(state.led_night_mode);
            }
            DeviceModel::Mv7Plus => {
                slot.save_limiter_and_compressor(state);
                slot.save_voice_settings(state);
                slot.mute_btn_disabled = state.mute_btn_disabled;
                slot.reverb_on_output = Some(state.reverb_on_output);
                slot.reverb_monitoring = Some(state.reverb_monitoring);
                slot.reverb_type = Some(SerReverbType::from(state.reverb_type));
                slot.reverb_intensity = Some(state.reverb_intensity);
                slot.led_behavior = Some(SerLedBehavior::from(state.led_behavior));
                slot.led_brightness = Some(SerLedBrightness::from(state.led_brightness));
                slot.led_live_theme = Some(SerLedLiveTheme::from(state.led_live_theme));
                slot.led_solid_theme = Some(SerLedSolidTheme::from(state.led_solid_theme));
                slot.led_pulsing_theme = Some(SerLedPulsingTheme::from(state.led_pulsing_theme));
                slot.led_solid_rgb = Some(state.led_solid_rgb);
                slot.led_pulsing_rgb = Some(state.led_pulsing_rgb);
                slot.led_live_edge_rgb = Some(state.led_live_edge_rgb);
                slot.led_live_middle_rgb = Some(state.led_live_middle_rgb);
                slot.led_live_interior_rgb = Some(state.led_live_interior_rgb);
            }
        }
        slot
    }

    /// Auto Level's Mic Position and Tone (MVX2U Gen 1, MV7).
    fn save_auto_position_and_tone(&mut self, state: &DeviceState) {
        self.auto_position = Some(SerMicPosition::from(state.auto_position));
        self.auto_tone = Some(SerAutoTone::from(state.auto_tone));
    }

    /// Limiter and Compressor (MVX2U Gen 1 and Gen 2, MV7+).
    fn save_limiter_and_compressor(&mut self, state: &DeviceState) {
        self.limiter_enabled = Some(state.limiter_enabled);
        self.compressor = Some(SerCompressorPreset::from(state.compressor));
    }

    /// Denoiser, Popper Stopper and Tone (MVX2U Gen 2, MV6, MV6 Gen 2, MV7+).
    fn save_voice_settings(&mut self, state: &DeviceState) {
        self.denoiser_enabled = Some(state.denoiser_enabled);
        self.popper_stopper_enabled = Some(state.popper_stopper_enabled);
        self.tone = Some(state.tone);
    }

    /// Apply this preset's settings onto a `DeviceState`, preserving
    /// hardware-identity fields (`serial_number`) and every setting the preset
    /// didn't save. Presets are hand-editable, so bounded values are clamped to
    /// the range the controls work in.
    pub fn apply_to_device_state(&self, state: &mut DeviceState) {
        state.gain_tenths = gain_db_to_tenths(self.gain_db);
        state.mode = InputMode::from(self.mode);
        state.muted = self.muted;
        state.monitor_mix = self.monitor_mix.min(100);
        // 0 is what every preset holds that wasn't saved on a model reading
        // Playback Mix (and MV6 presets from before it did), so it means "not
        // saved": keep the current level rather than silence headphone playback.
        if self.playback_mix > 0 {
            state.playback_mix = self.playback_mix.min(100);
        }
        load_saved(&mut state.hpf, self.hpf.map(HpfFrequency::from));
        load_saved(
            &mut state.auto_position,
            self.auto_position.map(MicPosition::from),
        );
        load_saved(&mut state.auto_tone, self.auto_tone.map(AutoTone::from));
        load_saved(&mut state.auto_gain, self.auto_gain.map(AutoGain::from));
        load_saved(&mut state.phantom_power, self.phantom_power);
        load_saved(&mut state.limiter_enabled, self.limiter_enabled);
        load_saved(
            &mut state.compressor,
            self.compressor.map(CompressorPreset::from),
        );
        load_saved(&mut state.eq_enabled, self.eq_enabled);
        if let Some(bands) = self.eq_bands {
            for (band, saved) in state.eq_bands.iter_mut().zip(bands) {
                band.gain_db = saved.gain_db.clamp(-80, 60);
                load_saved(&mut band.enabled, saved.enabled);
            }
        }
        load_saved(&mut state.denoiser_enabled, self.denoiser_enabled);
        load_saved(
            &mut state.popper_stopper_enabled,
            self.popper_stopper_enabled,
        );
        // An unread Mute Button stays unread: the device's own value is unknown.
        if let Some(disabled) = state.mute_btn_disabled.as_mut() {
            load_saved(disabled, self.mute_btn_disabled);
        }
        load_saved(&mut state.tone, self.tone.map(|tone| tone.clamp(-10, 10)));
        load_saved(&mut state.mv6_gain_locked, self.mv6_gain_locked);
        load_saved(&mut state.adaptation_rate_fast, self.adaptation_rate_fast);
        load_saved(
            &mut state.denoiser_level,
            self.denoiser_level.map(|level| level.clamp(1, 100)),
        );
        load_saved(&mut state.tone_advanced, self.tone_advanced);
        load_saved(&mut state.reverb_on_output, self.reverb_on_output);
        load_saved(&mut state.reverb_monitoring, self.reverb_monitoring);
        load_saved(
            &mut state.reverb_type,
            self.reverb_type.map(ReverbType::from),
        );
        load_saved(
            &mut state.reverb_intensity,
            self.reverb_intensity.map(|intensity| intensity.min(100)),
        );
        load_saved(
            &mut state.led_behavior,
            self.led_behavior.map(LedBehavior::from),
        );
        load_saved(
            &mut state.led_brightness,
            self.led_brightness.map(LedBrightness::from),
        );
        load_saved(
            &mut state.led_live_theme,
            self.led_live_theme.map(LedLiveTheme::from),
        );
        load_saved(
            &mut state.led_solid_theme,
            self.led_solid_theme.map(LedSolidTheme::from),
        );
        load_saved(
            &mut state.led_pulsing_theme,
            self.led_pulsing_theme.map(LedPulsingTheme::from),
        );
        load_saved(&mut state.led_solid_rgb, self.led_solid_rgb);
        load_saved(&mut state.led_pulsing_rgb, self.led_pulsing_rgb);
        load_saved(&mut state.led_live_edge_rgb, self.led_live_edge_rgb);
        load_saved(&mut state.led_live_middle_rgb, self.led_live_middle_rgb);
        load_saved(&mut state.led_live_interior_rgb, self.led_live_interior_rgb);
        load_saved(&mut state.eq_preset, self.eq_preset.map(EqPreset::from));
        load_saved(&mut state.led_live_meter, self.led_live_meter);
        load_saved(&mut state.led_night_mode, self.led_night_mode);
    }

    /// The preset's gain formatted the same way as everywhere else in the UI.
    fn gain_str(&self) -> String {
        format_gain(gain_db_to_tenths(self.gain_db))
    }

    /// `"<label> on"` / `"<label> off"`, or nothing when the preset didn't save it.
    fn on_off(label: &str, saved: Option<bool>) -> Option<String> {
        saved.map(|on| format!("{label} {}", if on { "on" } else { "off" }))
    }

    fn hpf_part(&self) -> Option<String> {
        self.hpf.map(|hpf| match HpfFrequency::from(hpf) {
            HpfFrequency::Off => "HPF off".to_string(),
            freq => format!("HPF {freq}"),
        })
    }

    /// `"Tone: 20% Bright"`, or nothing when the preset didn't save it.
    fn tone_part(&self) -> Option<String> {
        self.tone.map(|tone| match tone {
            0 => "Tone: Natural".to_string(),
            t if t > 0 => format!("Tone: {}% Bright", i32::from(t) * 10),
            t => format!("Tone: {}% Dark", (i32::from(t) * 10).abs()),
        })
    }

    fn compressor_part(&self) -> Option<String> {
        self.compressor
            .map(|comp| format!("Comp: {}", CompressorPreset::from(comp)))
    }

    /// One-line summary of the key settings for display in the TUI. A setting
    /// the preset didn't save (saved on another model) is left out.
    ///
    /// The model is needed because models have different configurable fields —
    /// showing phantom power on an MV6 (or reverb on an MVX2U) would be
    /// misleading.
    pub fn summary(&self, model: DeviceModel) -> String {
        let auto = InputMode::from(self.mode) == InputMode::Auto;
        let mode = Some(if auto { "Auto" } else { "Manual" }.to_string());
        let gain = Some(self.gain_str());
        let phantom = Self::on_off("48V", self.phantom_power);
        let denoiser = Self::on_off("Denoiser", self.denoiser_enabled);
        let popper = Self::on_off("Popper", self.popper_stopper_enabled);
        let parts = match model {
            DeviceModel::Mv6Gen2 => {
                // Auto shows the Auto Level settings in place of the unused gain.
                let level = if auto {
                    vec![
                        mode,
                        self.auto_gain.map(|target| {
                            format!("Target {}", AutoGain::from(target).target_level_label())
                        }),
                        self.adaptation_rate_fast
                            .map(|fast| if fast { "Fast" } else { "Slow" }.to_string()),
                    ]
                } else {
                    vec![gain]
                };
                let denoiser = match (self.denoiser_enabled, self.denoiser_level) {
                    (Some(true), Some(level)) => Some(format!("Denoiser {level}%")),
                    _ => denoiser,
                };
                let tone = match self.tone_advanced {
                    Some(true) => Some("Tone: 5-band EQ".to_string()),
                    Some(false) | None => self.tone_part(),
                };
                [level, vec![denoiser, popper, self.hpf_part(), tone]].concat()
            }
            DeviceModel::Mv6 => vec![gain, denoiser, popper, self.hpf_part(), self.tone_part()],
            DeviceModel::Mv7 if auto => vec![
                mode,
                self.auto_position
                    .map(|pos| MicPosition::from(pos).to_string()),
                self.auto_tone.map(|tone| AutoTone::from(tone).to_string()),
            ],
            DeviceModel::Mv7 => vec![
                mode,
                gain,
                self.compressor_part(),
                self.eq_preset
                    .map(|eq| format!("EQ: {}", EqPreset::from(eq))),
            ],
            DeviceModel::Mv7Plus => {
                let reverb = self.reverb_on_output.map(|on| {
                    match (on, self.reverb_type, self.reverb_intensity) {
                        (true, Some(kind), Some(intensity)) => {
                            format!("Reverb: {} {intensity}%", ReverbType::from(kind))
                        }
                        (true, _, _) => "Reverb: on".to_string(),
                        (false, _, _) => "Reverb: off".to_string(),
                    }
                });
                vec![
                    gain,
                    denoiser,
                    popper,
                    self.hpf_part(),
                    self.tone_part(),
                    reverb,
                ]
            }
            DeviceModel::Mvx2uGen2 if auto => {
                vec![mode, phantom, denoiser, popper, self.tone_part()]
            }
            DeviceModel::Mvx2uGen2 => vec![
                mode,
                gain,
                Self::on_off("Limiter", self.limiter_enabled),
                self.compressor_part(),
                phantom,
                denoiser,
                popper,
                self.hpf_part(),
            ],
            DeviceModel::Mvx2u if auto => vec![
                mode,
                self.auto_tone.map(|tone| AutoTone::from(tone).to_string()),
                self.auto_gain.map(|gain| AutoGain::from(gain).to_string()),
                self.auto_position
                    .map(|pos| MicPosition::from(pos).to_string()),
                phantom,
            ],
            DeviceModel::Mvx2u => vec![
                mode,
                gain,
                Self::on_off("EQ", self.eq_enabled),
                self.compressor_part(),
                phantom,
                self.hpf_part(),
            ],
        };
        parts.into_iter().flatten().collect::<Vec<_>>().join(" · ")
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

#[cfg(any(test, feature = "qa"))]
thread_local! {
    /// Where the `--qa` harness keeps presets instead of the user's config dir.
    static DIR_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Keep this thread's presets in `dir` (a temp dir) instead of the user's
/// config directory, or go back to it with `None`. For the `--qa` harness.
#[cfg(any(test, feature = "qa"))]
pub fn set_dir_override(dir: Option<PathBuf>) {
    DIR_OVERRIDE.with(|cell| *cell.borrow_mut() = dir);
}

/// Returns the `presets/` directory inside `config_dir()`, creating it if absent.
fn presets_dir() -> Result<PathBuf> {
    #[cfg(any(test, feature = "qa"))]
    if let Some(dir) = DIR_OVERRIDE.with(|cell| cell.borrow().clone()) {
        return Ok(dir);
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerInputMode {
    /// `DeviceState::default()`'s mode. A preset always records its own.
    #[default]
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
    /// Each band's own on/off switch, which only the MVX2U Gen 1 has. `None`
    /// keeps the device's switch, so a Gen 2 preset can't switch bands off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Gain in tenths of dB, range −80..+60.
    /// Stored as i16 so presets round-trip at 0.5 dB resolution for Gen 2.
    pub gain_db: i16,
}

impl From<EqBand> for SerEqBand {
    fn from(v: EqBand) -> Self {
        Self {
            enabled: Some(v.enabled),
            gain_db: v.gain_db,
        }
    }
}

impl SerEqBand {
    /// A band's gain alone, for models whose bands have no on/off switch.
    fn gain_only(v: EqBand) -> Self {
        Self {
            enabled: None,
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
            mute_btn_disabled: Some(true),
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

    /// Checked on hardware both ways between an MVX2U Gen 2 and an MV6 Gen 2.
    #[test]
    fn preset_from_another_model_keeps_settings_that_model_lacks() {
        // The MVX2U Gen 2 has no Mute Button setting or Denoiser level.
        let gen2 =
            PresetSlot::from_device_state("Gen 2", &DeviceState::default(), DeviceModel::Mvx2uGen2);
        let text = toml::to_string_pretty(&gen2).expect("serialise");
        assert!(!text.contains("mute_btn_disabled"), "{text}");
        assert!(!text.contains("denoiser_level"), "{text}");
        let mut mv6_gen2 = DeviceState {
            mute_btn_disabled: Some(true),
            denoiser_level: 80,
            ..DeviceState::default()
        };
        toml_roundtrip(&gen2).apply_to_device_state(&mut mv6_gen2);
        assert_eq!(mv6_gen2.mute_btn_disabled, Some(true));
        assert_eq!(mv6_gen2.denoiser_level, 80);

        // The MV6 Gen 2 has no Limiter or Compressor.
        let mv6 = PresetSlot::from_device_state(
            "MV6 Gen 2",
            &DeviceState::default(),
            DeviceModel::Mv6Gen2,
        );
        let mut gen2_state = DeviceState {
            limiter_enabled: true,
            compressor: CompressorPreset::Medium,
            ..DeviceState::default()
        };
        toml_roundtrip(&mv6).apply_to_device_state(&mut gen2_state);
        assert!(gen2_state.limiter_enabled);
        assert_eq!(gen2_state.compressor, CompressorPreset::Medium);

        // A summary leaves out what the preset didn't save.
        let gen1 =
            PresetSlot::from_device_state("Gen 1", &DeviceState::default(), DeviceModel::Mvx2u);
        let summary = gen1.summary(DeviceModel::Mv6);
        for absent in ["Denoiser", "Popper", "Tone"] {
            assert!(!summary.contains(absent), "{summary}");
        }
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
        let bands = decoded.eq_bands.expect("the Gen 1 saves its EQ bands");
        assert_eq!(bands[0].gain_db, 40); // +4.0 dB in tenths
        assert_eq!(bands[4].gain_db, -80); // -8.0 dB in tenths
        // The Gen 1 has no Denoiser, Popper Stopper or Tone, so it doesn't save them.
        assert_eq!(decoded.denoiser_enabled, None);
        assert_eq!(decoded.popper_stopper_enabled, None);
        assert_eq!(decoded.tone, None);
    }

    #[test]
    fn apply_to_device_state_restores_all_fields() {
        let original = example_state();
        let slot = PresetSlot::from_device_state("Test", &original, DeviceModel::Mvx2u);

        let mut target = DeviceState {
            serial_number: String::from("OTHER"),
            ..DeviceState::default()
        };

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
        // The Gen 1 has none of these, so its preset leaves them as they were.
        let untouched = DeviceState::default();
        assert_eq!(target.denoiser_enabled, untouched.denoiser_enabled);
        assert_eq!(
            target.popper_stopper_enabled,
            untouched.popper_stopper_enabled
        );
        assert_eq!(target.mute_btn_disabled, untouched.mute_btn_disabled);
        assert_eq!(target.tone, untouched.tone);
        // Identity fields must be untouched.
        assert_eq!(target.serial_number, "OTHER");
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
            mute_btn_disabled: Some(true),
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
        assert_eq!(decoded.denoiser_enabled, Some(true));
        assert_eq!(decoded.popper_stopper_enabled, Some(true));
        assert_eq!(decoded.mute_btn_disabled, Some(true));
        assert_eq!(decoded.tone, Some(5));
        assert_eq!(decoded.hpf, Some(SerHpfFrequency::Hz75));
        assert_eq!(
            decoded.monitor_mix, 62,
            "monitor_mix must survive TOML roundtrip"
        );
    }

    #[test]
    fn mv6_apply_to_device_state_restores_mv6_fields() {
        let original = mv6_example_state();
        let slot = PresetSlot::from_device_state("MV6 Test", &original, DeviceModel::Mv6);

        let mut target = DeviceState {
            serial_number: String::from("OTHER"),
            ..DeviceState::default()
        };

        slot.apply_to_device_state(&mut target);

        assert_eq!(target.gain_tenths, 240);
        assert!(target.denoiser_enabled);
        assert!(target.popper_stopper_enabled);
        assert_eq!(target.mute_btn_disabled, Some(true));
        assert_eq!(target.tone, 5);
        assert_eq!(target.hpf, HpfFrequency::Hz75);
        // Identity fields must be untouched.
        assert_eq!(target.serial_number, "OTHER");
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
    fn mv6_gen2_settings_roundtrip_and_old_presets_keep_them() {
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

        // A preset saved before the MV6 Gen 2 fields existed keeps the device's.
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
        assert_eq!(parsed.adaptation_rate_fast, None);
        assert_eq!(parsed.denoiser_level, None);
        assert_eq!(parsed.tone_advanced, None);
        parsed.apply_to_device_state(&mut restored);
        assert!(restored.adaptation_rate_fast);
        assert_eq!(restored.denoiser_level, 37);
        assert!(restored.tone_advanced);

        // A hand-edited level of 0 is clamped onto the 1–100% slider.
        let mut zero = parsed;
        zero.denoiser_level = Some(0);
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
    fn preset_without_mv7_fields_keeps_the_mics_settings() {
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
        assert_eq!(loaded.eq_preset, None);
        assert_eq!(loaded.led_live_meter, None);
        assert_eq!(loaded.led_night_mode, None);
        let mut mic = DeviceState {
            eq_preset: EqPreset::PresenceBoost,
            led_live_meter: false,
            led_night_mode: true,
            ..DeviceState::default()
        };
        loaded.apply_to_device_state(&mut mic);
        assert_eq!(mic.eq_preset, EqPreset::PresenceBoost);
        assert!(!mic.led_live_meter);
        assert!(mic.led_night_mode);
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
        let loaded = toml_roundtrip(&slot);
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
        slot.reverb_intensity = Some(101);
        slot.tone = Some(100);
        let bands = slot
            .eq_bands
            .as_mut()
            .expect("the Gen 1 saves its EQ bands");
        bands[0].gain_db = 1000;
        bands[1].gain_db = -1000;

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
