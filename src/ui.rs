//! TUI rendering using ratatui.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Gauge, Padding, Paragraph, Tabs},
};

use crate::app::{App, Focus, Tab};
use crate::protocol::{
    AutoGain, AutoTone, CompressorPreset, DeviceModel, EQ_BAND_FREQS, HpfFrequency, InputMode,
    MicPosition,
};

// ── Palette ───────────────────────────────────────────────────────────────────
const C_ACCENT: Color = Color::Rgb(255, 95, 0); // Shure orange
const C_BG: Color = Color::Rgb(18, 18, 18);
const C_SURFACE: Color = Color::Rgb(28, 28, 28);
const C_BORDER: Color = Color::Rgb(60, 60, 60);
const C_TEXT: Color = Color::White;
const C_DIM: Color = Color::Rgb(120, 120, 120);
const C_SUCCESS: Color = Color::Rgb(80, 200, 80);
const C_ERROR: Color = Color::Rgb(230, 70, 70);
const C_WARN: Color = Color::Rgb(230, 200, 50);
const C_FOCUS: Color = Color::Rgb(255, 140, 40);
const C_DISABLED: Color = Color::Rgb(70, 70, 70);

fn focused_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(C_TEXT)
    }
}

fn bool_span(val: bool) -> Span<'static> {
    if val {
        Span::styled(
            "● ON ",
            Style::default().fg(C_SUCCESS).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("○ OFF", Style::default().fg(C_DIM))
    }
}

/// Entry point called once per frame from main.
pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();

    // Background
    f.render_widget(Block::default().style(Style::default().bg(C_BG)), size);

    // Outer layout: header / tabs / content / status bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Length(3), // tabs
            Constraint::Min(0),    // content
            Constraint::Length(1), // status
        ])
        .split(size);

    draw_header(f, app, chunks[0]);
    draw_tabs(f, app, chunks[1]);

    match app.active_tab {
        Tab::Main => draw_main_tab(f, app, chunks[2]),
        Tab::Eq => draw_eq_tab(f, app, chunks[2]),
        Tab::Dynamics => draw_dynamics_tab(f, app, chunks[2]),
        Tab::Presets => draw_presets_tab(f, app, chunks[2]),
        Tab::Info => draw_info_tab(f, app, chunks[2]),
    }

    draw_status(f, app, chunks[3]);

    if app.help_visible {
        draw_help_overlay(f, size);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let demo_tag = if app.demo_mode {
        Span::styled(
            " [DEMO — no device] ",
            Style::default().fg(C_WARN).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            " [CONNECTED] ",
            Style::default().fg(C_SUCCESS).add_modifier(Modifier::BOLD),
        )
    };

    let title = vec![
        Span::styled(
            "shurectl",
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        demo_tag,
        Span::styled(
            format!("  S/N: {}", app.device_state.serial_number),
            Style::default().fg(C_DIM),
        ),
    ];

    let p = Paragraph::new(Line::from(title))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(C_ACCENT))
                .style(Style::default().bg(C_BG)),
        )
        .alignment(Alignment::Left);
    f.render_widget(p, area);
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| {
            if app.is_tab_locked(*t) {
                Line::from(Span::styled(
                    format!("{}🔒", t.title()),
                    Style::default().fg(C_DISABLED),
                ))
            } else {
                Line::from(t.title())
            }
        })
        .collect();

    let tabs = Tabs::new(titles)
        .select(app.active_tab.index())
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(C_BORDER)),
        )
        .style(Style::default().fg(C_DIM).bg(C_BG))
        .highlight_style(
            Style::default()
                .fg(C_ACCENT)
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::UNDERLINED),
        )
        .divider(Span::styled("│", Style::default().fg(C_BORDER)));

    f.render_widget(tabs, area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let (msg, color) = if app.status_is_error {
        (format!("✗ {}", app.status_message), C_ERROR)
    } else {
        (format!("  {}", app.status_message), C_DIM)
    };

    let hint = if app.editing_preset_name {
        Span::styled(
            " Editing name — type to change  [Enter/Esc] confirm  [Backspace] delete",
            Style::default().fg(C_ACCENT),
        )
    } else if app.active_tab == Tab::Eq
        && matches!(
            app.device_model,
            crate::protocol::DeviceModel::Mvx2u | crate::protocol::DeviceModel::Mvx2uGen2
        )
        && app.device_state.mode == crate::protocol::InputMode::Manual
    {
        Span::styled(
            " [Tab] Next section  [↑↓] Focus  [←→] Adjust  [f] Flat  [r] Refresh  [?] Help  [q] Quit",
            Style::default().fg(C_DISABLED),
        )
    } else {
        Span::styled(
            " [Tab] Next section  [↑↓] Focus  [←→/Enter] Adjust  [r] Refresh  [?] Help  [q] Quit",
            Style::default().fg(C_DISABLED),
        )
    };

    let status_line = vec![
        Span::styled(msg, Style::default().fg(color)),
        Span::raw("   "),
        hint,
    ];

    f.render_widget(
        Paragraph::new(Line::from(status_line)).style(Style::default().bg(C_SURFACE)),
        area,
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Main Tab
// ─────────────────────────────────────────────────────────────────────────────
fn draw_main_tab(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    draw_main_left(f, app, cols[0]);
    draw_main_right(f, app, cols[1]);
}

fn draw_main_left(f: &mut Frame, app: &App, area: Rect) {
    match (app.device_model, app.device_state.mode) {
        (DeviceModel::Mv6, InputMode::Manual) => draw_main_left_mv6_manual(f, app, area),
        (DeviceModel::Mv6, InputMode::Auto) => draw_main_left_mv6_auto(f, app, area),
        (DeviceModel::Mvx2uGen2, InputMode::Manual) => draw_main_left_gen2_manual(f, app, area),
        (DeviceModel::Mvx2uGen2, InputMode::Auto) => draw_main_left_gen2_auto(f, app, area),
        (DeviceModel::Mvx2u, InputMode::Auto) => draw_main_left_auto(f, app, area),
        (DeviceModel::Mvx2u, InputMode::Manual) => draw_main_left_manual(f, app, area),
    }
}

fn draw_main_left_mv6_manual(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode
            Constraint::Length(3), // mute
            Constraint::Length(3), // gain gauge
            Constraint::Length(3), // gain lock
            Constraint::Length(4), // level meter
            Constraint::Length(3), // monitor mix
            Constraint::Min(0),    // spacer
        ])
        .margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);

    let gain_focused = app.focus == Focus::Gain;
    let gain = app.device_state.gain_db;
    let gain_locked = app.device_state.mv6_gain_locked;
    let gauge = Gauge::default()
        .block(
            Block::default()
                .title(Line::from(vec![
                    Span::styled("  GAIN  ", focused_style(gain_focused)),
                    Span::styled(
                        format!(" {} dB ", gain),
                        Style::default()
                            .fg(if gain_locked { C_DIM } else { C_ACCENT })
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        if gain_focused && !gain_locked {
                            "  ◄ ► or ←→ to adjust"
                        } else if gain_focused && gain_locked {
                            "  🔒 locked"
                        } else {
                            ""
                        },
                        Style::default().fg(C_DIM),
                    ),
                ]))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(if gain_focused {
                    Style::default().fg(C_FOCUS)
                } else {
                    Style::default().fg(C_BORDER)
                }),
        )
        .gauge_style(
            Style::default()
                .fg(if gain_locked { C_DISABLED } else { C_ACCENT })
                .bg(C_SURFACE)
                .add_modifier(if gain_focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        )
        .ratio(gain as f64 / app.device_model.max_gain_db() as f64)
        .label(format!("{gain} / {} dB", app.device_model.max_gain_db()));
    f.render_widget(gauge, rows[2]);

    draw_gain_lock_block(f, app, rows[3]);

    draw_meter(f, app, rows[4]);
    draw_monitor_mix_gauge(f, app, rows[5]);
}

fn draw_main_left_mv6_auto(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode
            Constraint::Length(3), // mute
            Constraint::Length(4), // level meter
            Constraint::Length(3), // monitor mix
            Constraint::Min(0),    // spacer
        ])
        .margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);
    draw_meter(f, app, rows[2]);
    draw_monitor_mix_gauge(f, app, rows[3]);
}

// Gen 2 Manual: Mode → Mute → Gain → GainLock → Meter → MonitorMix → Phantom
fn draw_main_left_gen2_manual(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode
            Constraint::Length(3), // mute
            Constraint::Length(3), // gain gauge
            Constraint::Length(3), // gain lock
            Constraint::Length(4), // level meter
            Constraint::Length(3), // monitor mix
            Constraint::Length(3), // phantom
            Constraint::Min(0),    // spacer
        ])
        .margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);

    let gain_focused = app.focus == Focus::Gain;
    let gain = app.device_state.gain_db;
    let gain_locked = app.device_state.mv6_gain_locked;
    let gauge = Gauge::default()
        .block(
            Block::default()
                .title(Line::from(vec![
                    Span::styled("  GAIN  ", focused_style(gain_focused)),
                    Span::styled(
                        format!(" {} dB ", gain),
                        Style::default()
                            .fg(if gain_locked { C_DIM } else { C_ACCENT })
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        if gain_focused && !gain_locked {
                            "  ◄ ► or ←→ to adjust"
                        } else if gain_focused && gain_locked {
                            "  🔒 locked"
                        } else {
                            ""
                        },
                        Style::default().fg(C_DIM),
                    ),
                ]))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(if gain_focused {
                    Style::default().fg(C_FOCUS)
                } else {
                    Style::default().fg(C_BORDER)
                }),
        )
        .gauge_style(
            Style::default()
                .fg(if gain_locked { C_DISABLED } else { C_ACCENT })
                .bg(C_SURFACE)
                .add_modifier(if gain_focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        )
        .ratio(gain as f64 / app.device_model.max_gain_db() as f64)
        .label(format!("{gain} / {} dB", app.device_model.max_gain_db()));
    f.render_widget(gauge, rows[2]);

    draw_gain_lock_block(f, app, rows[3]);
    draw_meter(f, app, rows[4]);
    draw_monitor_mix_gauge(f, app, rows[5]);
    draw_phantom_block(f, app, rows[6]);
}

// Gen 2 Auto: Mode → Mute → Meter → MonitorMix → Phantom
fn draw_main_left_gen2_auto(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode
            Constraint::Length(3), // mute
            Constraint::Length(4), // level meter
            Constraint::Length(3), // monitor mix
            Constraint::Length(3), // phantom
            Constraint::Min(0),    // spacer
        ])
        .margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);
    draw_meter(f, app, rows[2]);
    draw_monitor_mix_gauge(f, app, rows[3]);
    draw_phantom_block(f, app, rows[4]);
}

fn draw_main_left_manual(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode       (1 line + 2 borders)
            Constraint::Length(3), // mute       (1 line + 2 borders)
            Constraint::Length(3), // gain gauge (1 bar + 2 borders)
            Constraint::Length(4), // level meter (bar + ruler + 2 borders)
            Constraint::Length(3), // monitor mix (1 bar + 2 borders)
            Constraint::Length(3), // phantom     (1 line + 2 borders)
            Constraint::Length(3), // lock        (1 line + 2 borders)
            Constraint::Min(0),    // spacer
        ])
        .margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);

    // ── Gain ──────────────────────────────────────────────────────────────────
    let gain_focused = app.focus == Focus::Gain;
    let gain = app.device_state.gain_db;
    let gauge =
        Gauge::default()
            .block(
                Block::default()
                    .title(Line::from(vec![
                        Span::styled("  GAIN  ", focused_style(gain_focused)),
                        Span::styled(
                            format!(" {} dB ", gain),
                            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            if gain_focused {
                                "  ◄ ► or ←→ to adjust"
                            } else {
                                ""
                            },
                            Style::default().fg(C_DIM),
                        ),
                    ]))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(if gain_focused {
                        Style::default().fg(C_FOCUS)
                    } else {
                        Style::default().fg(C_BORDER)
                    }),
            )
            .gauge_style(Style::default().fg(C_ACCENT).bg(C_SURFACE).add_modifier(
                if gain_focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                },
            ))
            .ratio(gain as f64 / app.device_model.max_gain_db() as f64)
            .label(format!("{gain} / {} dB", app.device_model.max_gain_db()));
    f.render_widget(gauge, rows[2]);

    draw_main_shared(f, app, &rows[3..]);
}

fn draw_main_left_auto(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode          (1 line + 2 borders)
            Constraint::Length(3), // mute          (1 line + 2 borders)
            Constraint::Length(7), // auto controls (3 rows + 2 borders + header)
            Constraint::Length(4), // level meter   (bar + ruler + 2 borders)
            Constraint::Length(3), // monitor mix   (1 bar + 2 borders)
            Constraint::Length(3), // phantom       (1 line + 2 borders)
            Constraint::Length(3), // lock          (1 line + 2 borders)
            Constraint::Min(0),    // spacer
        ])
        .margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);
    draw_auto_controls(f, app, rows[2]);
    draw_main_shared(f, app, &rows[3..]);
}

/// Renders the three Auto Level sub-controls: Position, Tone, Gain.
///
/// Each row shows all options for that setting as a horizontal "segmented
/// button" strip, with the active value highlighted in the accent colour
/// and focused rows highlighted with the focus border/colour.
fn draw_auto_controls(f: &mut Frame, app: &App, area: Rect) {
    let pos_focused = app.focus == Focus::AutoPosition;
    let tone_focused = app.focus == Focus::AutoTone;
    let gain_focused = app.focus == Focus::AutoGain;
    let any_focused = pos_focused || tone_focused || gain_focused;

    let inner_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Mic Position
            Constraint::Length(1), // Tone
            Constraint::Length(1), // Auto Gain
        ])
        .margin(1)
        .split(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(if any_focused {
                    Style::default().fg(C_FOCUS)
                } else {
                    Style::default().fg(C_BORDER)
                })
                .title(Span::styled(
                    "  Auto Level Controls  ",
                    if any_focused {
                        Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(C_TEXT)
                    },
                ))
                .inner(area),
        );

    // ── Mic Position row ──────────────────────────────────────────────────────
    let pos = &app.device_state.auto_position;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Mic Position: ",
                Style::default().fg(if pos_focused { C_FOCUS } else { C_DIM }),
            ),
            segmented_span("Near", pos == &MicPosition::Near, pos_focused),
            Span::raw("  "),
            segmented_span("Far", pos == &MicPosition::Far, pos_focused),
            Span::styled(
                if pos_focused { "  [Enter] cycle" } else { "" },
                Style::default().fg(C_DIM),
            ),
        ])),
        inner_rows[0],
    );

    // ── Tone row ──────────────────────────────────────────────────────────────
    let tone = &app.device_state.auto_tone;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Tone:         ",
                Style::default().fg(if tone_focused { C_FOCUS } else { C_DIM }),
            ),
            segmented_span("Dark", tone == &AutoTone::Dark, tone_focused),
            Span::raw("  "),
            segmented_span("Natural", tone == &AutoTone::Natural, tone_focused),
            Span::raw("  "),
            segmented_span("Bright", tone == &AutoTone::Bright, tone_focused),
            Span::styled(
                if tone_focused { "  [Enter] cycle" } else { "" },
                Style::default().fg(C_DIM),
            ),
        ])),
        inner_rows[1],
    );

    // ── Auto Gain row ─────────────────────────────────────────────────────────
    let gain = &app.device_state.auto_gain;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Gain:         ",
                Style::default().fg(if gain_focused { C_FOCUS } else { C_DIM }),
            ),
            segmented_span("Quiet", gain == &AutoGain::Quiet, gain_focused),
            Span::raw("  "),
            segmented_span("Normal", gain == &AutoGain::Normal, gain_focused),
            Span::raw("  "),
            segmented_span("Loud", gain == &AutoGain::Loud, gain_focused),
            Span::styled(
                if gain_focused { "  [Enter] cycle" } else { "" },
                Style::default().fg(C_DIM),
            ),
        ])),
        inner_rows[2],
    );
}

/// Render a segmented-button option: active value is bold+accent,
/// inactive values are dimmed. Focused row uses the focus colour.
fn segmented_span(label: &'static str, active: bool, focused: bool) -> Span<'static> {
    if active {
        let color = if focused { C_FOCUS } else { C_SUCCESS };
        Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(label, Style::default().fg(C_DISABLED))
    }
}

/// Renders the Input Mode block (Manual / Auto toggle only).
fn draw_mode_block(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Mode;

    let mode_span = match app.device_state.mode {
        InputMode::Auto => Span::styled(
            "AUTO LEVEL",
            Style::default().fg(C_SUCCESS).add_modifier(Modifier::BOLD),
        ),
        InputMode::Manual => Span::styled(
            "MANUAL",
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        ),
    };

    let p = Paragraph::new(Line::from(vec![
        Span::styled("Mode:  ", Style::default().fg(C_DIM)),
        mode_span,
        Span::styled(
            if focused { "  [Enter] toggle" } else { "" },
            Style::default().fg(C_DIM),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if focused {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            })
            .title(Span::styled(
                "  Input Mode  ",
                if focused {
                    Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_TEXT)
                },
            )),
    );
    f.render_widget(p, area);
}

/// Renders the Mute block as a standalone control. Mute is independent of
/// input mode — it silences output regardless of Manual or Auto state.
fn draw_mute_block(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Mute;
    let muted = app.device_state.muted;

    let p = Paragraph::new(Line::from(vec![
        Span::styled("Mute:  ", Style::default().fg(C_DIM)),
        bool_span(muted),
        Span::styled(
            if focused { "  [Enter] toggle" } else { "" },
            Style::default().fg(C_DIM),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if focused {
                Style::default().fg(C_FOCUS)
            } else if muted {
                Style::default().fg(C_ERROR)
            } else {
                Style::default().fg(C_BORDER)
            })
            .title(Span::styled(
                "  Mute  ",
                if focused {
                    Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                } else if muted {
                    Style::default().fg(C_ERROR).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_TEXT)
                },
            )),
    );
    f.render_widget(p, area);
}

/// Renders the monitor mix gauge. Used by both MVX2U (via draw_main_shared)
/// and MV6 (directly in draw_main_left_mv6_manual / draw_main_left_mv6_auto).
fn draw_monitor_mix_gauge(f: &mut Frame, app: &App, area: Rect) {
    let mm_focused = app.focus == Focus::MonitorMix;
    let mix = app.device_state.monitor_mix;
    let mix_gauge = Gauge::default()
        .block(
            Block::default()
                .title(Span::styled(
                    "  Monitor Mix  ",
                    if mm_focused {
                        Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(C_TEXT)
                    },
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(if mm_focused {
                    Style::default().fg(C_FOCUS)
                } else {
                    Style::default().fg(C_BORDER)
                }),
        )
        .gauge_style(Style::default().fg(Color::Rgb(50, 150, 220)).bg(C_SURFACE))
        .ratio(mix as f64 / 100.0)
        .label(format!("Mic ◄─{:3}%─► Playback", mix));
    f.render_widget(mix_gauge, area);
}

/// Renders the Phantom Power block as a standalone control.
/// Used by Gen 2 main tab and MVX2U via draw_main_shared.
fn draw_phantom_block(f: &mut Frame, app: &App, area: Rect) {
    let ph_focused = app.focus == Focus::Phantom;
    let phantom_p = Paragraph::new(Line::from(vec![
        Span::styled("48V Phantom Power:  ", Style::default().fg(C_DIM)),
        bool_span(app.device_state.phantom_power),
        Span::styled(
            if ph_focused {
                "  ⚠ Off before ribbon mics!"
            } else {
                ""
            },
            Style::default().fg(C_WARN),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if ph_focused {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            })
            .title(Span::styled(
                "  Phantom Power  ",
                if ph_focused {
                    Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_TEXT)
                },
            )),
    );
    f.render_widget(phantom_p, area);
}

/// Renders the Gain Lock block. Used by MV6 and Gen 2.
fn draw_gain_lock_block(f: &mut Frame, app: &App, area: Rect) {
    let lock_focused = app.focus == Focus::GainLock;
    let gain_locked = app.device_state.mv6_gain_locked;
    let lock_icon = if gain_locked { "🔒" } else { "🔓" };
    let gain_lock_p = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("{lock_icon}  Gain Lock:  "),
            Style::default().fg(C_DIM),
        ),
        if gain_locked {
            Span::styled(
                "LOCKED",
                Style::default().fg(C_ERROR).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("Unlocked", Style::default().fg(C_SUCCESS))
        },
        Span::styled(
            if lock_focused { "  [Enter] toggle" } else { "" },
            Style::default().fg(C_DIM),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if lock_focused {
                Style::default().fg(C_FOCUS)
            } else if gain_locked {
                Style::default().fg(C_ERROR)
            } else {
                Style::default().fg(C_BORDER)
            })
            .title(Span::styled(
                "  Gain Lock  ",
                if lock_focused {
                    Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                } else if gain_locked {
                    Style::default().fg(C_ERROR).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_TEXT)
                },
            )),
    );
    f.render_widget(gain_lock_p, area);
}

/// Renders the controls shared by both Manual and Auto layouts that follow
/// the mode-specific top section: Level Meter, Monitor Mix, Phantom Power,
/// Config Lock.
///
/// Control order follows the signal chain: observe the level produced by the
/// gain you just set, adjust monitoring, then the rarely-changed setup
/// controls (phantom, lock) at the bottom.
///
/// `rows` must have at least 4 elements (indices 0–3).
fn draw_main_shared(f: &mut Frame, app: &App, rows: &[Rect]) {
    assert!(
        rows.len() >= 4,
        "draw_main_shared requires at least 4 row slots, got {}",
        rows.len()
    );
    // ── Level Meter ───────────────────────────────────────────────────────────
    draw_meter(f, app, rows[0]);

    // ── Monitor Mix ───────────────────────────────────────────────────────────
    draw_monitor_mix_gauge(f, app, rows[1]);

    // ── Phantom Power ─────────────────────────────────────────────────────────
    draw_phantom_block(f, app, rows[2]);

    // ── Config Lock ───────────────────────────────────────────────────────────
    let lock_focused = app.focus == Focus::Lock;
    let locked = app.device_state.locked;
    let lock_icon = if locked { "🔒" } else { "🔓" };
    let lock_p = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("{lock_icon}  Config Lock:  "),
            Style::default().fg(C_DIM),
        ),
        if locked {
            Span::styled(
                "LOCKED",
                Style::default().fg(C_ERROR).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("Unlocked", Style::default().fg(C_SUCCESS))
        },
        Span::styled(
            if lock_focused { "  [Enter] toggle" } else { "" },
            Style::default().fg(C_DIM),
        ),
        if locked && !lock_focused {
            Span::styled(
                "  ⚠ Device ignores changes while locked",
                Style::default().fg(C_WARN),
            )
        } else {
            Span::raw("")
        },
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if lock_focused {
                Style::default().fg(C_FOCUS)
            } else if locked {
                Style::default().fg(C_ERROR)
            } else {
                Style::default().fg(C_BORDER)
            })
            .title(Span::styled(
                "  Config Lock  ",
                if lock_focused {
                    Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                } else if locked {
                    Style::default().fg(C_ERROR).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_TEXT)
                },
            )),
    );
    f.render_widget(lock_p, rows[3]);
}

fn draw_main_right(f: &mut Frame, app: &App, area: Rect) {
    let ds = &app.device_state;

    let lines = match app.device_model {
        DeviceModel::Mvx2u => {
            let mode_specific_lines: Vec<Line> = match ds.mode {
                InputMode::Manual => vec![Line::from(vec![
                    Span::styled("Gain        : ", Style::default().fg(C_DIM)),
                    Span::styled(format!("{} dB", ds.gain_db), Style::default().fg(C_TEXT)),
                ])],
                InputMode::Auto => vec![
                    Line::from(vec![
                        Span::styled("Position    : ", Style::default().fg(C_DIM)),
                        Span::styled(ds.auto_position.to_string(), Style::default().fg(C_TEXT)),
                    ]),
                    Line::from(vec![
                        Span::styled("Tone        : ", Style::default().fg(C_DIM)),
                        Span::styled(ds.auto_tone.to_string(), Style::default().fg(C_TEXT)),
                    ]),
                    Line::from(vec![
                        Span::styled("Auto Gain   : ", Style::default().fg(C_DIM)),
                        Span::styled(ds.auto_gain.to_string(), Style::default().fg(C_TEXT)),
                    ]),
                ],
            };

            let mut l = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("Mode        : ", Style::default().fg(C_DIM)),
                    Span::styled(ds.mode.to_string(), Style::default().fg(C_ACCENT)),
                ]),
            ];
            l.extend(mode_specific_lines);
            l.extend([
                Line::from(vec![
                    Span::styled("Muted       : ", Style::default().fg(C_DIM)),
                    if ds.muted {
                        Span::styled("YES", Style::default().fg(C_ERROR))
                    } else {
                        Span::styled("NO", Style::default().fg(C_SUCCESS))
                    },
                ]),
                Line::from(vec![
                    Span::styled("Locked      : ", Style::default().fg(C_DIM)),
                    if ds.locked {
                        Span::styled("YES", Style::default().fg(C_ERROR))
                    } else {
                        Span::styled("NO", Style::default().fg(C_DIM))
                    },
                ]),
                Line::from(vec![
                    Span::styled("Phantom Pwr : ", Style::default().fg(C_DIM)),
                    if ds.phantom_power {
                        Span::styled("48V ON", Style::default().fg(C_SUCCESS))
                    } else {
                        Span::styled("OFF", Style::default().fg(C_DIM))
                    },
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("EQ          : ", Style::default().fg(C_DIM)),
                    if ds.mode == InputMode::Auto {
                        Span::styled("auto", Style::default().fg(C_DISABLED))
                    } else if ds.eq_enabled {
                        Span::styled("Enabled", Style::default().fg(C_SUCCESS))
                    } else {
                        Span::styled("Bypass", Style::default().fg(C_DIM))
                    },
                ]),
                Line::from(vec![
                    Span::styled("HPF         : ", Style::default().fg(C_DIM)),
                    if ds.mode == InputMode::Auto {
                        Span::styled("auto", Style::default().fg(C_DISABLED))
                    } else {
                        Span::styled(ds.hpf.to_string(), Style::default().fg(C_TEXT))
                    },
                ]),
                Line::from(vec![
                    Span::styled("Limiter     : ", Style::default().fg(C_DIM)),
                    if ds.mode == InputMode::Auto {
                        Span::styled("auto", Style::default().fg(C_DISABLED))
                    } else if ds.limiter_enabled {
                        Span::styled("ON", Style::default().fg(C_SUCCESS))
                    } else {
                        Span::styled("OFF", Style::default().fg(C_DIM))
                    },
                ]),
                Line::from(vec![
                    Span::styled("Compressor  : ", Style::default().fg(C_DIM)),
                    if ds.mode == InputMode::Auto {
                        Span::styled("auto", Style::default().fg(C_DISABLED))
                    } else {
                        Span::styled(ds.compressor.to_string(), Style::default().fg(C_TEXT))
                    },
                ]),
            ]);
            l
        }
        DeviceModel::Mv6 => {
            let pct = ds.tone as i32 * 10;
            let tone_str = if pct < 0 {
                format!("{}% Dark", pct.abs())
            } else if pct > 0 {
                format!("{}% Bright", pct)
            } else {
                "Natural".to_string()
            };
            let mut l = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("Mode        : ", Style::default().fg(C_DIM)),
                    Span::styled(ds.mode.to_string(), Style::default().fg(C_ACCENT)),
                ]),
                Line::from(vec![
                    Span::styled("Tone        : ", Style::default().fg(C_DIM)),
                    Span::styled(tone_str, Style::default().fg(C_TEXT)),
                ]),
            ];
            if ds.mode == InputMode::Manual {
                l.push(Line::from(vec![
                    Span::styled("Gain        : ", Style::default().fg(C_DIM)),
                    Span::styled(format!("{} dB", ds.gain_db), Style::default().fg(C_TEXT)),
                ]));
                l.push(Line::from(vec![
                    Span::styled("Gain Lock   : ", Style::default().fg(C_DIM)),
                    if ds.mv6_gain_locked {
                        Span::styled("LOCKED", Style::default().fg(C_ERROR))
                    } else {
                        Span::styled("Unlocked", Style::default().fg(C_SUCCESS))
                    },
                ]));
            }
            l.extend([
                Line::from(vec![
                    Span::styled("Muted       : ", Style::default().fg(C_DIM)),
                    if ds.muted {
                        Span::styled("YES", Style::default().fg(C_ERROR))
                    } else {
                        Span::styled("NO", Style::default().fg(C_SUCCESS))
                    },
                ]),
                Line::from(vec![
                    Span::styled("Mute Btn    : ", Style::default().fg(C_DIM)),
                    if ds.mute_btn_disabled {
                        Span::styled("Disabled", Style::default().fg(C_WARN))
                    } else {
                        Span::styled("Enabled", Style::default().fg(C_TEXT))
                    },
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Denoiser    : ", Style::default().fg(C_DIM)),
                    if ds.denoiser_enabled {
                        Span::styled("ON", Style::default().fg(C_SUCCESS))
                    } else {
                        Span::styled("OFF", Style::default().fg(C_DIM))
                    },
                ]),
                Line::from(vec![
                    Span::styled("Pop. Stopper: ", Style::default().fg(C_DIM)),
                    if ds.popper_stopper_enabled {
                        Span::styled("ON", Style::default().fg(C_SUCCESS))
                    } else {
                        Span::styled("OFF", Style::default().fg(C_DIM))
                    },
                ]),
                Line::from(vec![
                    Span::styled("HPF         : ", Style::default().fg(C_DIM)),
                    Span::styled(ds.hpf.to_string(), Style::default().fg(C_TEXT)),
                ]),
            ]);
            l
        }
        DeviceModel::Mvx2uGen2 => {
            let mut l = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("Mode        : ", Style::default().fg(C_DIM)),
                    Span::styled(ds.mode.to_string(), Style::default().fg(C_ACCENT)),
                ]),
            ];
            if ds.mode == InputMode::Manual {
                l.push(Line::from(vec![
                    Span::styled("Gain        : ", Style::default().fg(C_DIM)),
                    Span::styled(format!("{} dB", ds.gain_db), Style::default().fg(C_TEXT)),
                ]));
                l.push(Line::from(vec![
                    Span::styled("Gain Lock   : ", Style::default().fg(C_DIM)),
                    if ds.mv6_gain_locked {
                        Span::styled("LOCKED", Style::default().fg(C_ERROR))
                    } else {
                        Span::styled("Unlocked", Style::default().fg(C_SUCCESS))
                    },
                ]));
            } else {
                // Auto mode: show tone slider value
                let pct = ds.tone as i32 * 10;
                let tone_str = if pct < 0 {
                    format!("{}% Dark", pct.abs())
                } else if pct > 0 {
                    format!("{}% Bright", pct)
                } else {
                    "Natural".to_string()
                };
                l.push(Line::from(vec![
                    Span::styled("Tone        : ", Style::default().fg(C_DIM)),
                    Span::styled(tone_str, Style::default().fg(C_TEXT)),
                ]));
            }
            l.extend([
                Line::from(vec![
                    Span::styled("Muted       : ", Style::default().fg(C_DIM)),
                    if ds.muted {
                        Span::styled("YES", Style::default().fg(C_ERROR))
                    } else {
                        Span::styled("NO", Style::default().fg(C_SUCCESS))
                    },
                ]),
                Line::from(vec![
                    Span::styled("Phantom     : ", Style::default().fg(C_DIM)),
                    if ds.phantom_power {
                        Span::styled("48V ON", Style::default().fg(C_SUCCESS))
                    } else {
                        Span::styled("OFF", Style::default().fg(C_DIM))
                    },
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Denoiser    : ", Style::default().fg(C_DIM)),
                    if ds.denoiser_enabled {
                        Span::styled("ON", Style::default().fg(C_SUCCESS))
                    } else {
                        Span::styled("OFF", Style::default().fg(C_DIM))
                    },
                ]),
                Line::from(vec![
                    Span::styled("Pop. Stopper: ", Style::default().fg(C_DIM)),
                    if ds.popper_stopper_enabled {
                        Span::styled("ON", Style::default().fg(C_SUCCESS))
                    } else {
                        Span::styled("OFF", Style::default().fg(C_DIM))
                    },
                ]),
                Line::from(vec![
                    Span::styled("HPF         : ", Style::default().fg(C_DIM)),
                    Span::styled(ds.hpf.to_string(), Style::default().fg(C_TEXT)),
                ]),
                Line::from(vec![
                    Span::styled("Limiter     : ", Style::default().fg(C_DIM)),
                    if ds.mode == InputMode::Auto {
                        Span::styled("auto", Style::default().fg(C_DISABLED))
                    } else if ds.limiter_enabled {
                        Span::styled("ON", Style::default().fg(C_SUCCESS))
                    } else {
                        Span::styled("OFF", Style::default().fg(C_DIM))
                    },
                ]),
            ]);
            l
        }
    };

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .title(Span::styled("  Summary  ", Style::default().fg(C_TEXT)))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(C_BORDER))
                .padding(Padding::horizontal(1)),
        )
        .style(Style::default().bg(C_BG));
    f.render_widget(p, area);
}

// ─────────────────────────────────────────────────────────────────────────────
// dBFS Level Meter
// ─────────────────────────────────────────────────────────────────────────────

/// Tick labels placed on the dB ruler, from quietest to loudest.
/// Each entry is `(db_value, label_str)`. We only include marks that fit
/// cleanly without overlapping — spacing is validated at render time.
const RULER_MARKS: &[(i32, &str)] = &[
    (-60, "-60"),
    (-48, "-48"),
    (-36, "-36"),
    (-24, "-24"),
    (-12, "-12"),
    (0, "0"),
];

/// Return the colour for a dBFS reading using standard metering zones:
/// green < -12, amber -12 to -3, red ≥ -3.
fn meter_color(dbfs: f32) -> Color {
    if dbfs >= -3.0 {
        C_ERROR
    } else if dbfs >= -12.0 {
        C_WARN
    } else {
        C_SUCCESS
    }
}

/// Build the dB ruler string for `inner_width` characters (bar area without borders).
///
/// Each label in `RULER_MARKS` is positioned proportionally between
/// `METER_FLOOR_DB` (left edge) and 0 dBFS (right edge). Labels that would
/// overflow or overlap a previously placed label are skipped silently.
fn build_ruler(inner_width: usize) -> String {
    use crate::meter::METER_FLOOR_DB;
    let floor = METER_FLOOR_DB as i32; // -60

    let mut chars: Vec<char> = vec![' '; inner_width];

    for &(db, label) in RULER_MARKS {
        // Map db → column: 0 % at left (-60 dB), 100 % at right (0 dB).
        let ratio = (db - floor) as f32 / (0 - floor) as f32;
        // Centre the label on its column position.
        let center = (ratio * inner_width as f32).round() as isize;
        let half = label.len() as isize / 2;
        let col_start = (center - half).max(0) as usize;
        let col_end = col_start + label.len();

        if col_end > inner_width {
            continue; // would overflow — skip
        }
        // Skip if any target cell is already occupied.
        if chars[col_start..col_end].iter().any(|&c| c != ' ') {
            continue;
        }
        for (i, ch) in label.chars().enumerate() {
            chars[col_start + i] = ch;
        }
    }

    chars.into_iter().collect()
}

fn draw_meter(f: &mut Frame, app: &App, area: Rect) {
    use crate::meter::{METER_FLOOR_DB, METER_SILENT};

    // ── Read from rolling windows ─────────────────────────────────────────────
    // Both values come from the shared PeakWindow; fall back to METER_SILENT
    // when the lock is unavailable (should never happen in practice) or the
    // window is empty (device just connected, no callbacks yet).
    let (short_raw, long_raw) = if app.demo_mode {
        (METER_SILENT, METER_SILENT)
    } else {
        match app.peak_window.try_lock() {
            Ok(pw) => (
                pw.short.max().unwrap_or(METER_SILENT),
                pw.long.max().unwrap_or(METER_SILENT),
            ),
            Err(_) => (METER_SILENT, METER_SILENT),
        }
    };

    // ── Silent / no-device state ──────────────────────────────────────────────
    if short_raw == METER_SILENT {
        let label = if app.demo_mode { "no device" } else { "---" };
        let empty = Gauge::default()
            .block(
                Block::default()
                    .title(Span::styled("  Level  ", Style::default().fg(C_TEXT)))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(C_BORDER)),
            )
            .gauge_style(Style::default().fg(C_DISABLED).bg(C_SURFACE))
            .ratio(0.0)
            .label(label);
        f.render_widget(empty, area);
        return;
    }

    let short_db = short_raw as f32 / 10.0;
    let long_db = long_raw as f32 / 10.0;
    let floor = METER_FLOOR_DB;

    let bar_ratio = ((short_db - floor) / (0.0 - floor)).clamp(0.0, 1.0) as f64;

    // ── Layout: bar + peak box on top row, ruler below ────────────────────────
    //
    //  ┌─ Level ──────────────────────────────┐ ┌─────────┐
    //  │ ████████████████░░░░  -12.3 dBFS     │ │pk -8.1  │
    //  │ -60      -36      -12    0            │ └─────────┘
    //  └──────────────────────────────────────┘
    //
    // Vertical split: top row = gauge + peak box, bottom row = ruler.
    let rows = ratatui::layout::Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // bar + peak box row (with borders)
            Constraint::Length(1), // ruler row (no border, sits flush below)
        ])
        .split(area);

    // Horizontal split of the top row: bar on left, peak readout on right.
    let cols = ratatui::layout::Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(11)])
        .split(rows[0]);

    // ── Bar gauge ─────────────────────────────────────────────────────────────
    let bar_color = meter_color(short_db);
    let gauge = Gauge::default()
        .block(
            Block::default()
                .title(Span::styled("  Level  ", Style::default().fg(C_TEXT)))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(C_BORDER)),
        )
        .gauge_style(Style::default().fg(bar_color).bg(C_SURFACE))
        .ratio(bar_ratio)
        .label(format!("{short_db:+.1} dBFS"));
    f.render_widget(gauge, cols[0]);

    // ── Peak-hold numeric readout (long window) ───────────────────────────────
    let (hold_text, hold_color) = if long_raw == METER_SILENT {
        ("  ---   ".to_string(), C_DIM)
    } else {
        (format!("{long_db:+.1}"), meter_color(long_db))
    };
    let readout = Paragraph::new(Line::from(vec![
        Span::styled("pk ", Style::default().fg(C_DIM)),
        Span::styled(
            hold_text,
            Style::default().fg(hold_color).add_modifier(Modifier::BOLD),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(C_BORDER)),
    );
    f.render_widget(readout, cols[1]);

    // ── dB ruler ─────────────────────────────────────────────────────────────
    // The ruler sits in rows[1] flush below the bar border. Its inner width
    // must match the bar's inner width: cols[0].width minus 2 border chars,
    // then indented by 1 to align with the bar's left border.
    let bar_inner_width = cols[0].width.saturating_sub(2) as usize;
    let ruler_str = build_ruler(bar_inner_width);

    let ruler = Paragraph::new(Line::from(Span::styled(
        ruler_str,
        Style::default().fg(C_DISABLED),
    )))
    // 1-char left margin aligns the ruler under the bar (skips the left border char).
    .block(Block::default().padding(ratatui::widgets::Padding::new(1, 0, 0, 0)));
    f.render_widget(ruler, rows[1]);
}

// ─────────────────────────────────────────────────────────────────────────────
// MV6 EQ Tab — Tone control
// ─────────────────────────────────────────────────────────────────────────────
fn draw_mv6_eq_tab(f: &mut Frame, app: &App, area: Rect) {
    let ds = &app.device_state;
    let tone_foc = app.focus == Focus::Tone;
    let tone = ds.tone;
    let pct = tone as i32 * 10;
    let tone_label = if pct < 0 {
        format!("{}% Dark", pct.abs())
    } else if pct > 0 {
        format!("{}% Bright", pct)
    } else {
        "Natural".to_string()
    };
    let tone_ratio = (tone as f64 + 10.0) / 20.0;

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .margin(1)
        .split(area);

    let tone_gauge = Gauge::default()
        .block(
            Block::default()
                .title(Line::from(vec![
                    Span::styled("  TONE  ", focused_style(tone_foc)),
                    Span::styled(
                        format!(" {} ", tone_label),
                        Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        if tone_foc { "  ◄ ► to adjust" } else { "" },
                        Style::default().fg(C_DIM),
                    ),
                ]))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(if tone_foc {
                    Style::default().fg(C_FOCUS)
                } else {
                    Style::default().fg(C_BORDER)
                }),
        )
        .gauge_style(
            Style::default()
                .fg(Color::Rgb(50, 150, 220))
                .bg(C_SURFACE)
                .add_modifier(if tone_foc {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        )
        .ratio(tone_ratio)
        .label(format!("Dark ◄─{:+}─► Bright", pct));
    f.render_widget(tone_gauge, rows[0]);
}

// ─────────────────────────────────────────────────────────────────────────────
// MV6 Dynamics Tab — Denoiser, Popper Stopper, Mute Button, HPF
// ─────────────────────────────────────────────────────────────────────────────
fn draw_mv6_dynamics_tab(f: &mut Frame, app: &App, area: Rect) {
    let ds = &app.device_state;

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .margin(1)
        .split(area);

    // ── Denoiser ──────────────────────────────────────────────────────────────
    let den_foc = app.focus == Focus::Denoiser;
    let den_p = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("Status: ", Style::default().fg(C_DIM)),
            bool_span(ds.denoiser_enabled),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Reduces background",
            Style::default().fg(C_DIM),
        )),
        Line::from(Span::styled(
            "noise in real time.",
            Style::default().fg(C_DIM),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "[Enter] to toggle",
            Style::default().fg(if den_foc { C_FOCUS } else { C_DISABLED }),
        )),
    ])
    .block(
        Block::default()
            .title(Span::styled("  Denoiser  ", focused_style(den_foc)))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if den_foc {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            })
            .padding(Padding::horizontal(1)),
    );
    f.render_widget(den_p, cols[0]);

    // ── Popper Stopper ────────────────────────────────────────────────────────
    let pop_foc = app.focus == Focus::PopperStopper;
    let pop_p = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("Status: ", Style::default().fg(C_DIM)),
            bool_span(ds.popper_stopper_enabled),
        ]),
        Line::from(""),
        Line::from(Span::styled("Reduces plosive", Style::default().fg(C_DIM))),
        Line::from(Span::styled(
            "sounds (p, b, t).",
            Style::default().fg(C_DIM),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "[Enter] to toggle",
            Style::default().fg(if pop_foc { C_FOCUS } else { C_DISABLED }),
        )),
    ])
    .block(
        Block::default()
            .title(Span::styled("  Popper Stopper  ", focused_style(pop_foc)))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if pop_foc {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            })
            .padding(Padding::horizontal(1)),
    );
    f.render_widget(pop_p, cols[1]);

    // ── Mute Button Disable ───────────────────────────────────────────────────
    let mbd_foc = app.focus == Focus::MuteBtnDisable;
    let mbd_p = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("Disabled: ", Style::default().fg(C_DIM)),
            bool_span(ds.mute_btn_disabled),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Prevents the physical",
            Style::default().fg(C_DIM),
        )),
        Line::from(Span::styled("mute button from", Style::default().fg(C_DIM))),
        Line::from(Span::styled("toggling mute.", Style::default().fg(C_DIM))),
        Line::from(""),
        Line::from(Span::styled(
            "[Enter] to toggle",
            Style::default().fg(if mbd_foc { C_FOCUS } else { C_DISABLED }),
        )),
    ])
    .block(
        Block::default()
            .title(Span::styled("  Mute Button  ", focused_style(mbd_foc)))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if mbd_foc {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            })
            .padding(Padding::horizontal(1)),
    );
    f.render_widget(mbd_p, cols[2]);

    // ── HPF ───────────────────────────────────────────────────────────────────
    let hpf_foc = app.focus == Focus::Hpf;
    let hpf_lines: Vec<Line> = [HpfFrequency::Off, HpfFrequency::Hz75, HpfFrequency::Hz150]
        .iter()
        .map(|freq| {
            let selected = *freq == ds.hpf;
            Line::from(vec![
                Span::styled(
                    if selected { "▶ " } else { "  " },
                    Style::default().fg(C_ACCENT),
                ),
                Span::styled(
                    freq.to_string(),
                    if selected {
                        Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(C_DIM)
                    },
                ),
            ])
        })
        .collect();
    let mut hpf_content = vec![Line::from("")];
    hpf_content.extend(hpf_lines);
    hpf_content.push(Line::from(""));
    hpf_content.push(Line::from(Span::styled(
        "[Enter] to cycle",
        Style::default().fg(if hpf_foc { C_FOCUS } else { C_DISABLED }),
    )));
    let hpf_p = Paragraph::new(hpf_content).block(
        Block::default()
            .title(Span::styled(
                "  High-Pass Filter  ",
                if hpf_foc {
                    Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_TEXT)
                },
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if hpf_foc {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            })
            .padding(Padding::horizontal(1)),
    );
    f.render_widget(hpf_p, cols[3]);
}

// ─────────────────────────────────────────────────────────────────────────────
// MVX2U Gen 2 EQ Tab — same visual style as Gen 1, no master enable, no per-band enable
// ─────────────────────────────────────────────────────────────────────────────
fn draw_gen2_eq_tab(f: &mut Frame, app: &App, area: Rect) {
    // Gen 2: Auto mode shows the Tone slider; Manual mode shows the 5-band EQ.
    if app.device_state.mode == InputMode::Auto {
        draw_mv6_eq_tab(f, app, area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // band selector header
            Constraint::Min(0),    // band columns
        ])
        .margin(1)
        .split(area);

    // ── Header: band selector only (no master enable) ─────────────────────────
    let selected_freq = EQ_BAND_FREQS[app.eq_selected_band];
    let freq_label = if selected_freq >= 1000 {
        format!("{}k Hz", selected_freq / 1000)
    } else {
        format!("{} Hz", selected_freq)
    };
    let band_sel_focused = app.focus == Focus::EqBandSelect;
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "Selected Band:  ",
            Style::default().fg(if band_sel_focused { C_FOCUS } else { C_DIM }),
        ),
        Span::styled(
            format!("Band {} ", app.eq_selected_band + 1),
            if band_sel_focused {
                Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
            },
        ),
        Span::styled(format!("({})", freq_label), Style::default().fg(C_DIM)),
        Span::styled(
            if band_sel_focused {
                "  ◄ ► to change band"
            } else {
                ""
            },
            Style::default().fg(C_DIM),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if band_sel_focused {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            }),
    );
    f.render_widget(header, chunks[0]);

    // ── Band columns ──────────────────────────────────────────────────────────
    let band_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Percentage(20); 5])
        .split(chunks[1]);

    for (i, band) in app.device_state.eq_bands.iter().enumerate() {
        let selected = i == app.eq_selected_band;
        let gain_foc = app.focus == Focus::EqGain(i);

        let border_color = if gain_foc {
            C_FOCUS
        } else if selected {
            C_ACCENT
        } else {
            C_BORDER
        };

        // Vertical gain bar: −8.0 dB = bottom, +6.0 dB = top. Total span = 14.0 dB.
        // gain_db is in tenths, so range is −80..+60. bar_pos = (gain_db + 80) * 7 / 140.
        let bar_height: i32 = 7;
        let bar_pos = ((band.gain_db as i32 + 80) * bar_height / 140)
            .max(0)
            .min(bar_height);

        let mut bar_lines: Vec<Line> = Vec::new();
        for row in (0..=bar_height).rev() {
            let ch = if row == bar_pos {
                Span::styled(
                    "━━━━",
                    Style::default().fg(if band.gain_db > 0 {
                        C_ACCENT
                    } else {
                        Color::Rgb(50, 150, 220)
                    }),
                )
            } else if row == bar_height / 2 {
                Span::styled("────", Style::default().fg(C_BORDER))
            } else {
                Span::styled("    ", Style::default())
            };
            bar_lines.push(Line::from(ch));
        }

        let freq = EQ_BAND_FREQS[i];
        let freq_str = if freq >= 1000 {
            format!("{}k Hz", freq / 1000)
        } else {
            format!("{} Hz", freq)
        };

        let gain_db_f = band.gain_db as f32 / 10.0;
        let detail_lines = vec![
            Line::from(vec![
                Span::styled("Freq: ", Style::default().fg(C_DIM)),
                Span::styled(freq_str, Style::default().fg(C_DIM)),
            ]),
            Line::from(vec![
                Span::styled(
                    "Gain: ",
                    Style::default().fg(if gain_foc { C_FOCUS } else { C_DIM }),
                ),
                Span::styled(
                    format!("{:+.1} dB", gain_db_f),
                    if gain_foc {
                        Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                    } else if band.gain_db > 0 {
                        Style::default().fg(C_ACCENT)
                    } else if band.gain_db < 0 {
                        Style::default().fg(Color::Rgb(50, 150, 220))
                    } else {
                        Style::default().fg(C_DIM)
                    },
                ),
            ]),
        ];

        let mut all_lines = bar_lines;
        all_lines.push(Line::from(""));
        all_lines.extend(detail_lines);

        let block = Paragraph::new(all_lines)
            .block(
                Block::default()
                    .title(Span::styled(
                        format!(" Band {} ", i + 1),
                        if selected {
                            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(C_DIM)
                        },
                    ))
                    .borders(Borders::ALL)
                    .border_type(if selected {
                        BorderType::Thick
                    } else {
                        BorderType::Rounded
                    })
                    .border_style(Style::default().fg(border_color))
                    .padding(Padding::horizontal(1)),
            )
            .style(Style::default().bg(C_BG));
        f.render_widget(block, band_cols[i]);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// MVX2U Gen 2 Dynamics Tab — Limiter + Compressor (Gen 1 style) + Denoiser + Popper Stopper + HPF
// ─────────────────────────────────────────────────────────────────────────────
fn draw_gen2_dynamics_tab(f: &mut Frame, app: &App, area: Rect) {
    let ds = &app.device_state;

    // Auto mode: only Denoiser, Popper Stopper, HPF are available.
    // Manual mode: all five controls.
    let (num_cols, show_limiter_comp) = if ds.mode == InputMode::Auto {
        (3usize, false)
    } else {
        (5usize, true)
    };

    let constraints: Vec<Constraint> = (0..num_cols)
        .map(|_| Constraint::Ratio(1, num_cols as u32))
        .collect();

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .margin(1)
        .split(area);

    let mut col = 0usize;

    if show_limiter_comp {
        // ── Limiter — identical to Gen 1 ─────────────────────────────────────
        let lim_foc = app.focus == Focus::Limiter;
        let lim_p = Paragraph::new(vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("Status: ", Style::default().fg(C_DIM)),
                bool_span(ds.limiter_enabled),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "Prevents clipping by",
                Style::default().fg(C_DIM),
            )),
            Line::from(Span::styled(
                "capping the output",
                Style::default().fg(C_DIM),
            )),
            Line::from(Span::styled("level at 0 dBFS.", Style::default().fg(C_DIM))),
            Line::from(""),
            Line::from(Span::styled(
                "[Enter] to toggle",
                Style::default().fg(if lim_foc { C_FOCUS } else { C_DISABLED }),
            )),
        ])
        .block(
            Block::default()
                .title(Span::styled(
                    "  Limiter  ",
                    if lim_foc {
                        Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(C_TEXT)
                    },
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(if lim_foc {
                    Style::default().fg(C_FOCUS)
                } else {
                    Style::default().fg(C_BORDER)
                })
                .padding(Padding::horizontal(1)),
        );
        f.render_widget(lim_p, cols[col]);
        col += 1;

        // ── Compressor — identical to Gen 1 ──────────────────────────────────
        let comp_foc = app.focus == Focus::Compressor;
        let comp_lines: Vec<Line> = [
            CompressorPreset::Off,
            CompressorPreset::Light,
            CompressorPreset::Medium,
            CompressorPreset::Heavy,
        ]
        .iter()
        .map(|preset| {
            let selected = *preset == ds.compressor;
            Line::from(vec![
                Span::styled(
                    if selected { "▶ " } else { "  " },
                    Style::default().fg(C_ACCENT),
                ),
                Span::styled(
                    preset.to_string(),
                    if selected {
                        Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(C_DIM)
                    },
                ),
            ])
        })
        .collect();

        let mut comp_content = vec![Line::from("")];
        comp_content.extend(comp_lines);
        comp_content.push(Line::from(""));
        comp_content.push(Line::from(Span::styled(
            "[Enter] to cycle",
            Style::default().fg(if comp_foc { C_FOCUS } else { C_DISABLED }),
        )));

        let comp_p = Paragraph::new(comp_content).block(
            Block::default()
                .title(Span::styled(
                    "  Compressor  ",
                    if comp_foc {
                        Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(C_TEXT)
                    },
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(if comp_foc {
                    Style::default().fg(C_FOCUS)
                } else {
                    Style::default().fg(C_BORDER)
                })
                .padding(Padding::horizontal(1)),
        );
        f.render_widget(comp_p, cols[col]);
        col += 1;
    }

    // ── Denoiser — always visible ─────────────────────────────────────────────
    let den_foc = app.focus == Focus::Denoiser;
    let den_p = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("Status: ", Style::default().fg(C_DIM)),
            bool_span(ds.denoiser_enabled),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Reduces background",
            Style::default().fg(C_DIM),
        )),
        Line::from(Span::styled(
            "noise in real time.",
            Style::default().fg(C_DIM),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "[Enter] to toggle",
            Style::default().fg(if den_foc { C_FOCUS } else { C_DISABLED }),
        )),
    ])
    .block(
        Block::default()
            .title(Span::styled("  Denoiser  ", focused_style(den_foc)))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if den_foc {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            })
            .padding(Padding::horizontal(1)),
    );
    f.render_widget(den_p, cols[col]);
    col += 1;

    // ── Popper Stopper — always visible ───────────────────────────────────────
    let pop_foc = app.focus == Focus::PopperStopper;
    let pop_p = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("Status: ", Style::default().fg(C_DIM)),
            bool_span(ds.popper_stopper_enabled),
        ]),
        Line::from(""),
        Line::from(Span::styled("Reduces plosive", Style::default().fg(C_DIM))),
        Line::from(Span::styled(
            "sounds (p, b, t).",
            Style::default().fg(C_DIM),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "[Enter] to toggle",
            Style::default().fg(if pop_foc { C_FOCUS } else { C_DISABLED }),
        )),
    ])
    .block(
        Block::default()
            .title(Span::styled("  Popper Stopper  ", focused_style(pop_foc)))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if pop_foc {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            })
            .padding(Padding::horizontal(1)),
    );
    f.render_widget(pop_p, cols[col]);
    col += 1;

    // ── HPF — always visible ──────────────────────────────────────────────────
    let hpf_foc = app.focus == Focus::Hpf;
    let hpf_lines: Vec<Line> = [HpfFrequency::Off, HpfFrequency::Hz75, HpfFrequency::Hz150]
        .iter()
        .map(|freq| {
            let selected = *freq == ds.hpf;
            Line::from(vec![
                Span::styled(
                    if selected { "▶ " } else { "  " },
                    Style::default().fg(C_ACCENT),
                ),
                Span::styled(
                    freq.to_string(),
                    if selected {
                        Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(C_DIM)
                    },
                ),
            ])
        })
        .collect();

    let mut hpf_content = vec![Line::from("")];
    hpf_content.extend(hpf_lines);
    hpf_content.push(Line::from(""));
    hpf_content.push(Line::from(Span::styled(
        "[Enter] to cycle",
        Style::default().fg(if hpf_foc { C_FOCUS } else { C_DISABLED }),
    )));

    let hpf_p = Paragraph::new(hpf_content).block(
        Block::default()
            .title(Span::styled(
                "  High-Pass Filter  ",
                if hpf_foc {
                    Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_TEXT)
                },
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if hpf_foc {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            })
            .padding(Padding::horizontal(1)),
    );
    f.render_widget(hpf_p, cols[col]);
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab locked / not-available notices
// ─────────────────────────────────────────────────────────────────────────────

/// Shown on EQ/Dynamics when MVX2U is in Auto Level mode.
fn draw_tab_locked_notice(f: &mut Frame, area: Rect, tab_name: &str) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "🔒  Auto Level mode is active",
            Style::default().fg(C_WARN).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("{tab_name} is managed by the device and cannot be configured independently."),
            Style::default().fg(C_DIM),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Switch to Manual mode on the Main tab to access this section.",
            Style::default().fg(C_DIM),
        )),
    ];
    render_notice(f, area, lines);
}

fn render_notice(f: &mut Frame, area: Rect, lines: Vec<Line>) {
    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(C_DISABLED))
                .padding(Padding::horizontal(2)),
        )
        .alignment(Alignment::Left)
        .style(Style::default().bg(C_BG));
    f.render_widget(p, area);
}

// ─────────────────────────────────────────────────────────────────────────────
// EQ Tab
// ─────────────────────────────────────────────────────────────────────────────
fn draw_eq_tab(f: &mut Frame, app: &App, area: Rect) {
    if app.device_model == DeviceModel::Mv6 {
        draw_mv6_eq_tab(f, app, area);
        return;
    }
    if app.device_model == DeviceModel::Mvx2uGen2 {
        draw_gen2_eq_tab(f, app, area);
        return;
    }
    if app.device_state.mode == InputMode::Auto {
        draw_tab_locked_notice(f, area, "EQ");
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // master enable + band selector
            Constraint::Min(0),    // band columns
        ])
        .margin(1)
        .split(area);

    // ── EQ master enable header ───────────────────────────────────────────────
    let eq_en_focused = app.focus == Focus::EqEnable;
    let selected_freq = EQ_BAND_FREQS[app.eq_selected_band];
    let freq_label = if selected_freq >= 1000 {
        format!("{}k Hz", selected_freq / 1000)
    } else {
        format!("{} Hz", selected_freq)
    };
    let band_sel_focused = app.focus == Focus::EqBandSelect;
    let header = Paragraph::new(Line::from(vec![
        Span::styled("EQ:  ", Style::default().fg(C_DIM)),
        bool_span(app.device_state.eq_enabled),
        Span::styled("   ", Style::default()),
        Span::styled(
            "Selected Band:  ",
            Style::default().fg(if band_sel_focused { C_FOCUS } else { C_DIM }),
        ),
        Span::styled(
            format!("Band {} ", app.eq_selected_band + 1),
            if band_sel_focused {
                Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
            },
        ),
        Span::styled(format!("({})", freq_label), Style::default().fg(C_DIM)),
        Span::styled(
            if band_sel_focused {
                "  ◄ ► to change band"
            } else {
                ""
            },
            Style::default().fg(C_DIM),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if eq_en_focused || band_sel_focused {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            }),
    );
    f.render_widget(header, chunks[0]);

    // ── Band columns ──────────────────────────────────────────────────────────
    let band_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Percentage(20); 5])
        .split(chunks[1]);

    for (i, band) in app.device_state.eq_bands.iter().enumerate() {
        let selected = i == app.eq_selected_band;
        let en_foc = app.focus == Focus::EqBandEnable(i);
        let gain_foc = app.focus == Focus::EqGain(i);
        let any_foc = en_foc || gain_foc;

        let border_color = if any_foc {
            C_FOCUS
        } else if selected {
            C_ACCENT
        } else {
            C_BORDER
        };

        // Vertical gain bar: −8.0 dB = bottom (row 0), +6.0 dB = top (row 7).
        // Total span = 14.0 dB. gain_db is in tenths, so range is −80..+60.
        // bar_pos = (gain_db + 80) * 7 / 140.
        let bar_height: i32 = 7;
        let bar_pos = ((band.gain_db as i32 + 80) * bar_height / 140)
            .max(0)
            .min(bar_height);

        let mut bar_lines: Vec<Line> = Vec::new();
        for row in (0..=bar_height).rev() {
            let ch = if row == bar_pos {
                Span::styled(
                    "━━━━",
                    Style::default().fg(if band.gain_db > 0 {
                        C_ACCENT
                    } else {
                        Color::Rgb(50, 150, 220)
                    }),
                )
            } else if row == bar_height / 2 {
                Span::styled("────", Style::default().fg(C_BORDER))
            } else {
                Span::styled("    ", Style::default())
            };
            bar_lines.push(Line::from(ch));
        }

        // Fixed frequency label for this band
        let freq = EQ_BAND_FREQS[i];
        let freq_str = if freq >= 1000 {
            format!("{}k Hz", freq / 1000)
        } else {
            format!("{} Hz", freq)
        };

        let detail_lines = vec![
            Line::from(vec![
                Span::styled("Freq: ", Style::default().fg(C_DIM)),
                Span::styled(freq_str, Style::default().fg(C_DIM)),
            ]),
            Line::from(vec![
                Span::styled(
                    "On:   ",
                    Style::default().fg(if en_foc { C_FOCUS } else { C_DIM }),
                ),
                bool_span(band.enabled),
            ]),
            Line::from(vec![
                Span::styled(
                    "Gain: ",
                    Style::default().fg(if gain_foc { C_FOCUS } else { C_DIM }),
                ),
                Span::styled(
                    format!("{:+.1} dB", band.gain_db as f32 / 10.0),
                    if gain_foc {
                        Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                    } else if band.gain_db > 0 {
                        Style::default().fg(C_ACCENT)
                    } else if band.gain_db < 0 {
                        Style::default().fg(Color::Rgb(50, 150, 220))
                    } else {
                        Style::default().fg(C_DIM)
                    },
                ),
            ]),
        ];

        let mut all_lines = bar_lines;
        all_lines.push(Line::from(""));
        all_lines.extend(detail_lines);

        let block = Paragraph::new(all_lines)
            .block(
                Block::default()
                    .title(Span::styled(
                        format!(" Band {} ", i + 1),
                        if selected {
                            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(C_DIM)
                        },
                    ))
                    .borders(Borders::ALL)
                    .border_type(if selected {
                        BorderType::Thick
                    } else {
                        BorderType::Rounded
                    })
                    .border_style(Style::default().fg(border_color))
                    .padding(Padding::horizontal(1)),
            )
            .style(Style::default().bg(C_BG));
        f.render_widget(block, band_cols[i]);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Dynamics Tab
// ─────────────────────────────────────────────────────────────────────────────
fn draw_dynamics_tab(f: &mut Frame, app: &App, area: Rect) {
    if app.device_model == DeviceModel::Mv6 {
        draw_mv6_dynamics_tab(f, app, area);
        return;
    }
    if app.device_model == DeviceModel::Mvx2uGen2 {
        draw_gen2_dynamics_tab(f, app, area);
        return;
    }
    if app.device_state.mode == InputMode::Auto {
        draw_tab_locked_notice(f, area, "Dynamics");
        return;
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(33),
            Constraint::Percentage(33),
            Constraint::Percentage(34),
        ])
        .margin(1)
        .split(area);

    // ── Limiter ───────────────────────────────────────────────────────────────
    let lim_foc = app.focus == Focus::Limiter;
    let lim_p = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("Status: ", Style::default().fg(C_DIM)),
            bool_span(app.device_state.limiter_enabled),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Prevents clipping by",
            Style::default().fg(C_DIM),
        )),
        Line::from(Span::styled(
            "capping the output",
            Style::default().fg(C_DIM),
        )),
        Line::from(Span::styled("level at 0 dBFS.", Style::default().fg(C_DIM))),
        Line::from(""),
        Line::from(Span::styled(
            "[Enter] to toggle",
            Style::default().fg(if lim_foc { C_FOCUS } else { C_DISABLED }),
        )),
    ])
    .block(
        Block::default()
            .title(Span::styled(
                "  Limiter  ",
                if lim_foc {
                    Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_TEXT)
                },
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if lim_foc {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            })
            .padding(Padding::horizontal(1)),
    );
    f.render_widget(lim_p, cols[0]);

    // ── Compressor ────────────────────────────────────────────────────────────
    let comp_foc = app.focus == Focus::Compressor;
    let comp_lines: Vec<Line> = [
        CompressorPreset::Off,
        CompressorPreset::Light,
        CompressorPreset::Medium,
        CompressorPreset::Heavy,
    ]
    .iter()
    .map(|preset| {
        let selected = *preset == app.device_state.compressor;
        Line::from(vec![
            Span::styled(
                if selected { "▶ " } else { "  " },
                Style::default().fg(C_ACCENT),
            ),
            Span::styled(
                preset.to_string(),
                if selected {
                    Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_DIM)
                },
            ),
        ])
    })
    .collect();

    let mut comp_content = vec![Line::from("")];
    comp_content.extend(comp_lines);
    comp_content.push(Line::from(""));
    comp_content.push(Line::from(Span::styled(
        "[Enter] to cycle",
        Style::default().fg(if comp_foc { C_FOCUS } else { C_DISABLED }),
    )));

    let comp_p = Paragraph::new(comp_content).block(
        Block::default()
            .title(Span::styled(
                "  Compressor  ",
                if comp_foc {
                    Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_TEXT)
                },
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if comp_foc {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            })
            .padding(Padding::horizontal(1)),
    );
    f.render_widget(comp_p, cols[1]);

    // ── HPF ───────────────────────────────────────────────────────────────────
    let hpf_foc = app.focus == Focus::Hpf;
    let hpf_lines: Vec<Line> = [HpfFrequency::Off, HpfFrequency::Hz75, HpfFrequency::Hz150]
        .iter()
        .map(|freq| {
            let selected = *freq == app.device_state.hpf;
            Line::from(vec![
                Span::styled(
                    if selected { "▶ " } else { "  " },
                    Style::default().fg(C_ACCENT),
                ),
                Span::styled(
                    freq.to_string(),
                    if selected {
                        Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(C_DIM)
                    },
                ),
            ])
        })
        .collect();

    let mut hpf_content = vec![Line::from("")];
    hpf_content.extend(hpf_lines);
    hpf_content.push(Line::from(""));
    hpf_content.push(Line::from(Span::styled(
        "[Enter] to cycle",
        Style::default().fg(if hpf_foc { C_FOCUS } else { C_DISABLED }),
    )));

    let hpf_p = Paragraph::new(hpf_content).block(
        Block::default()
            .title(Span::styled(
                "  High-Pass Filter  ",
                if hpf_foc {
                    Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_TEXT)
                },
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if hpf_foc {
                Style::default().fg(C_FOCUS)
            } else {
                Style::default().fg(C_BORDER)
            })
            .padding(Padding::horizontal(1)),
    );
    f.render_widget(hpf_p, cols[2]);
}

// ─────────────────────────────────────────────────────────────────────────────
// Presets Tab
// ─────────────────────────────────────────────────────────────────────────────
fn draw_presets_tab(f: &mut Frame, app: &App, area: Rect) {
    // Each slot gets a fixed-height card: name row (3) + actions row (3) = 6 lines each.
    let slot_constraints: Vec<Constraint> = (0..4).map(|_| Constraint::Length(7)).collect();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(slot_constraints)
        .margin(1)
        .split(area);

    for i in 0..4 {
        draw_preset_card(f, app, i, rows[i]);
    }
}

fn draw_preset_card(f: &mut Frame, app: &App, index: usize, area: Rect) {
    // Split the card into name row and actions row.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Length(4)])
        .split(area);

    draw_preset_name_row(f, app, index, rows[0]);
    draw_preset_actions_row(f, app, index, rows[1]);
}

fn draw_preset_name_row(f: &mut Frame, app: &App, index: usize, area: Rect) {
    let focused = app.focus == Focus::PresetName(index);
    let editing = app.editing_preset_name && app.editing_preset_index == index;

    let (name_text, border_color, title_style) = match &app.presets[index] {
        Some(slot) => {
            let display = if editing {
                format!("{}_", slot.name) // show cursor
            } else {
                slot.name.clone()
            };
            let color = if editing {
                C_ACCENT
            } else if focused {
                C_FOCUS
            } else {
                C_BORDER
            };
            let style = if focused || editing {
                Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(C_TEXT)
            };
            (display, color, style)
        }
        None => {
            let color = if focused { C_FOCUS } else { C_DISABLED };
            let style = if focused {
                Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(C_DISABLED)
            };
            (String::from("Empty"), color, style)
        }
    };

    let hint = if editing {
        Span::styled("  [Enter/Esc] confirm", Style::default().fg(C_ACCENT))
    } else if focused && app.presets[index].is_some() {
        Span::styled("  [Enter] rename", Style::default().fg(C_DIM))
    } else {
        Span::raw("")
    };

    let summary_line = match &app.presets[index] {
        Some(slot) if !editing => Line::from(Span::styled(
            slot.summary(app.device_model),
            Style::default().fg(C_DIM),
        )),
        _ => Line::from(""),
    };

    let content = vec![
        Line::from(vec![
            Span::styled(
                format!("  {name_text}"),
                if editing {
                    Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_TEXT)
                },
            ),
            hint,
        ]),
        summary_line,
    ];

    let block = Paragraph::new(content)
        .block(
            Block::default()
                .title(Span::styled(
                    format!("  Preset {}  ", index + 1),
                    title_style,
                ))
                .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
                .border_type(if focused || editing {
                    BorderType::Thick
                } else {
                    BorderType::Rounded
                })
                .border_style(Style::default().fg(border_color))
                .padding(Padding::horizontal(1)),
        )
        .style(Style::default().bg(C_BG));
    f.render_widget(block, area);
}

fn draw_preset_actions_row(f: &mut Frame, app: &App, index: usize, area: Rect) {
    let focused = app.focus == Focus::PresetActions(index);
    let filled = app.presets[index].is_some();

    let actions: Line = if filled {
        Line::from(vec![
            Span::styled(
                " [Enter] ",
                Style::default()
                    .fg(if focused { C_ACCENT } else { C_DIM })
                    .add_modifier(if focused {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(
                "Load  ",
                Style::default().fg(if focused { C_TEXT } else { C_DIM }),
            ),
            Span::styled(
                " [s] ",
                Style::default().fg(if focused { C_ACCENT } else { C_DIM }),
            ),
            Span::styled(
                "Save  ",
                Style::default().fg(if focused { C_TEXT } else { C_DIM }),
            ),
            Span::styled(
                " [d] ",
                Style::default().fg(if focused { C_ERROR } else { C_DIM }),
            ),
            Span::styled(
                "Delete",
                Style::default().fg(if focused { C_TEXT } else { C_DIM }),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                " [s] ",
                Style::default().fg(if focused { C_ACCENT } else { C_DIM }),
            ),
            Span::styled(
                "Save current settings here",
                Style::default().fg(if focused { C_TEXT } else { C_DISABLED }),
            ),
        ])
    };

    let border_color = if focused { C_FOCUS } else { C_BORDER };

    let block = Paragraph::new(vec![Line::from(""), actions])
        .block(
            Block::default()
                .borders(Borders::BOTTOM | Borders::LEFT | Borders::RIGHT)
                .border_type(if focused {
                    BorderType::Thick
                } else {
                    BorderType::Rounded
                })
                .border_style(Style::default().fg(border_color))
                .padding(Padding::horizontal(1)),
        )
        .style(Style::default().bg(C_BG));
    f.render_widget(block, area);
}

// ─────────────────────────────────────────────────────────────────────────────
// Info Tab
// ─────────────────────────────────────────────────────────────────────────────
fn draw_info_tab(f: &mut Frame, app: &App, area: Rect) {
    let ds = &app.device_state;
    let model = app.device_model;

    let (vid_pid, gain_range) = match model {
        DeviceModel::Mvx2u => ("14ED:1013", "0–60 dB"),
        DeviceModel::Mvx2uGen2 => ("14ED:1033", "0–60 dB"),
        DeviceModel::Mv6 => ("14ED:1026", "0–36 dB"),
    };

    let mut lines = vec![
        Line::from(Span::styled(
            "  Device Information",
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Manufacturer : ", Style::default().fg(C_DIM)),
            Span::styled("Shure Inc.", Style::default().fg(C_TEXT)),
        ]),
        Line::from(vec![
            Span::styled("  Model        : ", Style::default().fg(C_DIM)),
            Span::styled(model.display_name(), Style::default().fg(C_TEXT)),
        ]),
        Line::from(vec![
            Span::styled("  Serial No.   : ", Style::default().fg(C_DIM)),
            Span::styled(&*ds.serial_number, Style::default().fg(C_TEXT)),
        ]),
        Line::from(vec![
            Span::styled("  USB VID/PID  : ", Style::default().fg(C_DIM)),
            Span::styled(vid_pid, Style::default().fg(C_TEXT)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Capabilities",
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Gain Range   : ", Style::default().fg(C_DIM)),
            Span::styled(gain_range, Style::default().fg(C_TEXT)),
        ]),
        Line::from(vec![
            Span::styled("  Presets      : ", Style::default().fg(C_DIM)),
            Span::styled("4 slots (host-side TOML)", Style::default().fg(C_TEXT)),
        ]),
    ];

    let cap_rows: &[(&str, &str)] = match model {
        DeviceModel::Mvx2u => &[
            ("  Phantom Pwr  : ", "48V"),
            ("  EQ           : ", "5-band parametric"),
            ("  HPF          : ", "Off / 75 Hz / 150 Hz"),
            ("  Compressor   : ", "Off / Light / Medium / Heavy"),
            ("  Limiter      : ", "On / Off"),
            ("  Monitor Mix  : ", "0–100%"),
            ("  Auto Level   : ", "On / Off"),
            ("  Config Lock  : ", "On / Off"),
        ],
        DeviceModel::Mvx2uGen2 => &[
            ("  Phantom Pwr  : ", "48V"),
            ("  EQ           : ", "5-band parametric"),
            ("  HPF          : ", "Off / 75 Hz / 150 Hz"),
            ("  Compressor   : ", "Off / Light / Medium / Heavy"),
            ("  Limiter      : ", "On / Off"),
            ("  Denoiser     : ", "On / Off"),
            ("  Popper Stop. : ", "On / Off"),
            (
                "  Tone         : ",
                "Dark (−100%) → Natural → Bright (+100%)",
            ),
            ("  Gain Lock    : ", "On / Off (Manual mode)"),
            ("  Monitor Mix  : ", "0–100%"),
            ("  Auto Level   : ", "On / Off"),
        ],
        DeviceModel::Mv6 => &[
            ("  Denoiser     : ", "On / Off"),
            ("  Popper Stop. : ", "On / Off"),
            (
                "  Tone         : ",
                "Dark (−100%) → Natural → Bright (+100%)",
            ),
            ("  HPF          : ", "Off / 75 Hz / 150 Hz"),
            ("  Auto Level   : ", "On / Off"),
            ("  Mute Button  : ", "Enable / Disable"),
        ],
    };

    for (label, value) in cap_rows {
        lines.push(Line::from(vec![
            Span::styled(*label, Style::default().fg(C_DIM)),
            Span::styled(*value, Style::default().fg(C_TEXT)),
        ]));
    }

    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            concat!(
                "  shurectl v",
                env!("CARGO_PKG_VERSION"),
                " — open-source Shure device configurator"
            ),
            Style::default().fg(C_DIM),
        )),
    ]);

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(C_BORDER))
                .padding(Padding::horizontal(1)),
        )
        .style(Style::default().bg(C_BG));
    f.render_widget(p, area);
}

// ─────────────────────────────────────────────────────────────────────────────
// Help Overlay
// ─────────────────────────────────────────────────────────────────────────────
fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let popup_width = 58u16.min(area.width.saturating_sub(4));
    let popup_height = 32u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(popup_width)) / 2;
    let y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    f.render_widget(Clear, popup_area);

    let lines = vec![
        Line::from(Span::styled(
            "  Keyboard Shortcuts",
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Navigation",
            Style::default()
                .fg(C_TEXT)
                .add_modifier(Modifier::UNDERLINED),
        )]),
        Line::from(vec![
            Span::styled("  Tab / Shift+Tab  ", Style::default().fg(C_ACCENT)),
            Span::styled("Switch section", Style::default().fg(C_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  ↑ / k            ", Style::default().fg(C_ACCENT)),
            Span::styled("Focus previous control", Style::default().fg(C_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  ↓ / j            ", Style::default().fg(C_ACCENT)),
            Span::styled("Focus next control", Style::default().fg(C_DIM)),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Adjustment",
            Style::default()
                .fg(C_TEXT)
                .add_modifier(Modifier::UNDERLINED),
        )]),
        Line::from(vec![
            Span::styled("  ← / h            ", Style::default().fg(C_ACCENT)),
            Span::styled("Decrease value", Style::default().fg(C_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  → / l            ", Style::default().fg(C_ACCENT)),
            Span::styled("Increase value", Style::default().fg(C_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  Enter / Space    ", Style::default().fg(C_ACCENT)),
            Span::styled("Toggle boolean / cycle option", Style::default().fg(C_DIM)),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  General",
            Style::default()
                .fg(C_TEXT)
                .add_modifier(Modifier::UNDERLINED),
        )]),
        Line::from(vec![
            Span::styled("  r                ", Style::default().fg(C_ACCENT)),
            Span::styled("Refresh state from device", Style::default().fg(C_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  ?                ", Style::default().fg(C_ACCENT)),
            Span::styled("Toggle this help", Style::default().fg(C_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  q / Ctrl+C       ", Style::default().fg(C_ACCENT)),
            Span::styled("Quit", Style::default().fg(C_DIM)),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Presets tab",
            Style::default()
                .fg(C_TEXT)
                .add_modifier(Modifier::UNDERLINED),
        )]),
        Line::from(vec![
            Span::styled("  Enter (on name)  ", Style::default().fg(C_ACCENT)),
            Span::styled("Rename preset", Style::default().fg(C_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  Enter (on actions)", Style::default().fg(C_ACCENT)),
            Span::styled("Load preset to device", Style::default().fg(C_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  s                ", Style::default().fg(C_ACCENT)),
            Span::styled("Save current settings to slot", Style::default().fg(C_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  d / Delete       ", Style::default().fg(C_ACCENT)),
            Span::styled("Delete preset", Style::default().fg(C_DIM)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Press ? to close",
            Style::default().fg(C_DISABLED),
        )),
    ];

    let help = Paragraph::new(lines).block(
        Block::default()
            .title(Span::styled(
                "  Help  ",
                Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(C_ACCENT))
            .style(Style::default().bg(Color::Rgb(22, 22, 22))),
    );
    f.render_widget(help, popup_area);
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── meter_color ───────────────────────────────────────────────────────────

    #[test]
    fn meter_color_below_minus_12_is_green() {
        assert_eq!(meter_color(-60.0), C_SUCCESS);
        assert_eq!(meter_color(-13.0), C_SUCCESS);
        // Just below the amber boundary.
        assert_eq!(meter_color(-12.1), C_SUCCESS);
    }

    #[test]
    fn meter_color_at_minus_12_is_amber() {
        assert_eq!(meter_color(-12.0), C_WARN);
    }

    #[test]
    fn meter_color_between_minus_12_and_minus_3_is_amber() {
        assert_eq!(meter_color(-6.0), C_WARN);
        // Just below the red boundary.
        assert_eq!(meter_color(-3.1), C_WARN);
    }

    #[test]
    fn meter_color_at_minus_3_is_red() {
        assert_eq!(meter_color(-3.0), C_ERROR);
    }

    #[test]
    fn meter_color_above_minus_3_is_red() {
        assert_eq!(meter_color(-1.0), C_ERROR);
        assert_eq!(meter_color(0.0), C_ERROR);
    }

    // ── build_ruler ───────────────────────────────────────────────────────────

    #[test]
    fn build_ruler_output_is_exactly_inner_width_chars() {
        for width in [40, 80, 120, 200] {
            let ruler = build_ruler(width);
            assert_eq!(
                ruler.chars().count(),
                width,
                "ruler must be exactly {width} chars wide"
            );
        }
    }

    #[test]
    fn build_ruler_contains_minus_60_at_left_edge() {
        // At any reasonable width, "-60" must appear near the left edge.
        let ruler = build_ruler(80);
        assert!(
            ruler.starts_with("-60"),
            "'-60' must appear at the left edge; got: {ruler:?}"
        );
    }

    #[test]
    fn build_ruler_contains_expected_labels_at_wide_width() {
        // At 200 chars all six labels have room and must all appear.
        let ruler = build_ruler(200);
        for label in ["-60", "-48", "-36", "-24", "-12", "0"] {
            assert!(
                ruler.contains(label),
                "expected label '{label}' missing from ruler: {ruler:?}"
            );
        }
    }

    #[test]
    fn build_ruler_skips_overlapping_labels_at_narrow_width() {
        // At 10 chars only the first label(s) can fit; must not panic or overflow.
        let ruler = build_ruler(10);
        assert_eq!(ruler.chars().count(), 10);
    }

    #[test]
    fn build_ruler_zero_width_returns_empty_string() {
        let ruler = build_ruler(0);
        assert_eq!(ruler, "", "zero-width ruler must be empty");
    }

    #[test]
    fn build_ruler_no_char_placed_beyond_inner_width() {
        // Regression: label placement must never write past the end of the buffer.
        for width in [1, 2, 3, 5, 10, 40, 80] {
            let ruler = build_ruler(width);
            assert_eq!(
                ruler.chars().count(),
                width,
                "ruler overflowed at width {width}"
            );
        }
    }

    #[test]
    fn build_ruler_minus_24_is_centred_near_midpoint() {
        // -24 dB is 60 % of the way from -60 to 0, so its centre should land
        // near column 60 % * width. We allow ±2 columns for rounding.
        let width = 100usize;
        let ruler = build_ruler(width);
        let expected_col = (0.6 * width as f32).round() as usize;
        // Find where "-24" starts in the ruler string.
        let start = ruler
            .find("-24")
            .expect("'-24' must be present in a 100-char ruler");
        let center = start + 1; // centre of the 3-char label "-24"
        assert!(
            center.abs_diff(expected_col) <= 2,
            "'-24' centre at col {center}, expected ~{expected_col}"
        );
    }
}
