//! Application state and navigation logic.

use std::sync::atomic::AtomicI32;
use std::sync::{Arc, Mutex};

use crate::meter::{METER_SILENT, PeakWindow};
use crate::presets::{PRESET_COUNT, PresetSlot};
use crate::protocol::{
    AutoGain, AutoTone, CompressorPreset, DeviceState, HpfFrequency, InputMode, MicPosition,
};

/// Which top-level tab/panel is active.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tab {
    Main,
    Eq,
    Dynamics,
    Presets,
    Info,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Main, Tab::Eq, Tab::Dynamics, Tab::Presets, Tab::Info];

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Main => " Main ",
            Tab::Eq => " EQ ",
            Tab::Dynamics => " Dynamics ",
            Tab::Presets => " Presets ",
            Tab::Info => " Info ",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            Tab::Main => 0,
            Tab::Eq => 1,
            Tab::Dynamics => 2,
            Tab::Presets => 3,
            Tab::Info => 4,
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
    // Main tab — shared
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
    // EQ tab — focus carries the band index (0–4)
    EqEnable,
    EqBandSelect,
    EqBandEnable(usize),
    EqGain(usize),
    // Dynamics tab
    Limiter,
    Compressor,
    Hpf,
    // Presets tab — usize is slot index 0–3
    PresetName(usize),
    PresetActions(usize),
    // Info — no interactive focus
    None,
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
    pub help_visible: bool,
    /// Loaded preset slots. `None` means the slot file does not exist yet.
    pub presets: [Option<PresetSlot>; PRESET_COUNT],
    /// Set to `true` while the user is typing a new name for a preset slot.
    pub editing_preset_name: bool,
    /// Which slot index is being edited (valid when `editing_preset_name` is true).
    pub editing_preset_index: usize,
    /// Instantaneous peak level shared with the cpal capture thread.
    /// Stores `peak_dbfs * 10` as i32, or `METER_SILENT` when unavailable.
    pub meter_level: Arc<AtomicI32>,
    /// Rolling peak windows shared with the cpal capture thread.
    /// `short` (0.3 s) drives the bar; `long` (3.0 s) drives the peak marker.
    pub peak_window: Arc<Mutex<PeakWindow>>,
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
            help_visible: false,
            presets: [None, None, None, None],
            editing_preset_name: false,
            editing_preset_index: 0,
            meter_level: Arc::new(AtomicI32::new(METER_SILENT)),
            peak_window: Arc::new(Mutex::new(PeakWindow::new())),
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

    // ── Tab navigation ────────────────────────────────────────────────────────

    /// Returns true when a tab should be inaccessible given the current device state.
    /// EQ and Dynamics are managed by the device in Auto Level mode and cannot be
    /// configured independently.
    pub fn is_tab_locked(&self, tab: Tab) -> bool {
        matches!(
            (tab, self.device_state.mode),
            (Tab::Eq | Tab::Dynamics, InputMode::Auto)
        )
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
        self.active_tab = candidate;
        self.reset_focus_for_tab();
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
        self.active_tab = candidate;
        self.reset_focus_for_tab();
    }

    fn reset_focus_for_tab(&mut self) {
        self.focus = match self.active_tab {
            Tab::Main => match self.device_state.mode {
                InputMode::Manual => Focus::Gain,
                InputMode::Auto => Focus::Mode,
            },
            Tab::Eq => Focus::EqEnable,
            Tab::Dynamics => Focus::Limiter,
            Tab::Presets => Focus::PresetName(0),
            Tab::Info => Focus::None,
        };
    }

    // ── Focus cycling within a tab ────────────────────────────────────────────
    //
    // Main tab has two distinct cycles depending on input mode.
    // Order matches top-to-bottom screen layout:
    //
    //   Manual: Mode → Mute → Gain → MonitorMix → Phantom → Lock → (wrap)
    //   Auto:   Mode → Mute → AutoPosition → AutoTone → AutoGain → MonitorMix → Phantom → Lock → (wrap)
    //
    // If focus lands on a mode-specific control while the mode doesn't match
    // (e.g. user toggles mode while focused on Gain), the wildcard arm catches
    // it and returns to the correct starting focus for the current mode.
    pub fn focus_next(&mut self) {
        self.focus = match (&self.active_tab, &self.focus, &self.device_state.mode) {
            // Manual cycle (top → bottom): Mode → Mute → Gain → MonitorMix → Phantom → Lock
            (Tab::Main, Focus::Mode, InputMode::Manual) => Focus::Mute,
            (Tab::Main, Focus::Mute, InputMode::Manual) => Focus::Gain,
            (Tab::Main, Focus::Gain, InputMode::Manual) => Focus::MonitorMix,
            // Auto cycle (top → bottom): Mode → Mute → AutoPosition → AutoTone → AutoGain → MonitorMix → Phantom → Lock
            (Tab::Main, Focus::Mode, InputMode::Auto) => Focus::Mute,
            (Tab::Main, Focus::Mute, InputMode::Auto) => Focus::AutoPosition,
            (Tab::Main, Focus::AutoPosition, InputMode::Auto) => Focus::AutoTone,
            (Tab::Main, Focus::AutoTone, InputMode::Auto) => Focus::AutoGain,
            (Tab::Main, Focus::AutoGain, InputMode::Auto) => Focus::MonitorMix,
            // Shared tail (both modes)
            (Tab::Main, Focus::MonitorMix, _) => Focus::Phantom,
            (Tab::Main, Focus::Phantom, _) => Focus::Lock,
            (Tab::Main, Focus::Lock, _) => Focus::Mode,
            // Fallback — wrong-mode focus or unexpected state
            (Tab::Main, _, InputMode::Manual) => Focus::Mode,
            (Tab::Main, _, InputMode::Auto) => Focus::Mode,

            (Tab::Eq, Focus::EqEnable, _) => Focus::EqBandSelect,
            (Tab::Eq, Focus::EqBandSelect, _) => Focus::EqBandEnable(self.eq_selected_band),
            (Tab::Eq, Focus::EqBandEnable(b), _) => Focus::EqGain(*b),
            (Tab::Eq, Focus::EqGain(_), _) => Focus::EqEnable,
            (Tab::Eq, _, _) => Focus::EqEnable,

            (Tab::Dynamics, Focus::Limiter, _) => Focus::Compressor,
            (Tab::Dynamics, Focus::Compressor, _) => Focus::Hpf,
            (Tab::Dynamics, Focus::Hpf, _) => Focus::Limiter,
            (Tab::Dynamics, _, _) => Focus::Limiter,

            (Tab::Presets, Focus::PresetName(i), _) => Focus::PresetActions(*i),
            (Tab::Presets, Focus::PresetActions(i), _) => Focus::PresetName((i + 1) % PRESET_COUNT),
            (Tab::Presets, _, _) => Focus::PresetName(0),

            _ => self.focus,
        };
    }

    pub fn focus_prev(&mut self) {
        self.focus = match (&self.active_tab, &self.focus, &self.device_state.mode) {
            // Manual cycle reverse
            (Tab::Main, Focus::Mode, InputMode::Manual) => Focus::Lock,
            (Tab::Main, Focus::Mute, InputMode::Manual) => Focus::Mode,
            (Tab::Main, Focus::Gain, InputMode::Manual) => Focus::Mute,
            // Auto cycle reverse
            (Tab::Main, Focus::Mode, InputMode::Auto) => Focus::Lock,
            (Tab::Main, Focus::Mute, InputMode::Auto) => Focus::Mode,
            (Tab::Main, Focus::AutoPosition, InputMode::Auto) => Focus::Mute,
            (Tab::Main, Focus::AutoTone, InputMode::Auto) => Focus::AutoPosition,
            (Tab::Main, Focus::AutoGain, InputMode::Auto) => Focus::AutoTone,
            // Shared tail (both modes)
            (Tab::Main, Focus::MonitorMix, InputMode::Manual) => Focus::Gain,
            (Tab::Main, Focus::MonitorMix, InputMode::Auto) => Focus::AutoGain,
            (Tab::Main, Focus::Phantom, _) => Focus::MonitorMix,
            (Tab::Main, Focus::Lock, _) => Focus::Phantom,
            // Fallback
            (Tab::Main, _, InputMode::Manual) => Focus::Mode,
            (Tab::Main, _, InputMode::Auto) => Focus::Mode,

            (Tab::Eq, Focus::EqEnable, _) => Focus::EqGain(self.eq_selected_band),
            (Tab::Eq, Focus::EqBandSelect, _) => Focus::EqEnable,
            (Tab::Eq, Focus::EqBandEnable(_), _) => Focus::EqBandSelect,
            (Tab::Eq, Focus::EqGain(b), _) => Focus::EqBandEnable(*b),
            (Tab::Eq, _, _) => Focus::EqEnable,

            (Tab::Dynamics, Focus::Limiter, _) => Focus::Hpf,
            (Tab::Dynamics, Focus::Compressor, _) => Focus::Limiter,
            (Tab::Dynamics, Focus::Hpf, _) => Focus::Compressor,
            (Tab::Dynamics, _, _) => Focus::Limiter,

            (Tab::Presets, Focus::PresetName(0), _) => Focus::PresetActions(PRESET_COUNT - 1),
            (Tab::Presets, Focus::PresetName(i), _) => Focus::PresetActions(i - 1),
            (Tab::Presets, Focus::PresetActions(i), _) => Focus::PresetName(*i),
            (Tab::Presets, _, _) => Focus::PresetName(0),

            _ => self.focus,
        };
    }

    // ── Value adjustment helpers ──────────────────────────────────────────────
    /// Increment the focused parameter by `delta` (usually ±1).
    pub fn adjust_focused(&mut self, delta: i32) -> Option<DeviceAction> {
        match self.focus {
            Focus::Gain => {
                let g = &mut self.device_state.gain_db;
                if delta > 0 {
                    *g = (*g + 1).min(60);
                } else {
                    *g = g.saturating_sub(1);
                }
                Some(DeviceAction::SetGain(self.device_state.gain_db))
            }
            Focus::MonitorMix => {
                let m = &mut self.device_state.monitor_mix;
                if delta > 0 {
                    *m = (*m + 5).min(100);
                } else {
                    *m = m.saturating_sub(5);
                }
                Some(DeviceAction::SetMonitorMix(self.device_state.monitor_mix))
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
                let band = &mut self.device_state.eq_bands[b];
                // Hardware supports −8 to +6 in steps of 2.
                let new_gain = ((band.gain_db as i32) + delta * 2).clamp(-8, 6) as i8;
                band.gain_db = new_gain;
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
                // Jump focus to the first relevant control for the new mode so
                // the user doesn't end up on a control that no longer exists.
                self.focus = match self.device_state.mode {
                    InputMode::Auto => Focus::AutoPosition,
                    InputMode::Manual => Focus::Gain,
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
            Focus::PresetActions(i) => {
                // Enter on the actions row loads the preset (if filled).
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

/// Commands to send to the device, produced by App but executed by main.
#[derive(Debug)]
pub enum DeviceAction {
    SetGain(u8),
    /// Carry the full `InputMode` so callers never have to interpret a bare bool.
    SetMode(InputMode),
    SetAutoPosition(MicPosition),
    SetAutoTone(AutoTone),
    SetAutoGain(AutoGain),
    SetMute(bool),
    SetPhantom(bool),
    SetLock(bool),
    SetMonitorMix(u8),
    SetLimiter(bool),
    SetCompressor(CompressorPreset),
    SetHpf(HpfFrequency),
    SetEqEnable(bool),
    /// `usize` is band index 0–4.
    SetEqBandEnable(usize, bool),
    /// `usize` is band index 0–4; `i8` is gain in dB (−8..+6, steps of 2).
    SetEqBandGain(usize, i8),
    /// Save current device state to preset slot `usize`.
    SavePreset(usize),
    /// Load preset slot `usize` and apply all settings to the device.
    LoadPreset(usize),
    /// Delete the preset file for slot `usize`.
    DeletePreset(usize),
    /// Write the (already in-memory-updated) preset name for slot `usize` back to disk.
    PersistPresetName(usize),
    Refresh,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Tab navigation ────────────────────────────────────────────────────────

    #[test]
    fn tab_next_wraps_around() {
        // Info is the last tab; next should wrap back to Main.
        assert_eq!(Tab::Info.next(), Tab::Main);
    }

    #[test]
    fn tab_prev_wraps_around() {
        // Main is the first tab; prev should wrap to Info.
        assert_eq!(Tab::Main.prev(), Tab::Info);
    }

    #[test]
    fn tab_next_full_cycle_returns_to_start() {
        let mut tab = Tab::Main;
        for _ in 0..Tab::ALL.len() {
            tab = tab.next();
        }
        assert_eq!(tab, Tab::Main);
    }

    #[test]
    fn tab_index_is_unique_for_each_variant() {
        let indices: Vec<usize> = Tab::ALL.iter().map(|t| t.index()).collect();
        let unique: std::collections::HashSet<usize> = indices.iter().copied().collect();
        assert_eq!(
            indices.len(),
            unique.len(),
            "every Tab variant must have a distinct index"
        );
    }

    #[test]
    fn tab_all_covers_all_five_variants() {
        // Catches the case where a new Tab variant is added to the enum but
        // forgotten in Tab::ALL, which would cause tab_index() to panic at runtime.
        assert_eq!(Tab::ALL.len(), 5, "Tab::ALL must list all 5 Tab variants");
    }

    // ── reset_focus_for_tab (exercised via next_tab / prev_tab) ──────────────

    #[test]
    fn switching_to_main_tab_in_manual_mode_focuses_gain() {
        let mut app = App::default();
        app.device_state.mode = InputMode::Manual;
        app.active_tab = Tab::Eq; // navigate away first
        app.next_tab(); // → Dynamics
        app.prev_tab(); // → Eq
        app.prev_tab(); // → Main
        assert_eq!(app.focus, Focus::Gain);
    }

    #[test]
    fn switching_to_main_tab_in_auto_mode_focuses_mode() {
        let mut app = App::default();
        app.device_state.mode = InputMode::Auto;
        app.active_tab = Tab::Eq;
        app.prev_tab(); // → Main
        assert_eq!(app.focus, Focus::Mode);
    }

    #[test]
    fn switching_to_non_main_tabs_sets_correct_default_focus() {
        let expected: &[(Tab, Focus)] = &[
            (Tab::Eq, Focus::EqEnable),
            (Tab::Dynamics, Focus::Limiter),
            (Tab::Presets, Focus::PresetName(0)),
            (Tab::Info, Focus::None),
        ];
        for (tab, expected_focus) in expected {
            let mut app = App::default();
            app.device_state.mode = InputMode::Manual;
            app.active_tab = *tab;
            app.next_tab();
            app.prev_tab();
            assert_eq!(
                app.focus, *expected_focus,
                "wrong default focus after switching to {tab:?}"
            );
        }
    }

    // ── is_tab_locked ─────────────────────────────────────────────────────────

    #[test]
    fn is_tab_locked_returns_false_for_all_tabs_in_manual_mode() {
        let mut app = App::default();
        app.device_state.mode = InputMode::Manual;
        for tab in Tab::ALL {
            assert!(
                !app.is_tab_locked(tab),
                "{tab:?} must not be locked in Manual mode"
            );
        }
    }

    #[test]
    fn is_tab_locked_returns_true_for_eq_and_dynamics_in_auto_mode() {
        let mut app = App::default();
        app.device_state.mode = InputMode::Auto;
        assert!(app.is_tab_locked(Tab::Eq), "EQ must be locked in Auto mode");
        assert!(
            app.is_tab_locked(Tab::Dynamics),
            "Dynamics must be locked in Auto mode"
        );
    }

    #[test]
    fn is_tab_locked_returns_false_for_other_tabs_in_auto_mode() {
        let mut app = App::default();
        app.device_state.mode = InputMode::Auto;
        for tab in [Tab::Main, Tab::Presets, Tab::Info] {
            assert!(
                !app.is_tab_locked(tab),
                "{tab:?} must not be locked in Auto mode"
            );
        }
    }

    #[test]
    fn next_tab_skips_eq_and_dynamics_in_auto_mode() {
        let mut app = App::default();
        app.device_state.mode = InputMode::Auto;
        app.active_tab = Tab::Main;

        // Main → next should skip Eq and Dynamics, landing on Presets.
        app.next_tab();
        assert_eq!(
            app.active_tab,
            Tab::Presets,
            "next from Main in Auto mode must skip Eq+Dynamics and land on Presets"
        );
    }

    #[test]
    fn prev_tab_skips_eq_and_dynamics_in_auto_mode() {
        let mut app = App::default();
        app.device_state.mode = InputMode::Auto;
        app.active_tab = Tab::Presets;

        // Presets → prev should skip Dynamics and Eq, landing on Main.
        app.prev_tab();
        assert_eq!(
            app.active_tab,
            Tab::Main,
            "prev from Presets in Auto mode must skip Dynamics+Eq and land on Main"
        );
    }

    #[test]
    fn eq_tab_focus_cycles_enable_band_select_enable_gain() {
        let mut app = App::default();
        app.active_tab = Tab::Eq;
        app.focus = Focus::EqEnable;
        app.eq_selected_band = 0;

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
        let mut app = App::default();
        app.active_tab = Tab::Main;
        app.device_state.mode = InputMode::Manual;
        app.focus = Focus::Mode;

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
        let mut app = App::default();
        app.active_tab = Tab::Main;
        app.device_state.mode = InputMode::Auto;
        app.focus = Focus::Mode;

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
    fn dynamics_tab_focus_cycles_all_three_controls() {
        let mut app = App::default();
        app.active_tab = Tab::Dynamics;
        app.focus = Focus::Limiter;

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
        let mut app = App::default();
        app.active_tab = Tab::Presets;

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
    fn adjust_gain_increments_and_clamps_at_max() {
        let mut app = App::default();
        app.focus = Focus::Gain;
        app.device_state.gain_db = 59;

        app.adjust_focused(1);
        assert_eq!(app.device_state.gain_db, 60);

        app.adjust_focused(1); // already at max
        assert_eq!(app.device_state.gain_db, 60, "gain must not exceed 60");
    }

    #[test]
    fn adjust_gain_decrements_and_clamps_at_zero() {
        let mut app = App::default();
        app.focus = Focus::Gain;
        app.device_state.gain_db = 1;

        app.adjust_focused(-1);
        assert_eq!(app.device_state.gain_db, 0);

        app.adjust_focused(-1); // already at min
        assert_eq!(app.device_state.gain_db, 0, "gain must not underflow");
    }

    #[test]
    fn adjust_monitor_mix_steps_by_five_and_clamps() {
        let mut app = App::default();
        app.focus = Focus::MonitorMix;
        app.device_state.monitor_mix = 95;

        app.adjust_focused(1);
        assert_eq!(app.device_state.monitor_mix, 100);

        app.adjust_focused(1); // already at max
        assert_eq!(
            app.device_state.monitor_mix, 100,
            "monitor mix must not exceed 100"
        );

        app.device_state.monitor_mix = 3;
        app.adjust_focused(-1);
        assert_eq!(
            app.device_state.monitor_mix, 0,
            "monitor mix must not underflow"
        );
    }

    #[test]
    fn adjust_eq_band_select_cycles_through_five_bands() {
        let mut app = App::default();
        app.active_tab = Tab::Eq;
        app.focus = Focus::EqBandSelect;
        app.eq_selected_band = 4;

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
    fn adjust_eq_gain_clamps_at_plus_six_and_minus_eight() {
        let mut app = App::default();
        app.active_tab = Tab::Eq;
        app.focus = Focus::EqGain(0);
        app.device_state.eq_bands[0].gain_db = 6;

        app.adjust_focused(1);
        assert_eq!(
            app.device_state.eq_bands[0].gain_db, 6,
            "EQ gain must not exceed +6"
        );

        app.device_state.eq_bands[0].gain_db = -8;
        app.adjust_focused(-1);
        assert_eq!(
            app.device_state.eq_bands[0].gain_db, -8,
            "EQ gain must not go below -8"
        );
    }

    #[test]
    fn adjust_eq_gain_steps_by_two_and_returns_action() {
        let mut app = App::default();
        app.active_tab = Tab::Eq;
        app.focus = Focus::EqGain(2);
        app.device_state.eq_bands[2].gain_db = 0;

        let action = app.adjust_focused(1);
        assert_eq!(app.device_state.eq_bands[2].gain_db, 2);
        assert!(matches!(action, Some(DeviceAction::SetEqBandGain(2, 2))));
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
            Focus::None,
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
    fn toggle_mute_flips_state_and_returns_action() {
        let mut app = App::default();
        app.focus = Focus::Mute;
        app.device_state.muted = false;

        let action = app.toggle_focused();
        assert!(app.device_state.muted);
        assert!(matches!(action, Some(DeviceAction::SetMute(true))));

        let action = app.toggle_focused();
        assert!(!app.device_state.muted);
        assert!(matches!(action, Some(DeviceAction::SetMute(false))));
    }

    #[test]
    fn toggle_mode_switches_state_and_moves_focus() {
        let mut app = App::default();
        app.focus = Focus::Mode;
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
    fn toggle_auto_position_cycles_near_far_and_returns_action() {
        let mut app = App::default();
        app.focus = Focus::AutoPosition;
        app.device_state.auto_position = MicPosition::Near;

        let action = app.toggle_focused();
        assert_eq!(app.device_state.auto_position, MicPosition::Far);
        assert!(matches!(
            action,
            Some(DeviceAction::SetAutoPosition(MicPosition::Far))
        ));

        let action = app.toggle_focused();
        assert_eq!(app.device_state.auto_position, MicPosition::Near);
        assert!(matches!(
            action,
            Some(DeviceAction::SetAutoPosition(MicPosition::Near))
        ));
    }

    #[test]
    fn toggle_auto_tone_cycles_all_values_and_returns_action() {
        let mut app = App::default();
        app.focus = Focus::AutoTone;
        app.device_state.auto_tone = AutoTone::Dark;

        app.toggle_focused();
        assert_eq!(app.device_state.auto_tone, AutoTone::Natural);
        app.toggle_focused();
        assert_eq!(app.device_state.auto_tone, AutoTone::Bright);
        let action = app.toggle_focused();
        assert_eq!(app.device_state.auto_tone, AutoTone::Dark);
        assert!(matches!(
            action,
            Some(DeviceAction::SetAutoTone(AutoTone::Dark))
        ));
    }

    #[test]
    fn toggle_auto_gain_cycles_all_values_and_returns_action() {
        let mut app = App::default();
        app.focus = Focus::AutoGain;
        app.device_state.auto_gain = AutoGain::Quiet;

        app.toggle_focused();
        assert_eq!(app.device_state.auto_gain, AutoGain::Normal);
        app.toggle_focused();
        assert_eq!(app.device_state.auto_gain, AutoGain::Loud);
        let action = app.toggle_focused();
        assert_eq!(app.device_state.auto_gain, AutoGain::Quiet);
        assert!(matches!(
            action,
            Some(DeviceAction::SetAutoGain(AutoGain::Quiet))
        ));
    }

    #[test]
    fn toggle_compressor_cycles_through_all_presets() {
        let mut app = App::default();
        app.focus = Focus::Compressor;
        app.device_state.compressor = CompressorPreset::Off;

        app.toggle_focused();
        assert_eq!(app.device_state.compressor, CompressorPreset::Light);
        app.toggle_focused();
        assert_eq!(app.device_state.compressor, CompressorPreset::Medium);
        app.toggle_focused();
        assert_eq!(app.device_state.compressor, CompressorPreset::Heavy);
        app.toggle_focused();
        assert_eq!(app.device_state.compressor, CompressorPreset::Off);
    }

    #[test]
    fn toggle_eq_band_enable_flips_state_and_returns_action() {
        let mut app = App::default();
        app.active_tab = Tab::Eq;
        app.focus = Focus::EqBandEnable(1);
        app.device_state.eq_bands[1].enabled = false;

        let action = app.toggle_focused();
        assert!(app.device_state.eq_bands[1].enabled);
        assert!(matches!(
            action,
            Some(DeviceAction::SetEqBandEnable(1, true))
        ));

        let action = app.toggle_focused();
        assert!(!app.device_state.eq_bands[1].enabled);
        assert!(matches!(
            action,
            Some(DeviceAction::SetEqBandEnable(1, false))
        ));
    }

    #[test]
    fn toggle_lock_flips_state_and_returns_action() {
        let mut app = App::default();
        app.focus = Focus::Lock;
        app.device_state.locked = false;

        let action = app.toggle_focused();
        assert!(app.device_state.locked);
        assert!(matches!(action, Some(DeviceAction::SetLock(true))));

        let action = app.toggle_focused();
        assert!(!app.device_state.locked);
        assert!(matches!(action, Some(DeviceAction::SetLock(false))));
    }

    #[test]
    fn toggle_phantom_flips_state_and_returns_action() {
        let mut app = App::default();
        app.focus = Focus::Phantom;
        app.device_state.phantom_power = false;

        let action = app.toggle_focused();
        assert!(app.device_state.phantom_power);
        assert!(matches!(action, Some(DeviceAction::SetPhantom(true))));

        let action = app.toggle_focused();
        assert!(!app.device_state.phantom_power);
        assert!(matches!(action, Some(DeviceAction::SetPhantom(false))));
    }

    #[test]
    fn toggle_eq_enable_flips_state_and_returns_action() {
        let mut app = App::default();
        app.active_tab = Tab::Eq;
        app.focus = Focus::EqEnable;
        app.device_state.eq_enabled = false;

        let action = app.toggle_focused();
        assert!(app.device_state.eq_enabled);
        assert!(matches!(action, Some(DeviceAction::SetEqEnable(true))));

        let action = app.toggle_focused();
        assert!(!app.device_state.eq_enabled);
        assert!(matches!(action, Some(DeviceAction::SetEqEnable(false))));
    }

    #[test]
    fn toggle_limiter_flips_state_and_returns_action() {
        let mut app = App::default();
        app.active_tab = Tab::Dynamics;
        app.focus = Focus::Limiter;
        app.device_state.limiter_enabled = false;

        let action = app.toggle_focused();
        assert!(app.device_state.limiter_enabled);
        assert!(matches!(action, Some(DeviceAction::SetLimiter(true))));

        let action = app.toggle_focused();
        assert!(!app.device_state.limiter_enabled);
        assert!(matches!(action, Some(DeviceAction::SetLimiter(false))));
    }

    #[test]
    fn toggle_non_toggleable_focus_returns_none() {
        let mut app = App::default();
        for focus in [
            Focus::Gain,
            Focus::MonitorMix,
            Focus::EqBandSelect,
            Focus::None,
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

    // ── status messages ───────────────────────────────────────────────────────

    #[test]
    fn set_ok_clears_error_flag() {
        let mut app = App::default();
        app.set_err("something broke");
        assert!(app.status_is_error);

        app.set_ok("all good");
        assert!(!app.status_is_error);
        assert_eq!(app.status_message, "all good");
    }

    #[test]
    fn set_err_sets_error_flag() {
        let mut app = App::default();
        app.set_err("device disconnected");
        assert!(app.status_is_error);
        assert_eq!(app.status_message, "device disconnected");
    }

    // ── peak_window ───────────────────────────────────────────────────────────

    #[test]
    fn app_default_peak_window_is_empty() {
        let app = App::default();
        let pw = app.peak_window.lock().unwrap();
        assert!(
            pw.short.max().is_none(),
            "short window must be empty on a fresh App"
        );
        assert!(
            pw.long.max().is_none(),
            "long window must be empty on a fresh App"
        );
    }

    #[test]
    fn app_peak_window_arc_is_shared() {
        // Confirm the Arc is real: cloning it and writing through the clone
        // must be visible via app.peak_window.
        use std::time::Instant;
        let app = App::default();
        let shared = Arc::clone(&app.peak_window);
        shared.lock().unwrap().push(Instant::now(), -200);
        assert_eq!(
            app.peak_window.lock().unwrap().short.max(),
            Some(-200),
            "write through a cloned Arc must be visible via app.peak_window"
        );
    }

    // ── preset focus / toggle ─────────────────────────────────────────────────

    #[test]
    fn toggle_preset_actions_filled_returns_load_preset() {
        let mut app = App::default();
        app.active_tab = Tab::Presets;
        app.focus = Focus::PresetActions(2);
        // Populate slot 2 with a minimal preset.
        use crate::presets::{
            PresetSlot, SerAutoGain, SerAutoTone, SerCompressorPreset, SerEqBand, SerHpfFrequency,
            SerInputMode, SerMicPosition,
        };
        app.presets[2] = Some(PresetSlot {
            name: String::from("Test"),
            gain_db: 36,
            mode: SerInputMode::Manual,
            auto_position: SerMicPosition::Near,
            auto_tone: SerAutoTone::Natural,
            auto_gain: SerAutoGain::Normal,
            muted: false,
            phantom_power: false,
            monitor_mix: 0,
            limiter_enabled: false,
            compressor: SerCompressorPreset::Off,
            hpf: SerHpfFrequency::Off,
            eq_enabled: false,
            eq_bands: [SerEqBand {
                enabled: false,
                gain_db: 0,
            }; 5],
        });

        let action = app.toggle_focused();
        assert!(
            matches!(action, Some(DeviceAction::LoadPreset(2))),
            "filled PresetActions slot must return LoadPreset"
        );
    }
}
