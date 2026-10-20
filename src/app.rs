//! Application state and navigation logic.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossterm::event::KeyCode;

use crate::meter::PeakWindow;
use crate::presets::{PRESET_COUNT, PresetSlot};
use crate::protocol::{
    AutoGain, AutoTone, CompressorPreset, DeviceModel, DeviceState, EqPreset, HpfFrequency,
    InputMode, LedBehavior, LedBrightness, LedLiveTheme, LedPulsingTheme, LedSolidTheme,
    MicPosition, MicPreset, ReverbType,
};

/// Which top-level tab/panel is active.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tab {
    Main,
    Eq,
    Dynamics,
    Reverb,
    Led,
    Presets,
    Info,
}

impl Tab {
    pub const ALL: [Tab; 7] = [
        Tab::Main,
        Tab::Eq,
        Tab::Dynamics,
        Tab::Reverb,
        Tab::Led,
        Tab::Presets,
        Tab::Info,
    ];

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Main => " Main ",
            Tab::Eq => " EQ ",
            Tab::Dynamics => " Dynamics ",
            Tab::Reverb => " Reverb ",
            Tab::Led => " LED ",
            Tab::Presets => " Presets ",
            Tab::Info => " Info ",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            Tab::Main => 0,
            Tab::Eq => 1,
            Tab::Dynamics => 2,
            Tab::Reverb => 3,
            Tab::Led => 4,
            Tab::Presets => 5,
            Tab::Info => 6,
        }
    }

    pub fn next(&self) -> Tab {
        let i = (self.index() + 1) % Self::ALL.len();
        Self::ALL[i]
    }

    pub fn prev(&self) -> Tab {
        let i = (self.index() + Self::ALL.len() - 1) % Self::ALL.len();
        Self::ALL[i]
    }
}

/// Which control within the current tab has focus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Focus {
    // Main tab — Mode, Mute and Monitor Mix everywhere; Phantom (MVX2U Gen 1
    // and Gen 2) and Lock (MVX2U Gen 1, MV7) where the model has them
    Mode,
    Mute,
    Phantom,
    Lock,
    MonitorMix,
    // Main tab — Manual mode only
    Gain,
    // Main tab — Auto Level mode only
    AutoPosition,
    AutoTone,
    AutoGain,
    // Main tab — MV6 Gen 2 Auto Level (Target Level reuses AutoGain)
    AdaptationRate,
    // EQ tab — 5 bands (MVX2U Gen 1 and Gen 2 in Manual, MV6 Gen 2 in Advanced);
    // band enables are Gen 1 only
    EqEnable,
    EqBandSelect,
    EqBandEnable(usize),
    EqGain(usize),
    // EQ tab — Tone slider (MV6, MV6 Gen 2 in Simple, MVX2U Gen 2 in Auto, MV7+)
    Tone,
    // EQ tab — MV6 Gen 2 Simple (Tone slider) / Advanced (5 bands)
    ToneMode,
    // EQ tab — MV7 EQ preset
    EqPreset,
    // Dynamics tab — Limiter, Compressor, HPF
    Limiter,
    Compressor,
    Hpf,
    // Dynamics tab — MV6-family controls (also MVX2U Gen 2 and MV7+; the Mute
    // Button not on the MVX2U Gen 2)
    Denoiser,
    PopperStopper,
    MuteBtnDisable,
    // Main tab — Gain Lock (`has_gain_lock()`, Manual mode only)
    GainLock,
    // Main tab — Playback Level, the second row of Monitor Mix (`has_playback_mix()`)
    PlaybackMix,
    // Main tab — MV6 Gen 2 built-in preset (Speech / Singing / Instrument)
    MicPreset,
    // Reverb tab — MV7+
    ReverbOutput,
    ReverbMonitor,
    ReverbPreset,
    ReverbIntensity,
    // LED tab — MV7+ LED controls
    LedBehavior,
    LedBrightness,
    LedTheme,
    // Color channels — only navigable when Custom theme is active
    LedSolidR,
    LedSolidG,
    LedSolidB,
    LedPulsingR,
    LedPulsingG,
    LedPulsingB,
    LedLiveEdgeR,
    LedLiveEdgeG,
    LedLiveEdgeB,
    LedLiveMiddleR,
    LedLiveMiddleG,
    LedLiveMiddleB,
    LedLiveInteriorR,
    LedLiveInteriorG,
    LedLiveInteriorB,
    // LED tab — MV7 controls
    LedLiveMeter,
    LedNightMode,
    // Presets tab — usize is slot index 0–3
    PresetName(usize),
    PresetActions(usize),
    // Info tab
    DeviceName,
    FactoryReset,
}

/// The full TUI application state.
pub struct App {
    pub device_state: DeviceState,
    pub active_tab: Tab,
    pub focus: Focus,
    pub status_message: String,
    pub status_is_error: bool,
    pub eq_selected_band: usize,
    pub should_quit: bool,
    pub demo_mode: bool,
    /// False once the device is gone (a HID read or write failed, or the
    /// presence poll no longer sees it), until a refresh or reconnect reads it
    /// again. Meaningless in demo mode.
    pub device_connected: bool,
    /// True while `device_state` may not match the device: its settings were
    /// never read (defaults), or a failed write or reconnect left them unknown.
    /// Writes based on it are refused until a refresh reads the device.
    pub state_unread: bool,
    pub help_visible: bool,
    /// Which device model is connected. Drives which controls are shown.
    pub device_model: DeviceModel,
    /// Loaded preset slots. `None` means the slot file does not exist yet.
    pub presets: [Option<PresetSlot>; PRESET_COUNT],
    /// Set to `true` while the user is typing a new name for a preset slot.
    pub editing_preset_name: bool,
    /// Which slot index is being edited (valid when `editing_preset_name` is true).
    pub editing_preset_index: usize,
    /// Set to `true` while the user is typing a new device name on the Info tab.
    pub editing_device_name: bool,
    /// Uncommitted preset or device name; nothing changes until Enter.
    pub name_draft: String,
    /// Set to `true` after the user first presses Enter on the factory reset button.
    /// A second Enter fires the action; any other key cancels.
    pub confirming_factory_reset: bool,
    /// The preset slot and action a first click selected; only a second click on
    /// that same button fires it. Cleared by any other click or any key.
    pub armed_preset_action: Option<(usize, KeyCode)>,
    /// Rolling peak windows shared with the cpal capture thread.
    /// `short` (0.3 s) drives the bar; `long` (3.0 s) drives the peak-hold readout.
    pub peak_window: Arc<Mutex<PeakWindow>>,
    /// The time the meter is read at; `None` reads the system clock. The `--qa`
    /// harness sets its fake clock here, so peak-hold expiry follows `tick`.
    pub meter_clock: Option<Instant>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            device_state: DeviceState::default(),
            active_tab: Tab::Main,
            focus: Focus::Mode,
            status_message: String::from("Press ? for help"),
            status_is_error: false,
            eq_selected_band: 0,
            should_quit: false,
            demo_mode: false,
            device_connected: true,
            state_unread: false,
            help_visible: false,
            device_model: DeviceModel::Mvx2u,
            presets: [None, None, None, None],
            editing_preset_name: false,
            editing_preset_index: 0,
            editing_device_name: false,
            name_draft: String::new(),
            confirming_factory_reset: false,
            armed_preset_action: None,
            peak_window: Arc::new(Mutex::new(PeakWindow::new())),
            meter_clock: None,
        }
    }
}

impl App {
    fn set_status(&mut self, msg: impl Into<String>, is_error: bool) {
        self.status_message = msg.into();
        self.status_is_error = is_error;
    }

    pub fn set_ok(&mut self, msg: impl Into<String>) {
        self.set_status(msg, false);
    }

    pub fn set_err(&mut self, msg: impl Into<String>) {
        self.set_status(msg, true);
    }

    /// Refuse a Mute Button change while its state is unread, and say why.
    pub fn report_unavailable_mute_button(&mut self) {
        match self
            .device_state
            .mute_button_needs_firmware(self.device_model)
        {
            Some(firmware) => self.set_err(format!("Mute Button needs firmware {firmware}+")),
            None => self.set_err(MUTE_BTN_UNREAD),
        }
    }

    // ── Tab navigation ────────────────────────────────────────────────────────

    /// Returns true when a tab does not exist on this model at all (Reverb
    /// everywhere but the MV7+, LED on models without LED settings). Hidden tabs
    /// are left out of the tab bar entirely.
    pub fn is_tab_hidden(&self, tab: Tab) -> bool {
        matches!(
            (tab, self.device_model),
            (
                Tab::Reverb,
                DeviceModel::Mv6
                    | DeviceModel::Mv6Gen2
                    | DeviceModel::Mvx2u
                    | DeviceModel::Mvx2uGen2
                    | DeviceModel::Mv7
            ) | (
                Tab::Led,
                DeviceModel::Mv6
                    | DeviceModel::Mv6Gen2
                    | DeviceModel::Mvx2u
                    | DeviceModel::Mvx2uGen2
            )
        )
    }

    /// Whether a preset or device name is being typed.
    pub fn editing_name(&self) -> bool {
        self.editing_preset_name || self.editing_device_name
    }

    /// Returns true when the Info tab offers Factory Reset: the models whose
    /// reset packet was captured from MOTIV (`cmd_factory_reset`). The original
    /// MV7 has no factory reset (MOTIV doesn't offer one); the MVX2U Gen 1 and
    /// MV6 are uncaptured. The UI hides the button and the Enter handler ignores
    /// it on every other model.
    pub fn supports_factory_reset(&self) -> bool {
        match self.device_model {
            DeviceModel::Mv7Plus | DeviceModel::Mv6Gen2 | DeviceModel::Mvx2uGen2 => true,
            DeviceModel::Mvx2u | DeviceModel::Mv6 | DeviceModel::Mv7 => false,
        }
    }

    /// Returns true when the EQ tab shows the 5 parametric bands. MVX2U Gen 1
    /// and Gen 2 show them in Manual mode (Gen 2 Auto shows the tone slider, Gen 1
    /// Auto locks the tab); the MV6 Gen 2 in its Advanced tone mode. The MV6 and
    /// MV7+ have only a tone slider and the MV7 a four-way preset.
    pub fn eq_bands_shown(&self) -> bool {
        match self.device_model {
            DeviceModel::Mvx2u | DeviceModel::Mvx2uGen2 => {
                self.device_state.mode == InputMode::Manual
            }
            DeviceModel::Mv6Gen2 => self.device_state.tone_advanced,
            DeviceModel::Mv6 | DeviceModel::Mv7 | DeviceModel::Mv7Plus => false,
        }
    }

    /// Returns true when `f` flattens the EQ: on the EQ tab, with the 5 bands on
    /// screen.
    pub fn can_flatten_eq(&self) -> bool {
        self.active_tab == Tab::Eq && self.eq_bands_shown()
    }

    /// Returns true when Gain Lock is on and the model has one. A lock flag from
    /// a preset saved on another model is ignored, since there is no control
    /// here to turn it off.
    pub fn gain_locked(&self) -> bool {
        self.device_model.has_gain_lock() && self.device_state.mv6_gain_locked
    }

    /// Keep the view valid after the device state changed underneath it, such as
    /// a refresh that finds the mode switched. Leave a tab that is now locked, and
    /// re-pick focus when the mode changed on a tab whose controls depend on it,
    /// or when the EQ tab swapped the Tone slider for the bands (MV6 Gen 2 tone
    /// mode). Without this, ←/→ would still adjust controls no longer on screen.
    /// The Presets tab keeps its focus, so loading a preset leaves you on its slot.
    pub fn settle_focus(&mut self, mode_before: InputMode) {
        let eq_focus_hidden = match self.focus {
            Focus::Tone => self.eq_bands_shown(),
            Focus::EqBandSelect | Focus::EqGain(_) => !self.eq_bands_shown(),
            _ => false,
        };
        if self.is_tab_locked(self.active_tab) {
            self.active_tab = Tab::Main;
            self.reset_focus_for_tab();
        } else if (self.device_state.mode != mode_before
            && matches!(self.active_tab, Tab::Main | Tab::Eq | Tab::Dynamics))
            || (self.active_tab == Tab::Eq && eq_focus_hidden)
        {
            self.reset_focus_for_tab();
        }
    }

    /// Returns true when a tab should be inaccessible given the current device state.
    ///
    /// MVX2U Gen 1 and MV7: EQ and Dynamics are locked in Auto Level mode — the
    /// device manages those parameters itself (the MV7 resets them on entering Auto).
    /// MVX2U Gen 2 and the MV6s: EQ and Dynamics are always accessible regardless of mode.
    /// Hidden tabs count as locked so tab cycling skips them.
    pub fn is_tab_locked(&self, tab: Tab) -> bool {
        self.is_tab_hidden(tab)
            || matches!(
                (tab, self.device_model, self.device_state.mode),
                (
                    Tab::Eq | Tab::Dynamics,
                    DeviceModel::Mvx2u | DeviceModel::Mv7,
                    InputMode::Auto
                )
            )
    }

    /// Select a visible, available tab (shared by keyboard and mouse navigation).
    pub fn select_tab(&mut self, tab: Tab) {
        // Re-selecting the active tab (a click on its label) keeps focus.
        if tab != self.active_tab && !self.is_tab_locked(tab) {
            self.active_tab = tab;
            self.reset_focus_for_tab();
        }
    }

    pub fn next_tab(&mut self) {
        // Skip past any locked tabs so we never land on one.
        let mut candidate = self.active_tab.next();
        for _ in 0..Tab::ALL.len() {
            if !self.is_tab_locked(candidate) {
                break;
            }
            candidate = candidate.next();
        }
        self.select_tab(candidate);
    }

    pub fn prev_tab(&mut self) {
        // Skip past any locked tabs so we never land on one.
        let mut candidate = self.active_tab.prev();
        for _ in 0..Tab::ALL.len() {
            if !self.is_tab_locked(candidate) {
                break;
            }
            candidate = candidate.prev();
        }
        self.select_tab(candidate);
    }

    fn reset_focus_for_tab(&mut self) {
        self.focus = match (self.active_tab, self.device_model) {
            (Tab::Main, DeviceModel::Mv6 | DeviceModel::Mv6Gen2) => match self.device_state.mode {
                InputMode::Manual => Focus::Gain,
                InputMode::Auto => Focus::Mode,
            },
            (Tab::Main, DeviceModel::Mv7Plus) => match self.device_state.mode {
                InputMode::Manual => Focus::Gain,
                InputMode::Auto => Focus::Mode,
            },
            (Tab::Main, DeviceModel::Mv7) => match self.device_state.mode {
                InputMode::Manual => Focus::Gain,
                InputMode::Auto => Focus::Mode,
            },
            (Tab::Main, DeviceModel::Mvx2u) => match self.device_state.mode {
                InputMode::Manual => Focus::Gain,
                InputMode::Auto => Focus::Mode,
            },
            (Tab::Main, DeviceModel::Mvx2uGen2) => match self.device_state.mode {
                InputMode::Manual => Focus::Gain,
                InputMode::Auto => Focus::Mode,
            },
            (Tab::Eq, DeviceModel::Mv6 | DeviceModel::Mv7Plus) => Focus::Tone,
            (Tab::Eq, DeviceModel::Mv6Gen2) => Focus::ToneMode,
            (Tab::Eq, DeviceModel::Mvx2u) => Focus::EqEnable,
            (Tab::Eq, DeviceModel::Mv7) => Focus::EqPreset,
            (Tab::Eq, DeviceModel::Mvx2uGen2) => match self.device_state.mode {
                InputMode::Auto => Focus::Tone,
                InputMode::Manual => Focus::EqBandSelect,
            },
            (Tab::Dynamics, DeviceModel::Mv6 | DeviceModel::Mv6Gen2) => Focus::Denoiser,
            (Tab::Dynamics, DeviceModel::Mv7Plus) => Focus::Limiter,
            (Tab::Dynamics, DeviceModel::Mvx2u) => Focus::Limiter,
            (Tab::Dynamics, DeviceModel::Mv7) => Focus::Compressor,
            (Tab::Dynamics, DeviceModel::Mvx2uGen2) => match self.device_state.mode {
                InputMode::Auto => Focus::Denoiser,
                InputMode::Manual => Focus::Limiter,
            },
            (Tab::Reverb, _) => Focus::ReverbOutput,
            (Tab::Led, DeviceModel::Mv7) => Focus::LedLiveMeter,
            (Tab::Led, _) => Focus::LedBehavior,
            (Tab::Presets, _) => Focus::PresetName(0),
            (Tab::Info, model) if model.has_device_rename() => Focus::DeviceName,
            (Tab::Info, _) => Focus::FactoryReset,
        };
    }

    // ── Focus cycling within a tab ────────────────────────────────────────────
    //
    // MVX2U Gen 1 Main cycles:
    //   Manual: Mode → Mute → Gain → MonitorMix → Phantom → Lock → (wrap)
    //   Auto:   Mode → Mute → AutoPosition → AutoTone → AutoGain → MonitorMix → Phantom → Lock → (wrap)
    //
    // MVX2U Gen 2 Main cycles:
    //   Manual: Mode → Mute → Gain → GainLock → MonitorMix → PlaybackMix → Phantom → (wrap)
    //   Auto:   Mode → Mute → MonitorMix → PlaybackMix → Phantom → (wrap)
    //
    // MVX2U Gen 2 EQ cycles:
    //   Auto:   Tone (slider only)
    //   Manual: EqBandSelect → EqGain(band) → (wrap)
    //
    // MVX2U Gen 2 Dynamics cycles:
    //   Manual: Limiter → Compressor → Denoiser → PopperStopper → Hpf → (wrap)
    //   Auto:   Denoiser → PopperStopper → Hpf → (wrap)
    //   (A mode change while on Limiter/Compressor is fixed up by settle_focus().)
    //
    // MV6 Main cycles (Playback Level unconfirmed on hardware, see has_playback_mix):
    //   Manual: Mode → Mute → Gain → GainLock → MonitorMix → PlaybackMix → (wrap)
    //   Auto:   Mode → Mute → MonitorMix → PlaybackMix → (wrap)
    //
    // MV6 EQ:      Tone (slider only, no bands)
    // MV6 Dynamics: Denoiser → PopperStopper → MuteBtnDisable → Hpf → (wrap)
    //
    // MV6 Gen 2: as MV6, plus Mic Preset last (a device extra), and in Auto its
    // Auto Level controls (Gen 1 order):
    //   Manual: Mode → Mute → Gain → GainLock → MonitorMix → PlaybackMix → MicPreset → (wrap)
    //   Auto:   Mode → Mute → AutoGain (Target Level) → AdaptationRate → MonitorMix
    //           → PlaybackMix → MicPreset → (wrap)
    //   EQ:     ToneMode → Tone (Simple) or EqBandSelect → EqGain(band) (Advanced)
    //
    // MV7 Main cycles (Gen 1 order, minus Auto Gain and Phantom which it lacks):
    //   Manual: Mode → Mute → Gain → MonitorMix → Lock → (wrap)
    //   Auto:   Mode → Mute → AutoPosition → AutoTone → MonitorMix → Lock → (wrap)
    // MV7 EQ: EqPreset. MV7 Dynamics: Compressor. MV7 LED: LedLiveMeter → LedNightMode.
    //
    // MV7+ Main cycles:
    //   Manual: Mode → Mute → Gain → MonitorMix → PlaybackMix → (wrap)
    //   Auto:   Mode → Mute → MonitorMix → PlaybackMix → (wrap)
    // MV7+ EQ: Tone. MV7+ Dynamics: Limiter → Compressor → Denoiser → PopperStopper
    //   → MuteBtnDisable → Hpf → (wrap). Reverb and LED tabs: MV7+ only.
    pub fn focus_next(&mut self) {
        self.focus = match (
            &self.active_tab,
            &self.focus,
            &self.device_model,
            &self.device_state.mode,
        ) {
            // ── MV6 Gen 2 tail: Mic Level → Playback Level → Mic Preset ───────
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mv6Gen2, _) => Focus::PlaybackMix,
            (Tab::Main, Focus::PlaybackMix, DeviceModel::Mv6Gen2, _) => Focus::MicPreset,
            (Tab::Main, Focus::MicPreset, DeviceModel::Mv6Gen2, _) => Focus::Mode,
            // ── MV6 Gen 2 Auto Level controls ─────────────────────────────────
            (Tab::Main, Focus::Mute, DeviceModel::Mv6Gen2, InputMode::Auto) => Focus::AutoGain,
            (Tab::Main, Focus::AutoGain, DeviceModel::Mv6Gen2, InputMode::Auto) => {
                Focus::AdaptationRate
            }
            (Tab::Main, Focus::AdaptationRate, DeviceModel::Mv6Gen2, InputMode::Auto) => {
                Focus::MonitorMix
            }
            // ── MV6 Main cycle ────────────────────────────────────────────────
            (
                Tab::Main,
                Focus::Mode,
                DeviceModel::Mv6 | DeviceModel::Mv6Gen2,
                InputMode::Manual,
            ) => Focus::Mute,
            (
                Tab::Main,
                Focus::Mute,
                DeviceModel::Mv6 | DeviceModel::Mv6Gen2,
                InputMode::Manual,
            ) => Focus::Gain,
            (
                Tab::Main,
                Focus::Gain,
                DeviceModel::Mv6 | DeviceModel::Mv6Gen2,
                InputMode::Manual,
            ) => Focus::GainLock,
            (
                Tab::Main,
                Focus::GainLock,
                DeviceModel::Mv6 | DeviceModel::Mv6Gen2,
                InputMode::Manual,
            ) => Focus::MonitorMix,
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mv6, InputMode::Manual) => {
                Focus::PlaybackMix
            }
            (Tab::Main, Focus::Mode, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, InputMode::Auto) => {
                Focus::Mute
            }
            (Tab::Main, Focus::Mute, DeviceModel::Mv6, InputMode::Auto) => Focus::MonitorMix,
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mv6, InputMode::Auto) => Focus::PlaybackMix,
            (Tab::Main, Focus::PlaybackMix, DeviceModel::Mv6, _) => Focus::Mode,
            (Tab::Main, _, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, _) => Focus::Mode,

            // ── MVX2U Manual cycle ────────────────────────────────────────────
            (Tab::Main, Focus::Mode, DeviceModel::Mvx2u, InputMode::Manual) => Focus::Mute,
            (Tab::Main, Focus::Mute, DeviceModel::Mvx2u, InputMode::Manual) => Focus::Gain,
            (Tab::Main, Focus::Gain, DeviceModel::Mvx2u, InputMode::Manual) => Focus::MonitorMix,
            // ── MVX2U Auto cycle ──────────────────────────────────────────────
            (Tab::Main, Focus::Mode, DeviceModel::Mvx2u, InputMode::Auto) => Focus::Mute,
            (Tab::Main, Focus::Mute, DeviceModel::Mvx2u, InputMode::Auto) => Focus::AutoPosition,
            (Tab::Main, Focus::AutoPosition, DeviceModel::Mvx2u, InputMode::Auto) => {
                Focus::AutoTone
            }
            (Tab::Main, Focus::AutoTone, DeviceModel::Mvx2u, InputMode::Auto) => Focus::AutoGain,
            (Tab::Main, Focus::AutoGain, DeviceModel::Mvx2u, InputMode::Auto) => Focus::MonitorMix,
            // ── MVX2U shared tail ─────────────────────────────────────────────
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mvx2u, _) => Focus::Phantom,
            (Tab::Main, Focus::Phantom, DeviceModel::Mvx2u, _) => Focus::Lock,
            (Tab::Main, Focus::Lock, DeviceModel::Mvx2u, _) => Focus::Mode,
            (Tab::Main, _, DeviceModel::Mvx2u, _) => Focus::Mode,

            // ── MV6 / MV7+ EQ (Tone only) ────────────────────────────────────
            (Tab::Eq, _, DeviceModel::Mv6 | DeviceModel::Mv7Plus, _) => Focus::Tone, // Tone is the only EQ control

            // ── MV6 Gen 2 EQ: ToneMode → Tone (Simple) or bands (Advanced) ───
            (Tab::Eq, Focus::ToneMode, DeviceModel::Mv6Gen2, _)
                if self.device_state.tone_advanced =>
            {
                Focus::EqBandSelect
            }
            (Tab::Eq, Focus::ToneMode, DeviceModel::Mv6Gen2, _) => Focus::Tone,
            (Tab::Eq, Focus::EqBandSelect, DeviceModel::Mv6Gen2, _) => {
                Focus::EqGain(self.eq_selected_band)
            }
            (Tab::Eq, _, DeviceModel::Mv6Gen2, _) => Focus::ToneMode,

            // ── MVX2U Gen 2 Main cycle ────────────────────────────────────────
            (Tab::Main, Focus::Mode, DeviceModel::Mvx2uGen2, InputMode::Manual) => Focus::Mute,
            (Tab::Main, Focus::Mute, DeviceModel::Mvx2uGen2, InputMode::Manual) => Focus::Gain,
            (Tab::Main, Focus::Gain, DeviceModel::Mvx2uGen2, InputMode::Manual) => Focus::GainLock,
            (Tab::Main, Focus::GainLock, DeviceModel::Mvx2uGen2, InputMode::Manual) => {
                Focus::MonitorMix
            }
            (Tab::Main, Focus::Mode, DeviceModel::Mvx2uGen2, InputMode::Auto) => Focus::Mute,
            (Tab::Main, Focus::Mute, DeviceModel::Mvx2uGen2, InputMode::Auto) => Focus::MonitorMix,
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mvx2uGen2, _) => Focus::PlaybackMix,
            (Tab::Main, Focus::PlaybackMix, DeviceModel::Mvx2uGen2, _) => Focus::Phantom,
            (Tab::Main, Focus::Phantom, DeviceModel::Mvx2uGen2, _) => Focus::Mode,
            (Tab::Main, _, DeviceModel::Mvx2uGen2, _) => Focus::Mode,

            // ── MVX2U Gen 2 EQ (Tone in Auto, 5-band in Manual) ──────────────
            (Tab::Eq, _, DeviceModel::Mvx2uGen2, InputMode::Auto) => Focus::Tone,
            (Tab::Eq, Focus::EqBandSelect, DeviceModel::Mvx2uGen2, InputMode::Manual) => {
                Focus::EqGain(self.eq_selected_band)
            }
            (Tab::Eq, Focus::EqGain(_), DeviceModel::Mvx2uGen2, InputMode::Manual) => {
                Focus::EqBandSelect
            }
            (Tab::Eq, _, DeviceModel::Mvx2uGen2, _) => Focus::EqBandSelect,

            // ── MVX2U Gen 2 Dynamics ──────────────────────────────────────────
            (Tab::Dynamics, Focus::Limiter, DeviceModel::Mvx2uGen2, _) => Focus::Compressor,
            (Tab::Dynamics, Focus::Compressor, DeviceModel::Mvx2uGen2, _) => Focus::Denoiser,
            (Tab::Dynamics, Focus::Denoiser, DeviceModel::Mvx2uGen2, InputMode::Manual) => {
                Focus::PopperStopper
            }
            (Tab::Dynamics, Focus::PopperStopper, DeviceModel::Mvx2uGen2, InputMode::Manual) => {
                Focus::Hpf
            }
            (Tab::Dynamics, Focus::Hpf, DeviceModel::Mvx2uGen2, InputMode::Manual) => {
                Focus::Limiter
            }
            (Tab::Dynamics, Focus::Denoiser, DeviceModel::Mvx2uGen2, InputMode::Auto) => {
                Focus::PopperStopper
            }
            (Tab::Dynamics, Focus::PopperStopper, DeviceModel::Mvx2uGen2, InputMode::Auto) => {
                Focus::Hpf
            }
            (Tab::Dynamics, Focus::Hpf, DeviceModel::Mvx2uGen2, InputMode::Auto) => Focus::Denoiser,
            (Tab::Dynamics, _, DeviceModel::Mvx2uGen2, InputMode::Manual) => Focus::Limiter,
            (Tab::Dynamics, _, DeviceModel::Mvx2uGen2, _) => Focus::Denoiser,

            // ── MVX2U EQ ──────────────────────────────────────────────────────
            (Tab::Eq, Focus::EqEnable, DeviceModel::Mvx2u, _) => Focus::EqBandSelect,
            (Tab::Eq, Focus::EqBandSelect, DeviceModel::Mvx2u, _) => {
                Focus::EqBandEnable(self.eq_selected_band)
            }
            (Tab::Eq, Focus::EqBandEnable(b), DeviceModel::Mvx2u, _) => Focus::EqGain(*b),
            (Tab::Eq, Focus::EqGain(_), DeviceModel::Mvx2u, _) => Focus::EqEnable,
            (Tab::Eq, _, DeviceModel::Mvx2u, _) => Focus::EqEnable,

            // ── MV6 Dynamics ──────────────────────────────────────────────────
            (Tab::Dynamics, Focus::Denoiser, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, _) => {
                Focus::PopperStopper
            }
            (Tab::Dynamics, Focus::PopperStopper, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, _) => {
                Focus::MuteBtnDisable
            }
            (Tab::Dynamics, Focus::MuteBtnDisable, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, _) => {
                Focus::Hpf
            }
            (Tab::Dynamics, Focus::Hpf, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, _) => {
                Focus::Denoiser
            }
            (Tab::Dynamics, _, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, _) => Focus::Denoiser,

            // ── MV7+ Main cycle
            (Tab::Main, Focus::Mode, DeviceModel::Mv7Plus, _) => Focus::Mute,
            (Tab::Main, Focus::Mute, DeviceModel::Mv7Plus, InputMode::Manual) => Focus::Gain,
            (Tab::Main, Focus::Gain, DeviceModel::Mv7Plus, InputMode::Manual) => Focus::MonitorMix,
            (Tab::Main, Focus::Mute, DeviceModel::Mv7Plus, InputMode::Auto) => Focus::MonitorMix,
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mv7Plus, _) => Focus::PlaybackMix,
            (Tab::Main, Focus::PlaybackMix, DeviceModel::Mv7Plus, _) => Focus::Mode,
            (Tab::Main, _, DeviceModel::Mv7Plus, _) => Focus::Mode,

            // ── MV7+ Dynamics (Limiter+Comp+Denoiser+Popper+MuteBtn+Hpf)
            (Tab::Dynamics, Focus::Limiter, DeviceModel::Mv7Plus, _) => Focus::Compressor,
            (Tab::Dynamics, Focus::Compressor, DeviceModel::Mv7Plus, _) => Focus::Denoiser,
            (Tab::Dynamics, Focus::Denoiser, DeviceModel::Mv7Plus, _) => Focus::PopperStopper,
            (Tab::Dynamics, Focus::PopperStopper, DeviceModel::Mv7Plus, _) => Focus::MuteBtnDisable,
            (Tab::Dynamics, Focus::MuteBtnDisable, DeviceModel::Mv7Plus, _) => Focus::Hpf,
            (Tab::Dynamics, Focus::Hpf, DeviceModel::Mv7Plus, _) => Focus::Limiter,
            (Tab::Dynamics, _, DeviceModel::Mv7Plus, _) => Focus::Limiter,

            // ── MV7 Main cycle ────────────────────────────────────────────────
            (Tab::Main, Focus::Mode, DeviceModel::Mv7, _) => Focus::Mute,
            (Tab::Main, Focus::Mute, DeviceModel::Mv7, InputMode::Manual) => Focus::Gain,
            (Tab::Main, Focus::Gain, DeviceModel::Mv7, InputMode::Manual) => Focus::MonitorMix,
            (Tab::Main, Focus::Mute, DeviceModel::Mv7, InputMode::Auto) => Focus::AutoPosition,
            (Tab::Main, Focus::AutoPosition, DeviceModel::Mv7, InputMode::Auto) => Focus::AutoTone,
            (Tab::Main, Focus::AutoTone, DeviceModel::Mv7, InputMode::Auto) => Focus::MonitorMix,
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mv7, _) => Focus::Lock,
            (Tab::Main, Focus::Lock, DeviceModel::Mv7, _) => Focus::Mode,
            (Tab::Main, _, DeviceModel::Mv7, _) => Focus::Mode,

            // ── MV7 EQ, Dynamics, LED ─────────────────────────────────────────
            (Tab::Eq, _, DeviceModel::Mv7, _) => Focus::EqPreset,
            (Tab::Dynamics, _, DeviceModel::Mv7, _) => Focus::Compressor,
            (Tab::Led, Focus::LedLiveMeter, DeviceModel::Mv7, _) => Focus::LedNightMode,
            (Tab::Led, _, DeviceModel::Mv7, _) => Focus::LedLiveMeter,

            // ── Reverb tab (MV7+ only)
            (Tab::Reverb, Focus::ReverbOutput, _, _) => Focus::ReverbMonitor,
            (Tab::Reverb, Focus::ReverbMonitor, _, _) => Focus::ReverbPreset,
            (Tab::Reverb, Focus::ReverbPreset, _, _) => Focus::ReverbIntensity,
            (Tab::Reverb, Focus::ReverbIntensity, _, _) => Focus::ReverbOutput,
            (Tab::Reverb, _, _, _) => Focus::ReverbOutput,

            // ── LED tab (MV7+ only)
            (Tab::Led, Focus::LedBehavior, _, _) => Focus::LedBrightness,
            (Tab::Led, Focus::LedBrightness, _, _) => Focus::LedTheme,
            (Tab::Led, Focus::LedTheme, _, _) => self.led_first_color_focus(),
            (Tab::Led, Focus::LedSolidR, _, _) => Focus::LedSolidG,
            (Tab::Led, Focus::LedSolidG, _, _) => Focus::LedSolidB,
            (Tab::Led, Focus::LedSolidB, _, _) => Focus::LedBehavior,
            (Tab::Led, Focus::LedPulsingR, _, _) => Focus::LedPulsingG,
            (Tab::Led, Focus::LedPulsingG, _, _) => Focus::LedPulsingB,
            (Tab::Led, Focus::LedPulsingB, _, _) => Focus::LedBehavior,
            (Tab::Led, Focus::LedLiveEdgeR, _, _) => Focus::LedLiveEdgeG,
            (Tab::Led, Focus::LedLiveEdgeG, _, _) => Focus::LedLiveEdgeB,
            (Tab::Led, Focus::LedLiveEdgeB, _, _) => Focus::LedLiveMiddleR,
            (Tab::Led, Focus::LedLiveMiddleR, _, _) => Focus::LedLiveMiddleG,
            (Tab::Led, Focus::LedLiveMiddleG, _, _) => Focus::LedLiveMiddleB,
            (Tab::Led, Focus::LedLiveMiddleB, _, _) => Focus::LedLiveInteriorR,
            (Tab::Led, Focus::LedLiveInteriorR, _, _) => Focus::LedLiveInteriorG,
            (Tab::Led, Focus::LedLiveInteriorG, _, _) => Focus::LedLiveInteriorB,
            (Tab::Led, Focus::LedLiveInteriorB, _, _) => Focus::LedBehavior,
            (Tab::Led, _, _, _) => Focus::LedBehavior,

            // ── MVX2U Dynamics ────────────────────────────────────────────────
            (Tab::Dynamics, Focus::Limiter, DeviceModel::Mvx2u, _) => Focus::Compressor,
            (Tab::Dynamics, Focus::Compressor, DeviceModel::Mvx2u, _) => Focus::Hpf,
            (Tab::Dynamics, Focus::Hpf, DeviceModel::Mvx2u, _) => Focus::Limiter,
            (Tab::Dynamics, _, DeviceModel::Mvx2u, _) => Focus::Limiter,

            (Tab::Presets, Focus::PresetName(i), _, _) => Focus::PresetActions(*i),
            (Tab::Presets, Focus::PresetActions(i), _, _) => {
                Focus::PresetName((i + 1) % PRESET_COUNT)
            }
            (Tab::Presets, _, _, _) => Focus::PresetName(0),

            // Info: Device Name ↔ Factory Reset where both exist.
            (Tab::Info, Focus::DeviceName, _, _) if self.supports_factory_reset() => {
                Focus::FactoryReset
            }
            (Tab::Info, Focus::FactoryReset, model, _) if model.has_device_rename() => {
                Focus::DeviceName
            }

            _ => self.focus,
        };
    }

    pub fn focus_prev(&mut self) {
        self.focus = match (
            &self.active_tab,
            &self.focus,
            &self.device_model,
            &self.device_state.mode,
        ) {
            // ── MV6 Gen 2 tail reverse ────────────────────────────────────────
            (Tab::Main, Focus::Mode, DeviceModel::Mv6Gen2, _) => Focus::MicPreset,
            (Tab::Main, Focus::MicPreset, DeviceModel::Mv6Gen2, _) => Focus::PlaybackMix,
            (Tab::Main, Focus::PlaybackMix, DeviceModel::Mv6Gen2, _) => Focus::MonitorMix,
            // ── MV6 Gen 2 Auto Level controls reverse ─────────────────────────
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mv6Gen2, InputMode::Auto) => {
                Focus::AdaptationRate
            }
            (Tab::Main, Focus::AdaptationRate, DeviceModel::Mv6Gen2, InputMode::Auto) => {
                Focus::AutoGain
            }
            (Tab::Main, Focus::AutoGain, DeviceModel::Mv6Gen2, InputMode::Auto) => Focus::Mute,
            // ── MV6 Main reverse ──────────────────────────────────────────────
            (Tab::Main, Focus::Mode, DeviceModel::Mv6, InputMode::Manual) => Focus::PlaybackMix,
            (
                Tab::Main,
                Focus::Mute,
                DeviceModel::Mv6 | DeviceModel::Mv6Gen2,
                InputMode::Manual,
            ) => Focus::Mode,
            (
                Tab::Main,
                Focus::Gain,
                DeviceModel::Mv6 | DeviceModel::Mv6Gen2,
                InputMode::Manual,
            ) => Focus::Mute,
            (
                Tab::Main,
                Focus::GainLock,
                DeviceModel::Mv6 | DeviceModel::Mv6Gen2,
                InputMode::Manual,
            ) => Focus::Gain,
            (
                Tab::Main,
                Focus::MonitorMix,
                DeviceModel::Mv6 | DeviceModel::Mv6Gen2,
                InputMode::Manual,
            ) => Focus::GainLock,
            (Tab::Main, Focus::Mode, DeviceModel::Mv6, InputMode::Auto) => Focus::PlaybackMix,
            (Tab::Main, Focus::PlaybackMix, DeviceModel::Mv6, _) => Focus::MonitorMix,
            (Tab::Main, Focus::Mute, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, InputMode::Auto) => {
                Focus::Mode
            }
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mv6, InputMode::Auto) => Focus::Mute,
            (Tab::Main, _, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, _) => Focus::Mode,

            // ── MVX2U Manual reverse ──────────────────────────────────────────
            (Tab::Main, Focus::Mode, DeviceModel::Mvx2u, InputMode::Manual) => Focus::Lock,
            (Tab::Main, Focus::Mute, DeviceModel::Mvx2u, InputMode::Manual) => Focus::Mode,
            (Tab::Main, Focus::Gain, DeviceModel::Mvx2u, InputMode::Manual) => Focus::Mute,
            // ── MVX2U Auto reverse ────────────────────────────────────────────
            (Tab::Main, Focus::Mode, DeviceModel::Mvx2u, InputMode::Auto) => Focus::Lock,
            (Tab::Main, Focus::Mute, DeviceModel::Mvx2u, InputMode::Auto) => Focus::Mode,
            (Tab::Main, Focus::AutoPosition, DeviceModel::Mvx2u, InputMode::Auto) => Focus::Mute,
            (Tab::Main, Focus::AutoTone, DeviceModel::Mvx2u, InputMode::Auto) => {
                Focus::AutoPosition
            }
            (Tab::Main, Focus::AutoGain, DeviceModel::Mvx2u, InputMode::Auto) => Focus::AutoTone,
            // ── MVX2U shared tail reverse ─────────────────────────────────────
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mvx2u, InputMode::Manual) => Focus::Gain,
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mvx2u, InputMode::Auto) => Focus::AutoGain,
            (Tab::Main, Focus::Phantom, DeviceModel::Mvx2u, _) => Focus::MonitorMix,
            (Tab::Main, Focus::Lock, DeviceModel::Mvx2u, _) => Focus::Phantom,
            (Tab::Main, _, DeviceModel::Mvx2u, _) => Focus::Mode,

            // ── MV6 / MV7+ EQ ────────────────────────────────────────────────
            (Tab::Eq, _, DeviceModel::Mv6 | DeviceModel::Mv7Plus, _) => Focus::Tone,

            // ── MV6 Gen 2 EQ reverse ──────────────────────────────────────────
            (Tab::Eq, Focus::ToneMode, DeviceModel::Mv6Gen2, _)
                if self.device_state.tone_advanced =>
            {
                Focus::EqGain(self.eq_selected_band)
            }
            (Tab::Eq, Focus::ToneMode, DeviceModel::Mv6Gen2, _) => Focus::Tone,
            (Tab::Eq, Focus::EqGain(_), DeviceModel::Mv6Gen2, _) => Focus::EqBandSelect,
            (Tab::Eq, _, DeviceModel::Mv6Gen2, _) => Focus::ToneMode,

            // ── MVX2U Gen 2 Main reverse ──────────────────────────────────────
            (Tab::Main, Focus::Mode, DeviceModel::Mvx2uGen2, InputMode::Manual) => Focus::Phantom,
            (Tab::Main, Focus::Mute, DeviceModel::Mvx2uGen2, InputMode::Manual) => Focus::Mode,
            (Tab::Main, Focus::Gain, DeviceModel::Mvx2uGen2, InputMode::Manual) => Focus::Mute,
            (Tab::Main, Focus::GainLock, DeviceModel::Mvx2uGen2, InputMode::Manual) => Focus::Gain,
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mvx2uGen2, InputMode::Manual) => {
                Focus::GainLock
            }
            (Tab::Main, Focus::Mode, DeviceModel::Mvx2uGen2, InputMode::Auto) => Focus::Phantom,
            (Tab::Main, Focus::Mute, DeviceModel::Mvx2uGen2, InputMode::Auto) => Focus::Mode,
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mvx2uGen2, InputMode::Auto) => Focus::Mute,
            (Tab::Main, Focus::PlaybackMix, DeviceModel::Mvx2uGen2, _) => Focus::MonitorMix,
            (Tab::Main, Focus::Phantom, DeviceModel::Mvx2uGen2, _) => Focus::PlaybackMix,
            (Tab::Main, _, DeviceModel::Mvx2uGen2, _) => Focus::Mode,

            // ── MVX2U Gen 2 EQ reverse ────────────────────────────────────────
            (Tab::Eq, _, DeviceModel::Mvx2uGen2, InputMode::Auto) => Focus::Tone,
            (Tab::Eq, Focus::EqBandSelect, DeviceModel::Mvx2uGen2, InputMode::Manual) => {
                Focus::EqGain(self.eq_selected_band)
            }
            (Tab::Eq, Focus::EqGain(_), DeviceModel::Mvx2uGen2, InputMode::Manual) => {
                Focus::EqBandSelect
            }
            (Tab::Eq, _, DeviceModel::Mvx2uGen2, _) => Focus::EqBandSelect,

            // ── MVX2U Gen 2 Dynamics reverse ──────────────────────────────────
            (Tab::Dynamics, Focus::Limiter, DeviceModel::Mvx2uGen2, _) => Focus::Hpf,
            (Tab::Dynamics, Focus::Compressor, DeviceModel::Mvx2uGen2, _) => Focus::Limiter,
            (Tab::Dynamics, Focus::Denoiser, DeviceModel::Mvx2uGen2, InputMode::Manual) => {
                Focus::Compressor
            }
            (Tab::Dynamics, Focus::PopperStopper, DeviceModel::Mvx2uGen2, InputMode::Manual) => {
                Focus::Denoiser
            }
            (Tab::Dynamics, Focus::Hpf, DeviceModel::Mvx2uGen2, InputMode::Manual) => {
                Focus::PopperStopper
            }
            (Tab::Dynamics, Focus::Denoiser, DeviceModel::Mvx2uGen2, InputMode::Auto) => Focus::Hpf,
            (Tab::Dynamics, Focus::PopperStopper, DeviceModel::Mvx2uGen2, InputMode::Auto) => {
                Focus::Denoiser
            }
            (Tab::Dynamics, Focus::Hpf, DeviceModel::Mvx2uGen2, InputMode::Auto) => {
                Focus::PopperStopper
            }
            (Tab::Dynamics, _, DeviceModel::Mvx2uGen2, InputMode::Manual) => Focus::Limiter,
            (Tab::Dynamics, _, DeviceModel::Mvx2uGen2, _) => Focus::Denoiser,

            // ── MVX2U EQ reverse ──────────────────────────────────────────────
            (Tab::Eq, Focus::EqEnable, DeviceModel::Mvx2u, _) => {
                Focus::EqGain(self.eq_selected_band)
            }
            (Tab::Eq, Focus::EqBandSelect, DeviceModel::Mvx2u, _) => Focus::EqEnable,
            (Tab::Eq, Focus::EqBandEnable(_), DeviceModel::Mvx2u, _) => Focus::EqBandSelect,
            (Tab::Eq, Focus::EqGain(b), DeviceModel::Mvx2u, _) => Focus::EqBandEnable(*b),
            (Tab::Eq, _, DeviceModel::Mvx2u, _) => Focus::EqEnable,

            // ── MV6 Dynamics reverse ──────────────────────────────────────────
            (Tab::Dynamics, Focus::Denoiser, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, _) => {
                Focus::Hpf
            }
            (Tab::Dynamics, Focus::PopperStopper, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, _) => {
                Focus::Denoiser
            }
            (Tab::Dynamics, Focus::MuteBtnDisable, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, _) => {
                Focus::PopperStopper
            }
            (Tab::Dynamics, Focus::Hpf, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, _) => {
                Focus::MuteBtnDisable
            }
            (Tab::Dynamics, _, DeviceModel::Mv6 | DeviceModel::Mv6Gen2, _) => Focus::Denoiser,

            // ── MV7+ Main reverse
            (Tab::Main, Focus::Mode, DeviceModel::Mv7Plus, _) => Focus::PlaybackMix,
            (Tab::Main, Focus::Mute, DeviceModel::Mv7Plus, _) => Focus::Mode,
            (Tab::Main, Focus::Gain, DeviceModel::Mv7Plus, InputMode::Manual) => Focus::Mute,
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mv7Plus, InputMode::Manual) => Focus::Gain,
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mv7Plus, InputMode::Auto) => Focus::Mute,
            (Tab::Main, Focus::PlaybackMix, DeviceModel::Mv7Plus, _) => Focus::MonitorMix,
            (Tab::Main, _, DeviceModel::Mv7Plus, _) => Focus::Mode,

            // ── MV7+ Dynamics reverse ─────────────────────────────────────────
            (Tab::Dynamics, Focus::Limiter, DeviceModel::Mv7Plus, _) => Focus::Hpf,
            (Tab::Dynamics, Focus::Compressor, DeviceModel::Mv7Plus, _) => Focus::Limiter,
            (Tab::Dynamics, Focus::Denoiser, DeviceModel::Mv7Plus, _) => Focus::Compressor,
            (Tab::Dynamics, Focus::PopperStopper, DeviceModel::Mv7Plus, _) => Focus::Denoiser,
            (Tab::Dynamics, Focus::MuteBtnDisable, DeviceModel::Mv7Plus, _) => Focus::PopperStopper,
            (Tab::Dynamics, Focus::Hpf, DeviceModel::Mv7Plus, _) => Focus::MuteBtnDisable,
            (Tab::Dynamics, _, DeviceModel::Mv7Plus, _) => Focus::Limiter,

            // ── MV7 Main reverse ──────────────────────────────────────────────
            (Tab::Main, Focus::Mode, DeviceModel::Mv7, _) => Focus::Lock,
            (Tab::Main, Focus::Mute, DeviceModel::Mv7, _) => Focus::Mode,
            (Tab::Main, Focus::Gain, DeviceModel::Mv7, InputMode::Manual) => Focus::Mute,
            (Tab::Main, Focus::AutoPosition, DeviceModel::Mv7, InputMode::Auto) => Focus::Mute,
            (Tab::Main, Focus::AutoTone, DeviceModel::Mv7, InputMode::Auto) => Focus::AutoPosition,
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mv7, InputMode::Manual) => Focus::Gain,
            (Tab::Main, Focus::MonitorMix, DeviceModel::Mv7, InputMode::Auto) => Focus::AutoTone,
            (Tab::Main, Focus::Lock, DeviceModel::Mv7, _) => Focus::MonitorMix,
            (Tab::Main, _, DeviceModel::Mv7, _) => Focus::Mode,

            // ── MV7 EQ, Dynamics, LED reverse ─────────────────────────────────
            (Tab::Eq, _, DeviceModel::Mv7, _) => Focus::EqPreset,
            (Tab::Dynamics, _, DeviceModel::Mv7, _) => Focus::Compressor,
            (Tab::Led, Focus::LedNightMode, DeviceModel::Mv7, _) => Focus::LedLiveMeter,
            (Tab::Led, _, DeviceModel::Mv7, _) => Focus::LedNightMode,

            // ── Reverb tab reverse (MV7+ only)
            (Tab::Reverb, Focus::ReverbOutput, _, _) => Focus::ReverbIntensity,
            (Tab::Reverb, Focus::ReverbMonitor, _, _) => Focus::ReverbOutput,
            (Tab::Reverb, Focus::ReverbPreset, _, _) => Focus::ReverbMonitor,
            (Tab::Reverb, Focus::ReverbIntensity, _, _) => Focus::ReverbPreset,
            (Tab::Reverb, _, _, _) => Focus::ReverbOutput,

            // ── LED tab reverse (MV7+ only)
            (Tab::Led, Focus::LedBehavior, _, _) => self.led_last_color_focus(),
            (Tab::Led, Focus::LedBrightness, _, _) => Focus::LedBehavior,
            (Tab::Led, Focus::LedTheme, _, _) => Focus::LedBrightness,
            (Tab::Led, Focus::LedSolidR, _, _) => Focus::LedTheme,
            (Tab::Led, Focus::LedSolidG, _, _) => Focus::LedSolidR,
            (Tab::Led, Focus::LedSolidB, _, _) => Focus::LedSolidG,
            (Tab::Led, Focus::LedPulsingR, _, _) => Focus::LedTheme,
            (Tab::Led, Focus::LedPulsingG, _, _) => Focus::LedPulsingR,
            (Tab::Led, Focus::LedPulsingB, _, _) => Focus::LedPulsingG,
            (Tab::Led, Focus::LedLiveEdgeR, _, _) => Focus::LedTheme,
            (Tab::Led, Focus::LedLiveEdgeG, _, _) => Focus::LedLiveEdgeR,
            (Tab::Led, Focus::LedLiveEdgeB, _, _) => Focus::LedLiveEdgeG,
            (Tab::Led, Focus::LedLiveMiddleR, _, _) => Focus::LedLiveEdgeB,
            (Tab::Led, Focus::LedLiveMiddleG, _, _) => Focus::LedLiveMiddleR,
            (Tab::Led, Focus::LedLiveMiddleB, _, _) => Focus::LedLiveMiddleG,
            (Tab::Led, Focus::LedLiveInteriorR, _, _) => Focus::LedLiveMiddleB,
            (Tab::Led, Focus::LedLiveInteriorG, _, _) => Focus::LedLiveInteriorR,
            (Tab::Led, Focus::LedLiveInteriorB, _, _) => Focus::LedLiveInteriorG,
            (Tab::Led, _, _, _) => Focus::LedBehavior,

            // ── MVX2U Dynamics reverse ────────────────────────────────────────
            (Tab::Dynamics, Focus::Limiter, DeviceModel::Mvx2u, _) => Focus::Hpf,
            (Tab::Dynamics, Focus::Compressor, DeviceModel::Mvx2u, _) => Focus::Limiter,
            (Tab::Dynamics, Focus::Hpf, DeviceModel::Mvx2u, _) => Focus::Compressor,
            (Tab::Dynamics, _, DeviceModel::Mvx2u, _) => Focus::Limiter,

            (Tab::Presets, Focus::PresetName(0), _, _) => Focus::PresetActions(PRESET_COUNT - 1),
            (Tab::Presets, Focus::PresetName(i), _, _) => Focus::PresetActions(i - 1),
            (Tab::Presets, Focus::PresetActions(i), _, _) => Focus::PresetName(*i),
            (Tab::Presets, _, _, _) => Focus::PresetName(0),

            // Info: Device Name ↔ Factory Reset where both exist.
            (Tab::Info, Focus::DeviceName, _, _) if self.supports_factory_reset() => {
                Focus::FactoryReset
            }
            (Tab::Info, Focus::FactoryReset, model, _) if model.has_device_rename() => {
                Focus::DeviceName
            }

            _ => self.focus,
        };
    }

    // ── LED focus helpers ─────────────────────────────────────────────────────

    /// First focusable color control given current behavior + theme, or LedBehavior if none.
    fn led_first_color_focus(&self) -> Focus {
        let ds = &self.device_state;
        match ds.led_behavior {
            LedBehavior::Solid if ds.led_solid_theme == LedSolidTheme::Custom => Focus::LedSolidR,
            LedBehavior::Pulsing if ds.led_pulsing_theme == LedPulsingTheme::Custom => {
                Focus::LedPulsingR
            }
            LedBehavior::Live if ds.led_live_theme == LedLiveTheme::Custom => Focus::LedLiveEdgeR,
            _ => Focus::LedBehavior,
        }
    }

    /// Last focusable color control given current behavior + theme, or LedTheme if none.
    fn led_last_color_focus(&self) -> Focus {
        let ds = &self.device_state;
        match ds.led_behavior {
            LedBehavior::Solid if ds.led_solid_theme == LedSolidTheme::Custom => Focus::LedSolidB,
            LedBehavior::Pulsing if ds.led_pulsing_theme == LedPulsingTheme::Custom => {
                Focus::LedPulsingB
            }
            LedBehavior::Live if ds.led_live_theme == LedLiveTheme::Custom => {
                Focus::LedLiveInteriorB
            }
            _ => Focus::LedTheme,
        }
    }

    // ── Value adjustment helpers ──────────────────────────────────────────────
    /// Increment the focused parameter by `delta` (usually ±1).
    pub fn adjust_focused(&mut self, delta: i32) -> Option<DeviceAction> {
        match self.focus {
            Focus::Gain => {
                if self.gain_locked() {
                    return None;
                }
                let step = i32::from(self.device_model.gain_step_tenths());
                let moved = (i32::from(self.device_state.gain_tenths) + delta * step).max(0);
                // Land on the step grid even if the device reported an off-grid value.
                self.device_state.gain_tenths = self
                    .device_model
                    .snap_gain_tenths(u16::try_from(moved).unwrap_or(u16::MAX));
                Some(DeviceAction::SetGain(self.device_state.gain_tenths))
            }
            Focus::MonitorMix => {
                let m = &mut self.device_state.monitor_mix;
                if delta > 0 {
                    *m = (*m + 1).min(100);
                } else {
                    *m = m.saturating_sub(1);
                }
                Some(DeviceAction::SetMonitorMix(self.device_state.monitor_mix))
            }
            Focus::PlaybackMix => {
                let m = &mut self.device_state.playback_mix;
                if delta > 0 {
                    *m = (*m + 1).min(100);
                } else {
                    *m = m.saturating_sub(1);
                }
                Some(DeviceAction::SetMv7PlaybackMix(
                    self.device_state.playback_mix,
                ))
            }
            Focus::ReverbIntensity => {
                let i = &mut self.device_state.reverb_intensity;
                if delta > 0 {
                    *i = (*i + 1).min(100);
                } else {
                    *i = i.saturating_sub(1);
                }
                Some(DeviceAction::SetMv7ReverbIntensity(
                    self.device_state.reverb_intensity,
                ))
            }
            // Enter toggles the Denoiser; ←/→ sets its level where it has one.
            Focus::Denoiser if self.device_model.has_denoiser_level() => {
                let level = i32::from(self.device_state.denoiser_level) + delta;
                self.device_state.denoiser_level = level.clamp(1, 100) as u8;
                Some(DeviceAction::SetDenoiserLevel(
                    self.device_state.denoiser_level,
                ))
            }
            Focus::Tone => {
                let t = &mut self.device_state.tone;
                *t = ((*t as i32) + delta).clamp(-10, 10) as i8;
                Some(DeviceAction::SetMv6Tone(self.device_state.tone))
            }
            Focus::LedSolidR => {
                self.device_state.led_solid_rgb[0] =
                    ((self.device_state.led_solid_rgb[0] as i32) + delta).clamp(0, 255) as u8;
                Some(DeviceAction::SetMv7LedSolidRgb(
                    self.device_state.led_solid_rgb,
                ))
            }
            Focus::LedSolidG => {
                self.device_state.led_solid_rgb[1] =
                    ((self.device_state.led_solid_rgb[1] as i32) + delta).clamp(0, 255) as u8;
                Some(DeviceAction::SetMv7LedSolidRgb(
                    self.device_state.led_solid_rgb,
                ))
            }
            Focus::LedSolidB => {
                self.device_state.led_solid_rgb[2] =
                    ((self.device_state.led_solid_rgb[2] as i32) + delta).clamp(0, 255) as u8;
                Some(DeviceAction::SetMv7LedSolidRgb(
                    self.device_state.led_solid_rgb,
                ))
            }
            Focus::LedPulsingR => {
                self.device_state.led_pulsing_rgb[0] =
                    ((self.device_state.led_pulsing_rgb[0] as i32) + delta).clamp(0, 255) as u8;
                Some(DeviceAction::SetMv7LedPulsingRgb(
                    self.device_state.led_pulsing_rgb,
                ))
            }
            Focus::LedPulsingG => {
                self.device_state.led_pulsing_rgb[1] =
                    ((self.device_state.led_pulsing_rgb[1] as i32) + delta).clamp(0, 255) as u8;
                Some(DeviceAction::SetMv7LedPulsingRgb(
                    self.device_state.led_pulsing_rgb,
                ))
            }
            Focus::LedPulsingB => {
                self.device_state.led_pulsing_rgb[2] =
                    ((self.device_state.led_pulsing_rgb[2] as i32) + delta).clamp(0, 255) as u8;
                Some(DeviceAction::SetMv7LedPulsingRgb(
                    self.device_state.led_pulsing_rgb,
                ))
            }
            Focus::LedLiveEdgeR => {
                self.device_state.led_live_edge_rgb[0] =
                    ((self.device_state.led_live_edge_rgb[0] as i32) + delta).clamp(0, 255) as u8;
                Some(DeviceAction::SetMv7LedLiveEdgeRgb(
                    self.device_state.led_live_edge_rgb,
                ))
            }
            Focus::LedLiveEdgeG => {
                self.device_state.led_live_edge_rgb[1] =
                    ((self.device_state.led_live_edge_rgb[1] as i32) + delta).clamp(0, 255) as u8;
                Some(DeviceAction::SetMv7LedLiveEdgeRgb(
                    self.device_state.led_live_edge_rgb,
                ))
            }
            Focus::LedLiveEdgeB => {
                self.device_state.led_live_edge_rgb[2] =
                    ((self.device_state.led_live_edge_rgb[2] as i32) + delta).clamp(0, 255) as u8;
                Some(DeviceAction::SetMv7LedLiveEdgeRgb(
                    self.device_state.led_live_edge_rgb,
                ))
            }
            Focus::LedLiveMiddleR => {
                self.device_state.led_live_middle_rgb[0] =
                    ((self.device_state.led_live_middle_rgb[0] as i32) + delta).clamp(0, 255) as u8;
                Some(DeviceAction::SetMv7LedLiveMiddleRgb(
                    self.device_state.led_live_middle_rgb,
                ))
            }
            Focus::LedLiveMiddleG => {
                self.device_state.led_live_middle_rgb[1] =
                    ((self.device_state.led_live_middle_rgb[1] as i32) + delta).clamp(0, 255) as u8;
                Some(DeviceAction::SetMv7LedLiveMiddleRgb(
                    self.device_state.led_live_middle_rgb,
                ))
            }
            Focus::LedLiveMiddleB => {
                self.device_state.led_live_middle_rgb[2] =
                    ((self.device_state.led_live_middle_rgb[2] as i32) + delta).clamp(0, 255) as u8;
                Some(DeviceAction::SetMv7LedLiveMiddleRgb(
                    self.device_state.led_live_middle_rgb,
                ))
            }
            Focus::LedLiveInteriorR => {
                self.device_state.led_live_interior_rgb[0] =
                    ((self.device_state.led_live_interior_rgb[0] as i32) + delta).clamp(0, 255)
                        as u8;
                Some(DeviceAction::SetMv7LedLiveInteriorRgb(
                    self.device_state.led_live_interior_rgb,
                ))
            }
            Focus::LedLiveInteriorG => {
                self.device_state.led_live_interior_rgb[1] =
                    ((self.device_state.led_live_interior_rgb[1] as i32) + delta).clamp(0, 255)
                        as u8;
                Some(DeviceAction::SetMv7LedLiveInteriorRgb(
                    self.device_state.led_live_interior_rgb,
                ))
            }
            Focus::LedLiveInteriorB => {
                self.device_state.led_live_interior_rgb[2] =
                    ((self.device_state.led_live_interior_rgb[2] as i32) + delta).clamp(0, 255)
                        as u8;
                Some(DeviceAction::SetMv7LedLiveInteriorRgb(
                    self.device_state.led_live_interior_rgb,
                ))
            }
            Focus::EqBandSelect => {
                if delta > 0 {
                    self.eq_selected_band = (self.eq_selected_band + 1) % 5;
                } else {
                    self.eq_selected_band = (self.eq_selected_band + 4) % 5;
                }
                None
            }
            Focus::EqGain(b) => {
                let model = self.device_model;
                let band = &mut self.device_state.eq_bands[b];
                let moved = i32::from(band.gain_db) + delta * i32::from(model.eq_step_tenths());
                // Land on the step grid even if the device reported an off-grid value.
                band.gain_db = model.snap_eq_gain_tenths(moved.clamp(-80, 60) as i16);
                Some(DeviceAction::SetEqBandGain(b, band.gain_db))
            }
            _ => None,
        }
    }

    /// Toggle the focused boolean control.
    pub fn toggle_focused(&mut self) -> Option<DeviceAction> {
        match self.focus {
            Focus::Mode => {
                self.device_state.mode = match self.device_state.mode {
                    InputMode::Auto => InputMode::Manual,
                    InputMode::Manual => InputMode::Auto,
                };
                self.focus = match (self.device_model, self.device_state.mode) {
                    (DeviceModel::Mvx2u | DeviceModel::Mv7, InputMode::Auto) => Focus::AutoPosition,
                    (DeviceModel::Mv6Gen2, InputMode::Auto) => Focus::AutoGain,
                    (_, InputMode::Manual) => Focus::Gain,
                    (_, InputMode::Auto) => Focus::Mute,
                };
                Some(DeviceAction::SetMode(self.device_state.mode))
            }
            Focus::AutoPosition => {
                self.device_state.auto_position = self.device_state.auto_position.cycle_next();
                Some(DeviceAction::SetAutoPosition(
                    self.device_state.auto_position,
                ))
            }
            Focus::AutoTone => {
                self.device_state.auto_tone = self.device_state.auto_tone.cycle_next();
                Some(DeviceAction::SetAutoTone(self.device_state.auto_tone))
            }
            Focus::AutoGain => {
                self.device_state.auto_gain = self.device_state.auto_gain.cycle_next();
                Some(DeviceAction::SetAutoGain(self.device_state.auto_gain))
            }
            // The mic sets the preset's mode, Denoiser and Popper Stopper itself;
            // mirror that so the screen matches without a refresh.
            Focus::MicPreset => {
                let next = MicPreset::matching(&self.device_state)
                    .map_or(MicPreset::Speech, MicPreset::cycle_next);
                next.apply_to(&mut self.device_state);
                Some(DeviceAction::SetMicPreset(next))
            }
            Focus::ToneMode => {
                self.device_state.tone_advanced = !self.device_state.tone_advanced;
                Some(DeviceAction::SetToneMode(self.device_state.tone_advanced))
            }
            Focus::AdaptationRate => {
                self.device_state.adaptation_rate_fast = !self.device_state.adaptation_rate_fast;
                Some(DeviceAction::SetAdaptationRate(
                    self.device_state.adaptation_rate_fast,
                ))
            }
            Focus::Mute => {
                self.device_state.muted = !self.device_state.muted;
                Some(DeviceAction::SetMute(self.device_state.muted))
            }
            Focus::Phantom => {
                self.device_state.phantom_power = !self.device_state.phantom_power;
                Some(DeviceAction::SetPhantom(self.device_state.phantom_power))
            }
            Focus::Lock => {
                self.device_state.locked = !self.device_state.locked;
                Some(DeviceAction::SetLock(self.device_state.locked))
            }
            Focus::EqEnable => {
                self.device_state.eq_enabled = !self.device_state.eq_enabled;
                Some(DeviceAction::SetEqEnable(self.device_state.eq_enabled))
            }
            Focus::EqBandEnable(b) => {
                self.device_state.eq_bands[b].enabled = !self.device_state.eq_bands[b].enabled;
                Some(DeviceAction::SetEqBandEnable(
                    b,
                    self.device_state.eq_bands[b].enabled,
                ))
            }
            Focus::Limiter => {
                self.device_state.limiter_enabled = !self.device_state.limiter_enabled;
                Some(DeviceAction::SetLimiter(self.device_state.limiter_enabled))
            }
            Focus::Compressor => {
                self.device_state.compressor = self.device_state.compressor.cycle_next();
                Some(DeviceAction::SetCompressor(self.device_state.compressor))
            }
            Focus::Hpf => {
                self.device_state.hpf = self.device_state.hpf.cycle_next();
                Some(DeviceAction::SetHpf(self.device_state.hpf))
            }
            // ── MV6-family Dynamics controls (also MVX2U Gen 2, MV7+) ─────────
            Focus::Denoiser => {
                self.device_state.denoiser_enabled = !self.device_state.denoiser_enabled;
                Some(DeviceAction::SetMv6Denoiser(
                    self.device_state.denoiser_enabled,
                ))
            }
            Focus::PopperStopper => {
                self.device_state.popper_stopper_enabled =
                    !self.device_state.popper_stopper_enabled;
                Some(DeviceAction::SetMv6PopperStopper(
                    self.device_state.popper_stopper_enabled,
                ))
            }
            Focus::MuteBtnDisable => {
                let Some(disabled) = self.device_state.mute_btn_disabled else {
                    self.report_unavailable_mute_button();
                    return None;
                };
                self.device_state.mute_btn_disabled = Some(!disabled);
                Some(DeviceAction::SetMv6MuteBtnDisable(!disabled))
            }
            Focus::ReverbOutput => {
                self.device_state.reverb_on_output = !self.device_state.reverb_on_output;
                Some(DeviceAction::SetMv7ReverbOutput(
                    self.device_state.reverb_on_output,
                ))
            }
            Focus::ReverbMonitor => {
                self.device_state.reverb_monitoring = !self.device_state.reverb_monitoring;
                Some(DeviceAction::SetMv7ReverbMonitor(
                    self.device_state.reverb_monitoring,
                ))
            }
            Focus::ReverbPreset => {
                self.device_state.reverb_type = self.device_state.reverb_type.cycle_next();
                Some(DeviceAction::SetMv7ReverbPreset(
                    self.device_state.reverb_type,
                ))
            }
            Focus::GainLock => {
                self.device_state.mv6_gain_locked = !self.device_state.mv6_gain_locked;
                Some(DeviceAction::SetMv6GainLock(
                    self.device_state.mv6_gain_locked,
                ))
            }
            Focus::LedBehavior => {
                self.device_state.led_behavior = self.device_state.led_behavior.cycle_next();
                Some(DeviceAction::SetMv7LedBehavior(
                    self.device_state.led_behavior,
                ))
            }
            Focus::LedBrightness => {
                self.device_state.led_brightness = self.device_state.led_brightness.cycle_next();
                Some(DeviceAction::SetMv7LedBrightness(
                    self.device_state.led_brightness,
                ))
            }
            Focus::LedTheme => match self.device_state.led_behavior {
                LedBehavior::Live => {
                    self.device_state.led_live_theme =
                        self.device_state.led_live_theme.cycle_next();
                    Some(DeviceAction::SetMv7LedLiveTheme(
                        self.device_state.led_live_theme,
                    ))
                }
                LedBehavior::Solid => {
                    self.device_state.led_solid_theme =
                        self.device_state.led_solid_theme.cycle_next();
                    Some(DeviceAction::SetMv7LedSolidTheme(
                        self.device_state.led_solid_theme,
                    ))
                }
                LedBehavior::Pulsing => {
                    self.device_state.led_pulsing_theme =
                        self.device_state.led_pulsing_theme.cycle_next();
                    Some(DeviceAction::SetMv7LedPulsingTheme(
                        self.device_state.led_pulsing_theme,
                    ))
                }
            },
            // ── MV7 controls ──────────────────────────────────────────────────
            Focus::EqPreset => {
                self.device_state.eq_preset = self.device_state.eq_preset.cycle_next();
                Some(DeviceAction::SetEqPreset(self.device_state.eq_preset))
            }
            Focus::LedLiveMeter => {
                self.device_state.led_live_meter = !self.device_state.led_live_meter;
                Some(DeviceAction::SetLedLiveMeter(
                    self.device_state.led_live_meter,
                ))
            }
            Focus::LedNightMode => {
                self.device_state.led_night_mode = !self.device_state.led_night_mode;
                Some(DeviceAction::SetLedNightMode(
                    self.device_state.led_night_mode,
                ))
            }
            Focus::PresetActions(i) => {
                if self.presets[i].is_some() {
                    Some(DeviceAction::LoadPreset(i))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Status for a Mute Button change refused because its state wasn't read.
const MUTE_BTN_UNREAD: &str = "Mute Button unavailable: its state could not be read";

/// Commands to send to the device, produced by App but executed by main.
#[derive(Debug)]
pub enum DeviceAction {
    /// Gain in tenths of a dB.
    SetGain(u16),
    SetMode(InputMode),
    SetAutoPosition(MicPosition),
    SetAutoTone(AutoTone),
    SetAutoGain(AutoGain),
    SetMute(bool),
    SetPhantom(bool),
    SetLock(bool),
    SetLimiter(bool),
    SetCompressor(CompressorPreset),
    SetHpf(HpfFrequency),
    SetEqEnable(bool),
    /// `usize` is band index 0–4.
    SetEqBandEnable(usize, bool),
    /// `usize` is band index 0–4; `i16` is gain in tenths of dB (−80..+60).
    /// On the model's `eq_step_tenths()` grid.
    SetEqBandGain(usize, i16),
    // ── Named for the MV6, which had them first ───────────────────────────────
    SetMv6Denoiser(bool),
    SetMv6PopperStopper(bool),
    SetMv6MuteBtnDisable(bool),
    /// Tone in steps of 1 (−10 to +10); displayed as × 10%.
    SetMv6Tone(i8),
    SetMv6GainLock(bool),
    /// Monitor Mix, 0–100%: the mic ↔ playback balance on the MVX2U Gen 1 and
    /// MV7, Mic Level on models with `has_playback_mix()`. The Gen 1 balance is
    /// FEAT_MIX prefix 0x01 with the standard header; the Mic Level is prefix
    /// 0x00 with HDR_CONSTANT=0x00.
    SetMonitorMix(u8),
    /// Playback Level, 0–100% (models with `has_playback_mix()`).
    SetMv7PlaybackMix(u8),
    // ── MV6 Gen 2 actions ─────────────────────────────────────────────────────
    /// Auto Level Adaptation Rate: false = Slow, true = Fast.
    SetAdaptationRate(bool),
    /// Denoiser level, 1–100%.
    SetDenoiserLevel(u8),
    /// Tone mode: false = Simple (Tone slider), true = Advanced (5-band EQ).
    SetToneMode(bool),
    /// Select a built-in preset; the mic applies its settings.
    SetMicPreset(MicPreset),
    // ── MV7+ exclusive actions ────────────────────────────────────────────────
    SetMv7ReverbOutput(bool),
    SetMv7ReverbMonitor(bool),
    SetMv7ReverbPreset(ReverbType),
    SetMv7ReverbIntensity(u8),
    SetMv7LedBehavior(LedBehavior),
    SetMv7LedBrightness(LedBrightness),
    SetMv7LedLiveTheme(LedLiveTheme),
    SetMv7LedSolidTheme(LedSolidTheme),
    SetMv7LedPulsingTheme(LedPulsingTheme),
    SetMv7LedSolidRgb([u8; 3]),
    SetMv7LedPulsingRgb([u8; 3]),
    SetMv7LedLiveEdgeRgb([u8; 3]),
    SetMv7LedLiveMiddleRgb([u8; 3]),
    SetMv7LedLiveInteriorRgb([u8; 3]),
    // ── MV7 exclusive actions ─────────────────────────────────────────────────
    SetEqPreset(EqPreset),
    SetLedLiveMeter(bool),
    SetLedNightMode(bool),
    // ── Preset actions ────────────────────────────────────────────────────────
    SavePreset(usize),
    LoadPreset(usize),
    DeletePreset(usize),
    /// Write the (already in-memory-updated) preset name for slot `usize` back to disk.
    PersistPresetName(usize),
    /// Write a new user-set name to the device.
    RenameDevice(String),
    /// Zero all 5 EQ band gains. Gen 1: leaves EQ master and per-band enables untouched.
    FlattenEq,
    Refresh,
    /// Open the device again after it was unplugged and plugged back in, and
    /// load its state. Sent by main's presence poll rather than a key.
    Reconnect,
    /// Send a factory reset command (MV7+, MV6 Gen 2, MVX2U Gen 2). Device disconnects immediately after.
    FactoryReset,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── reset_focus_for_tab (exercised via next_tab / prev_tab) ──────────────

    #[test]
    fn info_tab_focuses_device_name_only_where_rename_is_supported() {
        for (model, expected) in [
            (DeviceModel::Mvx2uGen2, Focus::DeviceName),
            (DeviceModel::Mv7Plus, Focus::DeviceName),
            (DeviceModel::Mv7, Focus::FactoryReset),
        ] {
            let mut app = App {
                device_model: model,
                active_tab: Tab::Presets,
                ..App::default()
            };
            app.next_tab();
            assert_eq!(app.active_tab, Tab::Info);
            assert_eq!(app.focus, expected, "{model:?}");
        }
    }

    #[test]
    fn info_tab_focus_moves_between_device_name_and_factory_reset() {
        let mut app = App {
            device_model: DeviceModel::Mv7Plus,
            active_tab: Tab::Info,
            focus: Focus::DeviceName,
            ..App::default()
        };
        app.focus_next();
        assert_eq!(app.focus, Focus::FactoryReset);
        app.focus_next();
        assert_eq!(app.focus, Focus::DeviceName);
        app.focus_prev();
        assert_eq!(app.focus, Focus::FactoryReset);
        app.device_model = DeviceModel::Mv6;
        app.focus = Focus::DeviceName;
        app.focus_next();
        assert_eq!(app.focus, Focus::DeviceName, "the MV6 has no factory reset");
    }

    #[test]
    fn eq_tab_focus_cycles_enable_band_select_enable_gain() {
        let mut app = App {
            active_tab: Tab::Eq,
            focus: Focus::EqEnable,
            eq_selected_band: 0,
            ..App::default()
        };

        // Forward: EqEnable → EqBandSelect → EqBandEnable(0) → EqGain(0) → EqEnable
        app.focus_next();
        assert_eq!(app.focus, Focus::EqBandSelect);
        app.focus_next();
        assert_eq!(app.focus, Focus::EqBandEnable(0));
        app.focus_next();
        assert_eq!(app.focus, Focus::EqGain(0));
        app.focus_next();
        assert_eq!(app.focus, Focus::EqEnable);

        // Backward from EqEnable wraps to EqGain
        app.focus_prev();
        assert_eq!(app.focus, Focus::EqGain(0));
    }

    #[test]
    fn main_tab_manual_mode_focus_cycles_forward_and_back() {
        // Manual: Mode → Mute → Gain → MonitorMix → Phantom → Lock → Mode
        let forward = [
            Focus::Mode,
            Focus::Mute,
            Focus::Gain,
            Focus::MonitorMix,
            Focus::Phantom,
            Focus::Lock,
            Focus::Mode, // wrap
        ];
        let mut app = App {
            active_tab: Tab::Main,
            focus: Focus::Mode,
            ..App::default()
        };
        app.device_state.mode = InputMode::Manual;

        for expected in &forward[1..] {
            app.focus_next();
            assert_eq!(app.focus, *expected);
        }

        // Backward from Mode wraps to Lock
        app.focus = Focus::Mode;
        app.focus_prev();
        assert_eq!(app.focus, Focus::Lock);
    }

    #[test]
    fn main_tab_auto_mode_focus_cycles_forward_and_back() {
        // Auto: Mode → Mute → AutoPosition → AutoTone → AutoGain → MonitorMix → Phantom → Lock → Mode
        let forward = [
            Focus::Mode,
            Focus::Mute,
            Focus::AutoPosition,
            Focus::AutoTone,
            Focus::AutoGain,
            Focus::MonitorMix,
            Focus::Phantom,
            Focus::Lock,
            Focus::Mode, // wrap
        ];
        let mut app = App {
            active_tab: Tab::Main,
            focus: Focus::Mode,
            ..App::default()
        };
        app.device_state.mode = InputMode::Auto;

        for expected in &forward[1..] {
            app.focus_next();
            assert_eq!(app.focus, *expected);
        }

        // Backward from Mode wraps to Lock
        app.focus = Focus::Mode;
        app.focus_prev();
        assert_eq!(app.focus, Focus::Lock);
    }

    #[test]
    fn mv6_main_tab_manual_mode_focus_cycles_forward_and_back() {
        // MV6 Manual: Mode → Mute → Gain → GainLock → MonitorMix → PlaybackMix → Mode
        let forward = [
            Focus::Mode,
            Focus::Mute,
            Focus::Gain,
            Focus::GainLock,
            Focus::MonitorMix,
            Focus::PlaybackMix,
            Focus::Mode, // wrap
        ];
        let mut app = App {
            device_model: DeviceModel::Mv6,
            active_tab: Tab::Main,
            focus: Focus::Mode,
            ..App::default()
        };
        app.device_state.mode = InputMode::Manual;

        for expected in &forward[1..] {
            app.focus_next();
            assert_eq!(app.focus, *expected, "focus_next: expected {expected:?}");
        }

        // Backward from Mode wraps to PlaybackMix, then MonitorMix, then GainLock
        app.focus = Focus::Mode;
        app.focus_prev();
        assert_eq!(app.focus, Focus::PlaybackMix);
        app.focus_prev();
        assert_eq!(app.focus, Focus::MonitorMix);
        app.focus_prev();
        assert_eq!(app.focus, Focus::GainLock);
    }

    #[test]
    fn mv6_main_tab_auto_mode_focus_cycles_forward_and_back() {
        // MV6 Auto: Mode → Mute → MonitorMix → PlaybackMix → Mode
        let mut app = App {
            device_model: DeviceModel::Mv6,
            active_tab: Tab::Main,
            focus: Focus::Mode,
            ..App::default()
        };
        app.device_state.mode = InputMode::Auto;

        app.focus_next();
        assert_eq!(app.focus, Focus::Mute);
        app.focus_next();
        assert_eq!(app.focus, Focus::MonitorMix);
        app.focus_next();
        assert_eq!(app.focus, Focus::PlaybackMix);
        app.focus_next();
        assert_eq!(app.focus, Focus::Mode); // wrap

        // Backward from Mode wraps to PlaybackMix, then MonitorMix
        app.focus_prev();
        assert_eq!(app.focus, Focus::PlaybackMix);
        app.focus_prev();
        assert_eq!(app.focus, Focus::MonitorMix);
        app.focus_prev();
        assert_eq!(app.focus, Focus::Mute);
    }

    #[test]
    fn mv6_gen2_auto_mode_focus_visits_its_auto_level_controls() {
        // MV6 Gen 2 Auto: Mode → Mute → Target Level → Adaptation Rate → MonitorMix
        // → PlaybackMix
        let mut app = App {
            device_model: DeviceModel::Mv6Gen2,
            active_tab: Tab::Main,
            focus: Focus::Mode,
            ..App::default()
        };
        app.device_state.mode = InputMode::Auto;
        let order = [
            Focus::Mute,
            Focus::AutoGain,
            Focus::AdaptationRate,
            Focus::MonitorMix,
            Focus::PlaybackMix,
            Focus::MicPreset,
            Focus::Mode,
        ];
        for expected in order {
            app.focus_next();
            assert_eq!(app.focus, expected);
        }
        for expected in order.iter().rev().skip(1) {
            app.focus_prev();
            assert_eq!(app.focus, *expected);
        }

        // Manual matches the MV6 (no Auto Level controls) plus Mic Preset.
        app.device_state.mode = InputMode::Manual;
        app.focus = Focus::Mode;
        let order = [
            Focus::Mute,
            Focus::Gain,
            Focus::GainLock,
            Focus::MonitorMix,
            Focus::PlaybackMix,
            Focus::MicPreset,
            Focus::Mode,
        ];
        for expected in order {
            app.focus_next();
            assert_eq!(app.focus, expected);
        }
        app.focus_prev();
        app.focus_prev();
        assert_eq!(app.focus, Focus::PlaybackMix);
        assert!(matches!(
            app.adjust_focused(1),
            Some(DeviceAction::SetMv7PlaybackMix(1))
        ));
    }

    #[test]
    fn mv6_gen2_mic_preset_cycles_and_applies_its_settings() {
        let mut app = App {
            device_model: DeviceModel::Mv6Gen2,
            focus: Focus::MicPreset,
            ..App::default()
        };
        // Matching no preset, Enter starts at Speech.
        app.device_state.denoiser_enabled = false;
        app.device_state.mode = InputMode::Auto;
        let expected = [
            (MicPreset::Speech, InputMode::Auto, true, true),
            (MicPreset::Singing, InputMode::Manual, false, true),
            (MicPreset::Instrument, InputMode::Manual, false, false),
            (MicPreset::Speech, InputMode::Auto, true, true),
        ];
        for (preset, mode, denoiser, popper) in expected {
            assert!(
                matches!(app.toggle_focused(), Some(DeviceAction::SetMicPreset(p)) if p == preset)
            );
            let ds = &app.device_state;
            assert_eq!(
                (ds.mode, ds.denoiser_enabled, ds.popper_stopper_enabled),
                (mode, denoiser, popper)
            );
            assert_eq!(
                app.focus,
                Focus::MicPreset,
                "focus stays when the mode changes"
            );
        }
    }

    #[test]
    fn mv6_gen2_adaptation_rate_toggles_and_denoiser_level_adjusts() {
        let mut app = App {
            device_model: DeviceModel::Mv6Gen2,
            focus: Focus::AdaptationRate,
            ..App::default()
        };
        assert!(matches!(
            app.toggle_focused(),
            Some(DeviceAction::SetAdaptationRate(true))
        ));

        app.focus = Focus::Denoiser;
        app.device_state.denoiser_level = 100;
        assert!(matches!(
            app.adjust_focused(1),
            Some(DeviceAction::SetDenoiserLevel(100))
        ));
        app.device_state.denoiser_level = 1;
        assert!(matches!(
            app.adjust_focused(-1),
            Some(DeviceAction::SetDenoiserLevel(1))
        ));
        assert!(matches!(
            app.adjust_focused(1),
            Some(DeviceAction::SetDenoiserLevel(2))
        ));

        // The MV6 Denoiser has no level, so ←/→ does nothing there.
        app.device_model = DeviceModel::Mv6;
        assert!(app.adjust_focused(1).is_none());
    }

    #[test]
    fn mv6_gen2_eq_tab_switches_between_tone_slider_and_bands() {
        let mut app = App {
            device_model: DeviceModel::Mv6Gen2,
            ..App::default()
        };
        app.select_tab(Tab::Eq);
        assert_eq!(app.focus, Focus::ToneMode);
        assert!(!app.can_flatten_eq(), "Simple shows the Tone slider");

        // Simple: ToneMode ↔ Tone.
        app.focus_next();
        assert_eq!(app.focus, Focus::Tone);
        app.focus_next();
        assert_eq!(app.focus, Focus::ToneMode);

        // Enter switches to Advanced: ToneMode → band select → band gain.
        assert!(matches!(
            app.toggle_focused(),
            Some(DeviceAction::SetToneMode(true))
        ));
        assert!(app.can_flatten_eq());
        app.focus_next();
        assert_eq!(app.focus, Focus::EqBandSelect);
        app.focus_next();
        assert_eq!(app.focus, Focus::EqGain(0));
        // MOTIV steps the bands in 2 dB.
        assert!(matches!(
            app.adjust_focused(1),
            Some(DeviceAction::SetEqBandGain(0, 20))
        ));
        app.focus_prev();
        app.focus_prev();
        assert_eq!(app.focus, Focus::ToneMode);

        // A refresh that finds Simple while a band is focused re-picks focus.
        app.focus = Focus::EqGain(0);
        app.device_state.tone_advanced = false;
        app.settle_focus(app.device_state.mode);
        assert_eq!(app.focus, Focus::ToneMode);
        assert!(!app.is_tab_locked(Tab::Eq));
    }

    #[test]
    fn dynamics_tab_focus_cycles_all_three_controls() {
        let mut app = App {
            active_tab: Tab::Dynamics,
            focus: Focus::Limiter,
            ..App::default()
        };

        app.focus_next();
        assert_eq!(app.focus, Focus::Compressor);
        app.focus_next();
        assert_eq!(app.focus, Focus::Hpf);
        app.focus_next();
        assert_eq!(app.focus, Focus::Limiter); // wrap

        // Backwards
        app.focus_prev();
        assert_eq!(app.focus, Focus::Hpf);
    }

    #[test]
    fn presets_tab_focus_cycles_through_name_and_actions_rows() {
        let mut app = App {
            active_tab: Tab::Presets,
            ..App::default()
        };

        // Walk forward through all slots: Name(0) → Actions(0) → Name(1) → ... → Actions(3) → Name(0)
        app.focus = Focus::PresetName(0);
        let expected_forward = [
            Focus::PresetActions(0),
            Focus::PresetName(1),
            Focus::PresetActions(1),
            Focus::PresetName(2),
            Focus::PresetActions(2),
            Focus::PresetName(3),
            Focus::PresetActions(3),
            Focus::PresetName(0), // wraps
        ];
        for expected in expected_forward {
            app.focus_next();
            assert_eq!(app.focus, expected, "focus_next mismatch");
        }

        // Walk backward: Actions(3) → Name(3) → Actions(2) → ... → Name(0) → Actions(3)
        app.focus = Focus::PresetActions(3);
        let expected_backward = [
            Focus::PresetName(3),
            Focus::PresetActions(2),
            Focus::PresetName(2),
            Focus::PresetActions(1),
            Focus::PresetName(1),
            Focus::PresetActions(0),
            Focus::PresetName(0),
            Focus::PresetActions(3), // wraps
        ];
        for expected in expected_backward {
            app.focus_prev();
            assert_eq!(app.focus, expected, "focus_prev mismatch");
        }
    }

    // ── adjust_focused ────────────────────────────────────────────────────────

    #[test]
    fn adjust_gain_steps_one_db_and_snaps_off_grid_values() {
        let mut app = App {
            focus: Focus::Gain,
            ..App::default()
        };
        // A device that reports a fractional gain lands back on the 1 dB grid.
        app.device_state.gain_tenths = 283;

        assert!(matches!(
            app.adjust_focused(1),
            Some(DeviceAction::SetGain(290))
        ));
        assert!(matches!(
            app.adjust_focused(1),
            Some(DeviceAction::SetGain(300))
        ));
        assert!(matches!(
            app.adjust_focused(-1),
            Some(DeviceAction::SetGain(290))
        ));
    }

    #[test]
    fn adjust_gain_on_mv7_uses_the_1_5_db_hardware_step() {
        let mut app = App {
            device_model: DeviceModel::Mv7,
            focus: Focus::Gain,
            ..App::default()
        };
        app.device_state.gain_tenths = 285;

        assert!(matches!(
            app.adjust_focused(1),
            Some(DeviceAction::SetGain(300))
        ));
        assert!(matches!(
            app.adjust_focused(-1),
            Some(DeviceAction::SetGain(285))
        ));
        app.device_state.gain_tenths = 355;
        assert!(matches!(
            app.adjust_focused(1),
            Some(DeviceAction::SetGain(360))
        ));
        assert!(matches!(
            app.adjust_focused(1),
            Some(DeviceAction::SetGain(360))
        ));
    }

    #[test]
    fn adjust_monitor_mix_dispatches_set_monitor_mix_on_every_model() {
        for model in [
            DeviceModel::Mvx2u,
            DeviceModel::Mvx2uGen2,
            DeviceModel::Mv6,
            DeviceModel::Mv6Gen2,
            DeviceModel::Mv7,
            DeviceModel::Mv7Plus,
        ] {
            let mut app = App {
                device_model: model,
                focus: Focus::MonitorMix,
                ..App::default()
            };
            app.device_state.monitor_mix = 50;
            let action = app.adjust_focused(1);
            assert_eq!(app.device_state.monitor_mix, 51, "{model:?}");
            assert!(
                matches!(action, Some(DeviceAction::SetMonitorMix(51))),
                "{model:?}: {action:?}"
            );
        }
    }

    #[test]
    fn adjust_eq_band_select_cycles_through_five_bands() {
        let mut app = App {
            active_tab: Tab::Eq,
            focus: Focus::EqBandSelect,
            eq_selected_band: 4,
            ..App::default()
        };

        let action = app.adjust_focused(1);
        assert_eq!(
            app.eq_selected_band, 0,
            "band 4 forward should wrap to band 0"
        );
        assert!(
            action.is_none(),
            "band selection is UI-only, no DeviceAction expected"
        );
    }

    #[test]
    fn adjust_non_adjustable_focus_returns_none() {
        // Controls like Mode, Mute, Phantom, Lock are toggled, not adjusted.
        let mut app = App::default();
        for focus in [
            Focus::Mode,
            Focus::Mute,
            Focus::Phantom,
            Focus::Lock,
            Focus::GainLock,
            Focus::FactoryReset,
        ] {
            app.focus = focus;
            assert!(
                app.adjust_focused(1).is_none(),
                "{focus:?} should return None from adjust_focused"
            );
        }
    }

    // ── toggle_focused ────────────────────────────────────────────────────────

    #[test]
    fn toggle_mode_switches_state_and_moves_focus() {
        let mut app = App {
            focus: Focus::Mode,
            ..App::default()
        };
        app.device_state.mode = InputMode::Auto;

        // Auto → Manual: focus jumps to Gain
        let action = app.toggle_focused();
        assert_eq!(app.device_state.mode, InputMode::Manual);
        assert_eq!(app.focus, Focus::Gain, "Manual mode must focus Gain");
        assert!(matches!(
            action,
            Some(DeviceAction::SetMode(InputMode::Manual))
        ));

        // Re-focus Mode to toggle back
        app.focus = Focus::Mode;
        // Manual → Auto: focus jumps to AutoPosition
        let action = app.toggle_focused();
        assert_eq!(app.device_state.mode, InputMode::Auto);
        assert_eq!(
            app.focus,
            Focus::AutoPosition,
            "Auto mode must focus AutoPosition"
        );
        assert!(matches!(
            action,
            Some(DeviceAction::SetMode(InputMode::Auto))
        ));
    }

    #[test]
    fn toggle_non_toggleable_focus_returns_none() {
        let mut app = App::default();
        for focus in [
            Focus::Gain,
            Focus::MonitorMix,
            Focus::EqBandSelect,
            Focus::FactoryReset,
            // PresetName: editing is handled in handle_key, not toggle_focused
            Focus::PresetName(0),
            // PresetActions with empty slot: no preset to load
            Focus::PresetActions(0),
        ] {
            app.focus = focus;
            assert!(
                app.toggle_focused().is_none(),
                "{focus:?} should return None from toggle_focused"
            );
        }
    }

    #[test]
    fn mv6_dynamics_tab_focus_cycles_all_four_controls() {
        let mut app = App {
            device_model: DeviceModel::Mv6,
            active_tab: Tab::Dynamics,
            focus: Focus::Denoiser,
            ..App::default()
        };

        // Forward: Denoiser → PopperStopper → MuteBtnDisable → Hpf → Denoiser
        app.focus_next();
        assert_eq!(app.focus, Focus::PopperStopper);
        app.focus_next();
        assert_eq!(app.focus, Focus::MuteBtnDisable);
        app.focus_next();
        assert_eq!(app.focus, Focus::Hpf);
        app.focus_next();
        assert_eq!(app.focus, Focus::Denoiser); // wrap

        // Backward: Denoiser → Hpf → MuteBtnDisable → PopperStopper → Denoiser
        app.focus_prev();
        assert_eq!(app.focus, Focus::Hpf);
        app.focus_prev();
        assert_eq!(app.focus, Focus::MuteBtnDisable);
        app.focus_prev();
        assert_eq!(app.focus, Focus::PopperStopper);
        app.focus_prev();
        assert_eq!(app.focus, Focus::Denoiser); // wrap
    }

    /// Gen 2 Manual shows all five controls. The order must match the card order
    /// in `draw_dynamics_tab()`.
    #[test]
    fn gen2_dynamics_tab_manual_focus_cycles_all_five_controls() {
        let mut app = App {
            device_model: DeviceModel::Mvx2uGen2,
            active_tab: Tab::Dynamics,
            focus: Focus::Limiter,
            ..App::default()
        };
        app.device_state.mode = InputMode::Manual;

        // Forward: Limiter → Compressor → Denoiser → PopperStopper → Hpf → wrap
        for expected in [
            Focus::Compressor,
            Focus::Denoiser,
            Focus::PopperStopper,
            Focus::Hpf,
            Focus::Limiter,
        ] {
            app.focus_next();
            assert_eq!(app.focus, expected, "focus_next: expected {expected:?}");
        }

        // Backward over the same cycle
        for expected in [
            Focus::Hpf,
            Focus::PopperStopper,
            Focus::Denoiser,
            Focus::Compressor,
            Focus::Limiter,
        ] {
            app.focus_prev();
            assert_eq!(app.focus, expected, "focus_prev: expected {expected:?}");
        }
    }

    /// Gen 2 Auto hides Limiter and Compressor, so focus must skip them entirely
    /// rather than land on a control the tab does not draw.
    #[test]
    fn gen2_dynamics_tab_auto_focus_skips_limiter_and_compressor() {
        let mut app = App {
            device_model: DeviceModel::Mvx2uGen2,
            active_tab: Tab::Dynamics,
            focus: Focus::Denoiser,
            ..App::default()
        };
        app.device_state.mode = InputMode::Auto;

        for expected in [Focus::PopperStopper, Focus::Hpf, Focus::Denoiser] {
            app.focus_next();
            assert_eq!(app.focus, expected, "focus_next: expected {expected:?}");
            assert_ne!(app.focus, Focus::Limiter);
            assert_ne!(app.focus, Focus::Compressor);
        }

        for expected in [Focus::Hpf, Focus::PopperStopper, Focus::Denoiser] {
            app.focus_prev();
            assert_eq!(app.focus, expected, "focus_prev: expected {expected:?}");
        }
    }

    /// MV7+ shows all six controls. The order must match the card order in
    /// `draw_dynamics_tab()`.
    #[test]
    fn mv7plus_dynamics_tab_focus_cycles_all_six_controls() {
        let mut app = App {
            device_model: DeviceModel::Mv7Plus,
            active_tab: Tab::Dynamics,
            focus: Focus::Limiter,
            ..App::default()
        };

        for expected in [
            Focus::Compressor,
            Focus::Denoiser,
            Focus::PopperStopper,
            Focus::MuteBtnDisable,
            Focus::Hpf,
            Focus::Limiter,
        ] {
            app.focus_next();
            assert_eq!(app.focus, expected, "focus_next: expected {expected:?}");
        }

        for expected in [
            Focus::Hpf,
            Focus::MuteBtnDisable,
            Focus::PopperStopper,
            Focus::Denoiser,
            Focus::Compressor,
            Focus::Limiter,
        ] {
            app.focus_prev();
            assert_eq!(app.focus, expected, "focus_prev: expected {expected:?}");
        }
    }

    // ── MV7 ───────────────────────────────────────────────────────────────────

    fn mv7_app(mode: InputMode) -> App {
        let mut app = App {
            device_model: DeviceModel::Mv7,
            ..App::default()
        };
        app.device_state.mode = mode;
        app
    }

    fn assert_focus_cycle(app: &mut App, cycle: &[Focus]) {
        app.focus = cycle[0];
        for expected in &cycle[1..] {
            app.focus_next();
            assert_eq!(app.focus, *expected, "focus_next");
        }
        for expected in cycle[..cycle.len() - 1].iter().rev() {
            app.focus_prev();
            assert_eq!(app.focus, *expected, "focus_prev");
        }
    }

    #[test]
    fn mv7_main_manual_focus_cycle() {
        let mut app = mv7_app(InputMode::Manual);
        app.active_tab = Tab::Main;
        assert_focus_cycle(
            &mut app,
            &[
                Focus::Mode,
                Focus::Mute,
                Focus::Gain,
                Focus::MonitorMix,
                Focus::Lock,
                Focus::Mode,
            ],
        );
    }

    #[test]
    fn mv7_main_auto_focus_cycle() {
        let mut app = mv7_app(InputMode::Auto);
        app.active_tab = Tab::Main;
        assert_focus_cycle(
            &mut app,
            &[
                Focus::Mode,
                Focus::Mute,
                Focus::AutoPosition,
                Focus::AutoTone,
                Focus::MonitorMix,
                Focus::Lock,
                Focus::Mode,
            ],
        );
    }

    #[test]
    fn mv7_led_focus_cycle() {
        let mut app = mv7_app(InputMode::Manual);
        app.active_tab = Tab::Led;
        assert_focus_cycle(
            &mut app,
            &[
                Focus::LedLiveMeter,
                Focus::LedNightMode,
                Focus::LedLiveMeter,
            ],
        );
    }

    #[test]
    fn mv7_tabs_hide_reverb_and_show_led() {
        let app = mv7_app(InputMode::Manual);
        assert!(app.is_tab_hidden(Tab::Reverb));
        assert!(!app.is_tab_hidden(Tab::Led));
        for tab in [
            Tab::Main,
            Tab::Eq,
            Tab::Dynamics,
            Tab::Led,
            Tab::Presets,
            Tab::Info,
        ] {
            assert!(!app.is_tab_locked(tab), "{tab:?} must be open in Manual");
        }
    }

    #[test]
    fn factory_reset_only_on_captured_models() {
        for (model, supported) in [
            (DeviceModel::Mvx2u, false),
            (DeviceModel::Mvx2uGen2, true),
            (DeviceModel::Mv6, false),
            (DeviceModel::Mv6Gen2, true),
            (DeviceModel::Mv7, false),
            (DeviceModel::Mv7Plus, true),
        ] {
            let app = App {
                device_model: model,
                ..App::default()
            };
            assert_eq!(app.supports_factory_reset(), supported, "{model:?}");
        }
    }

    #[test]
    fn mv7_switching_to_auto_focuses_mic_position() {
        let mut app = mv7_app(InputMode::Manual);
        app.focus = Focus::Mode;
        assert!(matches!(
            app.toggle_focused(),
            Some(DeviceAction::SetMode(InputMode::Auto))
        ));
        assert_eq!(app.focus, Focus::AutoPosition);
    }

    /// Every model focuses Gain on switching to Manual; the MV7+ used to jump to Mute.
    #[test]
    fn mv7plus_mode_toggle_focuses_gain_in_manual_and_mute_in_auto() {
        let mut app = App {
            device_model: DeviceModel::Mv7Plus,
            focus: Focus::Mode,
            ..App::default()
        };
        app.toggle_focused();
        assert_eq!(app.device_state.mode, InputMode::Manual);
        assert_eq!(app.focus, Focus::Gain);

        app.focus = Focus::Mode;
        app.toggle_focused();
        assert_eq!(app.device_state.mode, InputMode::Auto);
        assert_eq!(app.focus, Focus::Mute);
    }

    /// A refresh that finds a Gen 1 in Auto must not leave the EQ tab's
    /// controls live behind its lock notice.
    #[test]
    fn settle_focus_leaves_a_tab_that_became_locked() {
        let mut app = App {
            active_tab: Tab::Eq,
            focus: Focus::EqGain(0),
            device_state: DeviceState {
                mode: InputMode::Auto,
                ..DeviceState::default()
            },
            ..App::default()
        };
        app.settle_focus(InputMode::Manual);
        assert_eq!(app.active_tab, Tab::Main);
        assert_eq!(app.focus, Focus::Mode);
        assert!(app.adjust_focused(1).is_none());
    }

    /// Gain is hidden in Auto, so focus must not stay on it for ←/→ to adjust.
    #[test]
    fn settle_focus_repicks_focus_when_the_mode_changed_under_it() {
        let mut app = App {
            device_model: DeviceModel::Mvx2uGen2,
            focus: Focus::Gain,
            device_state: DeviceState {
                mode: InputMode::Auto,
                ..DeviceState::default()
            },
            ..App::default()
        };
        app.settle_focus(InputMode::Manual);
        assert_eq!(app.active_tab, Tab::Main);
        assert_eq!(app.focus, Focus::Mode);
    }

    #[test]
    fn settle_focus_keeps_focus_when_nothing_changed_or_on_presets() {
        let mut app = App {
            device_model: DeviceModel::Mvx2uGen2,
            focus: Focus::MonitorMix,
            device_state: DeviceState {
                mode: InputMode::Manual,
                ..DeviceState::default()
            },
            ..App::default()
        };
        app.settle_focus(InputMode::Manual);
        assert_eq!(app.focus, Focus::MonitorMix);

        app.active_tab = Tab::Presets;
        app.focus = Focus::PresetActions(2);
        app.device_state.mode = InputMode::Auto;
        app.settle_focus(InputMode::Manual);
        assert_eq!(app.focus, Focus::PresetActions(2));
    }

    #[test]
    fn eq_gain_adjust_lands_back_on_the_gen1_grid() {
        let mut app = App {
            active_tab: Tab::Eq,
            focus: Focus::EqGain(0),
            ..App::default()
        };
        app.device_state.eq_bands[0].gain_db = 15; // 1.5 dB, from a Gen 2 preset
        assert!(matches!(
            app.adjust_focused(1),
            Some(DeviceAction::SetEqBandGain(0, 40))
        ));
        app.device_state.eq_bands[0].gain_db = 15;
        assert!(matches!(
            app.adjust_focused(-1),
            Some(DeviceAction::SetEqBandGain(0, 0))
        ));
    }
}
