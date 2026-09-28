//! Mouse hit testing for the most recently rendered frame.
//!
//! Rendering registers visible regions; input selects a target and translates it
//! into the same key action used by the keyboard. No device writes live here.

use crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::app::{App, Focus, Tab};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    Tab(Tab),
    Control(Focus),
    /// A specific preset action, rather than the whole actions row.
    PresetAction(usize, KeyCode),
}

#[derive(Default)]
pub struct HitMap {
    regions: Vec<(Rect, Target)>,
}

impl HitMap {
    pub fn add(&mut self, area: Rect, target: Target) {
        if !area.is_empty() {
            self.regions.push((area, target));
        }
    }

    pub fn control(&mut self, area: Rect, focus: Focus) {
        self.add(area, Target::Control(focus));
    }

    pub fn target_at(&self, column: u16, row: u16) -> Option<Target> {
        // Later, more specific regions (e.g. an EQ band's enable row) win.
        self.regions
            .iter()
            .rev()
            .find(|(area, _)| area.contains(Position::new(column, row)))
            .map(|(_, target)| *target)
    }
}

/// Returns a key to dispatch through handle_key after updating UI-only state.
/// A click focuses an unselected control without activating it. Clicking an
/// already-focused control activates it, including focus set by the keyboard.
/// Wheel events still focus and adjust immediately.
/// Clicking outside an edited name cancels via Esc, consuming that click.
/// Other modal interactions stay keyboard-only, preventing click-through and
/// an accidental double-click from confirming a factory reset.
pub fn handle_mouse(app: &mut App, event: MouseEvent, hits: &HitMap) -> Option<KeyCode> {
    if app.help_visible || app.confirming_factory_reset {
        return None;
    }
    if app.editing_preset_name {
        let outside_name = hits.target_at(event.column, event.row)
            != Some(Target::Control(Focus::PresetName(app.editing_preset_index)));
        return (event.kind == MouseEventKind::Down(MouseButton::Left) && outside_name)
            .then_some(KeyCode::Esc);
    }
    let key = match event.kind {
        MouseEventKind::Down(MouseButton::Left) => KeyCode::Enter,
        MouseEventKind::ScrollUp => KeyCode::Right,
        MouseEventKind::ScrollDown => KeyCode::Left,
        MouseEventKind::Down(MouseButton::Right | MouseButton::Middle)
        | MouseEventKind::Up(_)
        | MouseEventKind::Drag(_)
        | MouseEventKind::Moved
        | MouseEventKind::ScrollLeft
        | MouseEventKind::ScrollRight => return None,
    };
    match hits.target_at(event.column, event.row)? {
        Target::Tab(tab) => {
            if key == KeyCode::Enter {
                app.select_tab(tab);
            }
            None
        }
        Target::Control(focus) => {
            if app.is_tab_locked(app.active_tab) {
                return None;
            }
            let already_focused = app.focus == focus;
            app.focus = focus;
            if let Focus::EqGain(band) | Focus::EqBandEnable(band) = focus {
                app.eq_selected_band = band;
            }
            (already_focused || key != KeyCode::Enter).then_some(key)
        }
        Target::PresetAction(index, action) => {
            if key != KeyCode::Enter || app.active_tab != Tab::Presets {
                return None;
            }
            let already_focused = app.focus == Focus::PresetActions(index);
            app.focus = Focus::PresetActions(index);
            already_focused.then_some(action)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::DeviceAction;
    use crate::presets::PresetSlot;
    use crate::protocol::{
        DeviceModel, InputMode, LedBehavior, LedLiveTheme, LedPulsingTheme, LedSolidTheme,
    };
    use crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};

    const MODELS: [DeviceModel; 5] = [
        DeviceModel::Mvx2u,
        DeviceModel::Mvx2uGen2,
        DeviceModel::Mv6,
        DeviceModel::Mv7,
        DeviceModel::Mv7Plus,
    ];

    fn render(app: &App, width: u16, height: u16) -> HitMap {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut hits = HitMap::default();
        terminal
            .draw(|frame| hits = crate::ui::draw(frame, app))
            .unwrap();
        hits
    }

    fn event_at(hits: &HitMap, target: Target, kind: MouseEventKind) -> MouseEvent {
        for (area, _) in hits
            .regions
            .iter()
            .filter(|(_, candidate)| *candidate == target)
        {
            for row in area.y..area.bottom() {
                for column in area.x..area.right() {
                    if hits.target_at(column, row) == Some(target) {
                        return MouseEvent {
                            kind,
                            column,
                            row,
                            modifiers: KeyModifiers::NONE,
                        };
                    }
                }
            }
        }
        panic!("missing visible target: {target:?}");
    }

    fn dispatch(
        app: &mut App,
        hits: &HitMap,
        target: Target,
        kind: MouseEventKind,
    ) -> Option<DeviceAction> {
        let event = event_at(hits, target, kind);
        handle_mouse(app, event, hits)
            .and_then(|key| crate::handle_key(app, key, KeyModifiers::NONE))
    }

    fn click(app: &mut App, hits: &HitMap, target: Target) -> Option<DeviceAction> {
        dispatch(app, hits, target, MouseEventKind::Down(MouseButton::Left))
    }

    /// Check actual rendered cells, independently of the registered rectangles.
    fn assert_label_target(app: &App, label: &str, target: Target, tab_row_only: bool) {
        let mut terminal = Terminal::new(TestBackend::new(120, 50)).unwrap();
        let mut hits = HitMap::default();
        terminal
            .draw(|frame| hits = crate::ui::draw(frame, app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows = if tab_row_only { 3..4 } else { 6..50 };
        for row in rows {
            for column in 0..120 - label.len() as u16 {
                if label.chars().enumerate().all(|(offset, ch)| {
                    buffer[(column + offset as u16, row)].symbol() == ch.to_string()
                }) {
                    for offset in 0..label.len() as u16 {
                        assert_eq!(
                            hits.target_at(column + offset, row),
                            Some(target),
                            "{label}"
                        );
                    }
                    return;
                }
            }
        }
        panic!("label not rendered: {label}");
    }

    #[test]
    fn hit_regions_align_with_rendered_labels_including_tabs_after_lock_icons() {
        for model in MODELS {
            let app = App {
                device_model: model,
                ..App::default()
            };
            for tab in Tab::ALL {
                if !app.is_tab_hidden(tab) {
                    assert_label_target(&app, tab.title().trim(), Target::Tab(tab), true);
                }
            }
        }
        let mut app = App {
            active_tab: Tab::Presets,
            ..App::default()
        };
        app.presets[0] = Some(PresetSlot::from_device_state(
            "Mouse test",
            &app.device_state,
        ));
        for (label, key) in [
            ("Load", KeyCode::Enter),
            ("Save", KeyCode::Char('s')),
            ("Delete", KeyCode::Char('d')),
        ] {
            assert_label_target(&app, label, Target::PresetAction(0, key), false);
        }
        app.active_tab = Tab::Eq;
        app.device_state.mode = InputMode::Manual;
        assert_label_target(&app, "On:", Target::Control(Focus::EqBandEnable(0)), false);
        app.active_tab = Tab::Info;
        app.device_model = DeviceModel::Mv7Plus;
        assert_label_target(
            &app,
            "Factory Reset",
            Target::Control(Focus::FactoryReset),
            false,
        );
    }

    #[test]
    fn hit_testing_uses_exclusive_edges_and_specific_regions() {
        let mut hits = HitMap::default();
        hits.control(Rect::new(2, 3, 10, 4), Focus::Gain);
        hits.control(Rect::new(2, 4, 10, 1), Focus::Mute);
        hits.control(Rect::new(2, 3, 0, 4), Focus::Mode);
        assert_eq!(hits.target_at(2, 3), Some(Target::Control(Focus::Gain)));
        assert_eq!(hits.target_at(11, 4), Some(Target::Control(Focus::Mute)));
        assert_eq!(hits.target_at(12, 4), None);
        assert_eq!(hits.target_at(2, 7), None);
        assert_eq!(hits.target_at(1, 3), None);
    }

    #[test]
    fn tab_clicks_respect_hidden_and_locked_tabs_for_every_model() {
        for model in MODELS {
            for mode in [InputMode::Manual, InputMode::Auto] {
                let mut app = App {
                    device_model: model,
                    ..App::default()
                };
                app.device_state.mode = mode;
                let hits = render(&app, 120, 40);
                for tab in Tab::ALL {
                    if app.is_tab_hidden(tab) {
                        assert!(
                            !hits
                                .regions
                                .iter()
                                .any(|(_, target)| *target == Target::Tab(tab))
                        );
                        continue;
                    }
                    let previous = app.active_tab;
                    let locked = app.is_tab_locked(tab);
                    assert!(click(&mut app, &hits, Target::Tab(tab)).is_none());
                    assert_eq!(app.active_tab, if locked { previous } else { tab });
                }
            }
        }
    }

    #[test]
    fn every_keyboard_control_has_a_mouse_target() {
        for model in MODELS {
            for mode in [InputMode::Manual, InputMode::Auto] {
                let mut app = App {
                    device_model: model,
                    ..App::default()
                };
                app.device_state.mode = mode;
                for tab in Tab::ALL {
                    if app.is_tab_locked(tab) || (tab == Tab::Info && !app.supports_factory_reset())
                    {
                        continue;
                    }
                    app.select_tab(tab);
                    let hits = render(&app, 160, 50);
                    let first = app.focus;
                    for step in 0..100 {
                        let target = match app.focus {
                            Focus::PresetActions(index) => {
                                Target::PresetAction(index, KeyCode::Char('s'))
                            }
                            focus => Target::Control(focus),
                        };
                        event_at(&hits, target, MouseEventKind::Moved);
                        app.focus_next();
                        if app.focus == first {
                            break;
                        }
                        assert!(step < 99, "focus didn't wrap: {model:?} {mode:?} {tab:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn first_click_highlights_like_keyboard_navigation_without_changing_mode() {
        for model in MODELS {
            let mut app = App {
                device_model: model,
                ..App::default()
            };
            app.device_state.mode = InputMode::Manual;
            app.select_tab(Tab::Main);
            while app.focus != Focus::Mode {
                app.focus_prev();
            }
            let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
            terminal
                .draw(|frame| {
                    crate::ui::draw(frame, &app);
                })
                .unwrap();
            // Retain a snapshot of the keyboard-focused frame for comparison.
            let keyboard_frame = terminal.backend().buffer().clone();
            app.focus_next();
            let hits = render(&app, 120, 40);
            assert!(click(&mut app, &hits, Target::Control(Focus::Mode)).is_none());
            assert_eq!(app.focus, Focus::Mode);
            assert_eq!(app.device_state.mode, InputMode::Manual);
            terminal
                .draw(|frame| {
                    crate::ui::draw(frame, &app);
                })
                .unwrap();
            assert_eq!(terminal.backend().buffer(), &keyboard_frame);
            assert!(matches!(
                click(&mut app, &hits, Target::Control(Focus::Mode)),
                Some(DeviceAction::SetMode(InputMode::Auto))
            ));
        }
    }

    #[test]
    fn keyboard_focused_control_activates_on_click_and_refocusing_is_safe() {
        let mut app = App::default();
        app.focus_next();
        assert_eq!(app.focus, Focus::Mute);
        let hits = render(&app, 120, 40);
        assert!(matches!(
            click(&mut app, &hits, Target::Control(Focus::Mute)),
            Some(DeviceAction::SetMute(true))
        ));
        app.focus_next();
        assert!(click(&mut app, &hits, Target::Control(Focus::Mute)).is_none());
        assert!(app.device_state.muted);
        assert!(matches!(
            click(&mut app, &hits, Target::Control(Focus::Mute)),
            Some(DeviceAction::SetMute(false))
        ));
    }

    #[test]
    fn clicks_toggle_and_wheel_adjusts_without_changing_keyboard_steps() {
        for model in MODELS {
            let mut app = App {
                device_model: model,
                ..App::default()
            };
            app.device_state.mode = InputMode::Manual;
            app.device_state.gain_tenths = 0;
            let hits = render(&app, 120, 40);
            assert!(click(&mut app, &hits, Target::Control(Focus::Mute)).is_none());
            assert_eq!(app.focus, Focus::Mute);
            assert!(!app.device_state.muted);
            assert!(matches!(
                click(&mut app, &hits, Target::Control(Focus::Mute)),
                Some(DeviceAction::SetMute(true))
            ));
            assert!(click(&mut app, &hits, Target::Control(Focus::Gain)).is_none());
            assert_eq!(app.device_state.gain_tenths, 0);
            assert!(matches!(
                dispatch(
                    &mut app,
                    &hits,
                    Target::Control(Focus::Gain),
                    MouseEventKind::ScrollUp
                ),
                Some(DeviceAction::SetGain(_))
            ));
            assert_eq!(app.device_state.gain_tenths, model.gain_step_tenths());
            dispatch(
                &mut app,
                &hits,
                Target::Control(Focus::Gain),
                MouseEventKind::ScrollDown,
            );
            assert_eq!(app.device_state.gain_tenths, 0);
            app.device_state.mv6_gain_locked = true;
            let action = dispatch(
                &mut app,
                &hits,
                Target::Control(Focus::Gain),
                MouseEventKind::ScrollUp,
            );
            if model.has_gain_lock() {
                assert!(action.is_none());
                assert_eq!(app.device_state.gain_tenths, 0);
            } else {
                assert!(matches!(action, Some(DeviceAction::SetGain(_))));
                assert_eq!(app.device_state.gain_tenths, model.gain_step_tenths());
            }
        }
    }

    #[test]
    fn eq_band_click_updates_selection_and_enable_row_wins() {
        for model in [DeviceModel::Mvx2u, DeviceModel::Mvx2uGen2] {
            let mut app = App {
                device_model: model,
                ..App::default()
            };
            app.device_state.mode = InputMode::Manual;
            app.select_tab(Tab::Eq);
            let hits = render(&app, 120, 40);
            click(&mut app, &hits, Target::Control(Focus::EqGain(3)));
            assert_eq!(app.eq_selected_band, 3);
            assert_eq!(app.focus, Focus::EqGain(3));
            assert!(matches!(
                dispatch(
                    &mut app,
                    &hits,
                    Target::Control(Focus::EqGain(3)),
                    MouseEventKind::ScrollUp
                ),
                Some(DeviceAction::SetEqBandGain(3, _))
            ));
            if model == DeviceModel::Mvx2u {
                let enabled = app.device_state.eq_bands[1].enabled;
                assert!(click(&mut app, &hits, Target::Control(Focus::EqBandEnable(1))).is_none());
                assert_eq!(app.focus, Focus::EqBandEnable(1));
                assert_eq!(app.eq_selected_band, 1);
                assert_eq!(app.device_state.eq_bands[1].enabled, enabled);
                assert!(matches!(
                    click(&mut app, &hits, Target::Control(Focus::EqBandEnable(1))),
                    Some(DeviceAction::SetEqBandEnable(1, _))
                ));
                assert_eq!(app.eq_selected_band, 1);
            }
        }
    }

    #[test]
    fn preset_actions_have_separate_targets_and_rename_blocks_click_through() {
        let mut app = App {
            active_tab: Tab::Presets,
            ..App::default()
        };
        app.presets[0] = Some(PresetSlot::from_device_state("Test", &app.device_state));
        let hits = render(&app, 120, 40);
        assert!(click(&mut app, &hits, Target::PresetAction(0, KeyCode::Enter)).is_none());
        assert_eq!(app.focus, Focus::PresetActions(0));
        assert!(matches!(
            click(&mut app, &hits, Target::PresetAction(0, KeyCode::Enter)),
            Some(DeviceAction::LoadPreset(0))
        ));
        app.focus = Focus::PresetName(0);
        assert!(click(&mut app, &hits, Target::PresetAction(0, KeyCode::Char('s'))).is_none());
        assert_eq!(app.focus, Focus::PresetActions(0));
        assert!(matches!(
            click(&mut app, &hits, Target::PresetAction(0, KeyCode::Char('s'))),
            Some(DeviceAction::SavePreset(0))
        ));
        app.focus = Focus::PresetName(0);
        assert!(click(&mut app, &hits, Target::PresetAction(0, KeyCode::Char('d'))).is_none());
        assert_eq!(app.focus, Focus::PresetActions(0));
        assert!(matches!(
            click(&mut app, &hits, Target::PresetAction(0, KeyCode::Char('d'))),
            Some(DeviceAction::DeletePreset(0))
        ));
        assert!(click(&mut app, &hits, Target::PresetAction(1, KeyCode::Char('s'))).is_none());
        assert_eq!(app.focus, Focus::PresetActions(1));
        assert!(matches!(
            click(&mut app, &hits, Target::PresetAction(1, KeyCode::Char('s'))),
            Some(DeviceAction::SavePreset(1))
        ));
        click(&mut app, &hits, Target::Control(Focus::PresetName(0)));
        assert!(!app.editing_preset_name);
        assert_eq!(app.focus, Focus::PresetName(0));
        click(&mut app, &hits, Target::Control(Focus::PresetName(0)));
        assert!(app.editing_preset_name);
        assert!(click(&mut app, &hits, Target::PresetAction(0, KeyCode::Char('d'))).is_none());
        assert!(!app.editing_preset_name);
        assert!(app.presets[0].is_some());
        assert_eq!(app.focus, Focus::PresetName(0));
    }

    fn editing_preset() -> App {
        let mut app = App {
            active_tab: Tab::Presets,
            focus: Focus::PresetName(0),
            ..App::default()
        };
        app.presets[0] = Some(PresetSlot::from_device_state("Test", &app.device_state));
        app.presets[1] = Some(PresetSlot::from_device_state("Other", &app.device_state));
        for key in [KeyCode::Enter, KeyCode::Backspace, KeyCode::Char('X')] {
            assert!(crate::handle_key(&mut app, key, KeyModifiers::NONE).is_none());
        }
        assert_eq!(app.preset_name_draft, "TesX");
        assert_eq!(app.presets[0].as_ref().unwrap().name, "Test");
        app
    }

    #[test]
    fn outside_click_discards_name_draft_without_activating_any_target() {
        for target in [
            Some(Target::Tab(Tab::Main)),
            Some(Target::Control(Focus::PresetName(1))),
            Some(Target::PresetAction(0, KeyCode::Char('d'))),
            None, // Blank space must cancel too.
        ] {
            let mut app = editing_preset();
            let hits = render(&app, 120, 40);
            let kind = MouseEventKind::Down(MouseButton::Left);
            let event = match target {
                Some(target) => event_at(&hits, target, kind),
                None => MouseEvent {
                    kind,
                    column: 0,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                },
            };
            let key = handle_mouse(&mut app, event, &hits).unwrap();
            assert_eq!(key, KeyCode::Esc);
            assert!(crate::handle_key(&mut app, key, KeyModifiers::NONE).is_none());
            assert!(!app.editing_preset_name);
            assert!(app.preset_name_draft.is_empty());
            assert_eq!(app.presets[0].as_ref().unwrap().name, "Test");
            assert_eq!(app.presets[1].as_ref().unwrap().name, "Other");
            assert_eq!(app.active_tab, Tab::Presets);
            assert_eq!(app.focus, Focus::PresetName(0));
            // A subsequent click can navigate normally.
            click(&mut app, &hits, Target::Tab(Tab::Main));
            assert_eq!(app.active_tab, Tab::Main);
        }
    }

    #[test]
    fn inside_click_and_non_click_events_keep_name_draft_active() {
        let mut app = editing_preset();
        let hits = render(&app, 120, 40);
        assert_label_target(&app, "TesX_", Target::Control(Focus::PresetName(0)), false);
        assert!(click(&mut app, &hits, Target::Control(Focus::PresetName(0))).is_none());
        for kind in [
            MouseEventKind::Moved,
            MouseEventKind::ScrollUp,
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Drag(MouseButton::Left),
        ] {
            assert!(dispatch(&mut app, &hits, Target::Tab(Tab::Main), kind).is_none());
        }
        assert!(app.editing_preset_name);
        assert_eq!(app.preset_name_draft, "TesX");
        assert_eq!(app.presets[0].as_ref().unwrap().name, "Test");
    }

    #[test]
    fn escape_discards_name_draft_and_only_enter_commits() {
        let mut app = editing_preset();
        assert!(crate::handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE).is_none());
        assert!(!app.editing_preset_name);
        assert!(app.preset_name_draft.is_empty());
        assert_eq!(app.presets[0].as_ref().unwrap().name, "Test");
        crate::handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.preset_name_draft, "Test");
        crate::handle_key(&mut app, KeyCode::Char('!'), KeyModifiers::NONE);
        assert!(matches!(
            crate::handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            Some(DeviceAction::PersistPresetName(0))
        ));
        assert!(!app.editing_preset_name);
        assert!(app.preset_name_draft.is_empty());
        assert_eq!(app.presets[0].as_ref().unwrap().name, "Test!");
    }

    #[test]
    fn help_and_reset_confirmation_block_mouse_actions() {
        let mut app = App {
            device_model: DeviceModel::Mv7Plus,
            active_tab: Tab::Info,
            ..App::default()
        };
        let hits = render(&app, 120, 50);
        app.help_visible = true;
        click(&mut app, &hits, Target::Control(Focus::FactoryReset));
        assert!(!app.confirming_factory_reset);
        app.help_visible = false;
        assert!(click(&mut app, &hits, Target::Control(Focus::FactoryReset)).is_none());
        assert_eq!(app.focus, Focus::FactoryReset);
        assert!(!app.confirming_factory_reset);
        assert!(click(&mut app, &hits, Target::Control(Focus::FactoryReset)).is_none());
        assert!(app.confirming_factory_reset);
        assert!(click(&mut app, &hits, Target::Control(Focus::FactoryReset)).is_none());
        click(&mut app, &hits, Target::Tab(Tab::Main));
        assert_eq!(app.active_tab, Tab::Info);
        assert!(matches!(
            crate::handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            Some(DeviceAction::FactoryReset)
        ));
    }

    #[test]
    fn custom_led_colors_have_targets_only_when_rendered() {
        let mut app = App {
            device_model: DeviceModel::Mv7Plus,
            active_tab: Tab::Led,
            ..App::default()
        };
        app.device_state.led_solid_theme = LedSolidTheme::Custom;
        app.device_state.led_pulsing_theme = LedPulsingTheme::Custom;
        app.device_state.led_live_theme = LedLiveTheme::Custom;
        for (behavior, focus) in [
            (LedBehavior::Solid, Focus::LedSolidR),
            (LedBehavior::Pulsing, Focus::LedPulsingB),
            (LedBehavior::Live, Focus::LedLiveInteriorG),
        ] {
            app.device_state.led_behavior = behavior;
            let hits = render(&app, 120, 40);
            assert!(
                dispatch(
                    &mut app,
                    &hits,
                    Target::Control(focus),
                    MouseEventKind::ScrollUp
                )
                .is_some()
            );
        }
    }

    #[test]
    fn resize_rebuilds_regions_and_ignores_non_action_events() {
        let mut app = App::default();
        for (width, height) in [(160, 50), (80, 24), (20, 10), (1, 1)] {
            for tab in Tab::ALL {
                app.active_tab = tab;
                let hits = render(&app, width, height);
                for (area, _) in &hits.regions {
                    assert!(area.right() <= width && area.bottom() <= height);
                }
                assert_eq!(hits.target_at(width, height), None);
            }
        }
        app.active_tab = Tab::Main;
        let hits = render(&app, 120, 40);
        for kind in [
            MouseEventKind::Moved,
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Down(MouseButton::Right),
        ] {
            assert!(dispatch(&mut app, &hits, Target::Control(Focus::Mute), kind).is_none());
        }
        assert!(!app.device_state.muted);
    }
}
