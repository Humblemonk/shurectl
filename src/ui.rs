//! TUI rendering using ratatui.

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Gauge, Padding, Paragraph, Tabs, Wrap},
};

use crate::app::{App, Focus, Tab};
use crate::mouse::{HitMap, Target};
use crate::protocol::{
    AutoGain, AutoTone, CompressorPreset, DeviceModel, EQ_BAND_FREQS, EqPreset, HpfFrequency,
    InputMode, LedBehavior, LedLiveTheme, LedPulsingTheme, LedSolidTheme, MicPosition, MicPreset,
    ReverbType, format_gain,
};

/// Keep hit regions beside the layout that draws them, not in a second layout.
struct UiFrame<'a, 'b> {
    frame: &'a mut Frame<'b>,
    hits: HitMap,
}

impl UiFrame<'_, '_> {
    fn control(&mut self, area: Rect, focus: Focus) {
        self.hits
            .control(area.intersection(self.frame.area()), focus);
    }

    fn render_widget(&mut self, widget: impl ratatui::widgets::Widget, area: Rect) {
        self.frame.render_widget(widget, area);
    }
}

// ── Palette ───────────────────────────────────────────────────────────────────
// Shure's "Prism" web palette and MOTIV Mix: lime on near-black.
const C_ACCENT: Color = Color::Rgb(178, 255, 51); // prismgreen
const C_BG: Color = Color::Rgb(22, 22, 22); // prismgray-s70
const C_SURFACE: Color = Color::Rgb(34, 34, 34); // prismgray-s60
const C_BORDER: Color = Color::Rgb(64, 64, 65); // prismgray-s40
const C_TEXT: Color = Color::White;
const C_DIM: Color = Color::Rgb(140, 140, 142); // prismgray-s20
// MOTIV Mix's meter green; bluer than C_ACCENT so an ON value never reads as focus.
const C_SUCCESS: Color = Color::Rgb(0, 200, 83);
const C_ERROR: Color = Color::Rgb(255, 101, 103);
const C_WARN: Color = Color::Rgb(255, 170, 0);
const C_FOCUS: Color = C_ACCENT;
// prismgray-s30: 3:1 on C_BG, below the 4.5:1 text minimum, so only for inactive things.
const C_DISABLED: Color = Color::Rgb(98, 98, 100);
// Darker than prismblue (C_PLAYBACK_LEVEL), so the two monitor gauges stay distinct.
const C_BLUE: Color = Color::Rgb(50, 150, 220);

/// A text row clipped to its widget's inner area, including tiny terminals.
fn text_row(inner: Rect, offset: u16) -> Rect {
    Rect::new(inner.x, inner.y.saturating_add(offset), inner.width, 1).intersection(inner)
}

fn focused_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(C_TEXT)
    }
}

/// A rounded block whose border turns thick and lights up when its control has
/// focus, so focus doesn't rely on lime alone (ON is green). Gauges that show
/// their value in the title add their own.
fn focus_border(focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Rounded
        })
        .border_style(Style::default().fg(if focused { C_FOCUS } else { C_BORDER }))
}

/// A rounded block whose title and border light up when one of its controls has focus.
fn focus_block(title: &str, focused: bool) -> Block<'static> {
    alert_block(title, focused, None)
}

/// `focus_block()` whose title and border take the `alert` color (red while muted,
/// amber while locked) when it isn't focused.
fn alert_block(title: &str, focused: bool, alert: Option<Color>) -> Block<'static> {
    let alert = alert.filter(|_| !focused);
    let title_style = match alert {
        Some(color) => Style::default().fg(color).add_modifier(Modifier::BOLD),
        None => focused_style(focused),
    };
    let block = focus_border(focused).title(Span::styled(format!("  {title}  "), title_style));
    match alert {
        Some(color) => block.border_style(Style::default().fg(color)),
        None => block,
    }
}

/// A slider: its label, the value in bold accent and the adjust hint (while
/// focused) in the title, and a `color` bar that turns bold on focus. Callers
/// add the ratio and label.
fn slider_gauge(label: &str, value: &str, focused: bool, color: Color) -> Gauge<'static> {
    let block = focus_border(focused).title(Line::from(vec![
        Span::styled(format!("  {label}  "), focused_style(focused)),
        Span::styled(
            format!(" {value} "),
            Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if focused { "  ◄ ► to adjust" } else { "" },
            Style::default().fg(C_DIM),
        ),
    ]));
    let bar_style = Style::default().fg(color).bg(C_SURFACE);
    Gauge::default().block(block).gauge_style(if focused {
        bar_style.add_modifier(Modifier::BOLD)
    } else {
        bar_style
    })
}

/// Make a whole multi-row block clickable: each row's control owns the block
/// from its row down (the first also takes the title), and later regions win.
/// Rows squeezed out of a short terminal take nothing.
fn control_rows(
    f: &mut UiFrame,
    block_area: Rect,
    rows: &[Rect],
    focuses: impl Iterator<Item = Focus>,
) {
    for (i, (row, focus)) in rows.iter().zip(focuses).enumerate() {
        if row.height == 0 {
            continue;
        }
        let top = if i == 0 { block_area.y } else { row.y };
        let height = block_area.bottom().saturating_sub(top);
        f.control(
            Rect::new(block_area.x, top, block_area.width, height),
            focus,
        );
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
pub fn draw(frame: &mut Frame, app: &App) -> HitMap {
    let size = frame.area();
    let mut context = UiFrame {
        frame,
        hits: HitMap::default(),
    };
    let f = &mut context;

    // Background
    f.render_widget(Block::default().style(Style::default().bg(C_BG)), size);

    // Outer layout: header / tabs / content / status bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Length(2), // tabs
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
        Tab::Reverb => draw_reverb_tab(f, app, chunks[2]),
        Tab::Led => draw_led_tab(f, app, chunks[2]),
        Tab::Presets => draw_presets_tab(f, app, chunks[2]),
        Tab::Info => draw_info_tab(f, app, chunks[2]),
    }

    draw_status(f, app, chunks[3]);

    if app.help_visible {
        draw_help_overlay(f, size);
    }
    context.hits
}

fn draw_header(f: &mut UiFrame, app: &App, area: Rect) {
    let (connection_label, connection_color) = if app.demo_mode {
        (" [DEMO — no device] ", C_WARN)
    } else if app.device_connected {
        (" [CONNECTED] ", C_SUCCESS)
    } else {
        (" [DISCONNECTED] ", C_ERROR)
    };
    let demo_tag = Span::styled(
        connection_label,
        Style::default()
            .fg(connection_color)
            .add_modifier(Modifier::BOLD),
    );

    // Identity label: the user-set device name when present, otherwise the model.
    let device_label: &str = if app.device_state.device_name != "Unknown" {
        &app.device_state.device_name
    } else {
        app.device_model.display_name()
    };

    let title = vec![
        Span::styled(
            "shurectl",
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        demo_tag,
        Span::styled(format!("  {device_label}"), Style::default().fg(C_TEXT)),
        Span::styled(
            format!("  S/N: {}", app.device_state.display_serial()),
            Style::default().fg(C_DIM),
        ),
    ];

    let p = Paragraph::new(Line::from(title))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(C_BORDER))
                .style(Style::default().bg(C_BG)),
        )
        .alignment(Alignment::Left);
    f.render_widget(p, area);
}

fn draw_tabs(f: &mut UiFrame, app: &App, area: Rect) {
    // Tabs the model has no settings for (Reverb and LED on most devices) are
    // hidden entirely. Temporarily locked tabs (EQ, Dynamics on MVX2U Gen 1 and
    // MV7 in Auto mode) remain visible with a lock icon since they're meaningful
    // but currently inaccessible.
    let visible: Vec<Tab> = Tab::ALL
        .iter()
        .copied()
        .filter(|t| !app.is_tab_hidden(*t))
        .collect();

    let titles: Vec<Line> = visible
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

    // Tabs adds one space on either side and a one-cell divider. Use Line's
    // display width so the lock icon doesn't shift subsequent click targets.
    let mut x = area.x;
    for (tab, title) in visible.iter().zip(&titles) {
        let width = title.width() as u16 + 2;
        f.hits.add(
            Rect::new(x, area.y, width, 1).intersection(area),
            Target::Tab(*tab),
        );
        x = x.saturating_add(width + 1);
    }

    let selected = visible
        .iter()
        .position(|t| *t == app.active_tab)
        .unwrap_or(0);

    let tabs = Tabs::new(titles)
        .select(selected)
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(C_BORDER)),
        )
        .style(Style::default().fg(C_DIM).bg(C_BG))
        // MOTIV Mix's Mixer / Library tabs: bold white over a lime underline.
        .highlight_style(
            Style::default()
                .fg(C_TEXT)
                .underline_color(C_ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider(Span::styled("│", Style::default().fg(C_BORDER)));

    f.render_widget(tabs, area);
}

fn draw_status(f: &mut UiFrame, app: &App, area: Rect) {
    let (msg, color) = if app.status_is_error {
        (format!("✗ {}", app.status_message), C_ERROR)
    } else {
        (format!("  {}", app.status_message), C_DIM)
    };

    let hint = if app.editing_name() {
        Span::styled(
            " Editing name — [Enter] save  [Esc/click outside] cancel  [Backspace] delete",
            Style::default().fg(C_ACCENT),
        )
    } else if app.can_flatten_eq() {
        Span::styled(
            " [Tab] Next section  [↑↓] Focus  [←→] Adjust  [f] Flat  [r] Refresh  [?] Help  [q] Quit",
            Style::default().fg(C_DIM),
        )
    } else {
        Span::styled(
            " [Tab] Next section  [↑↓] Focus  [←→/Enter] Adjust  [r] Refresh  [?] Help  [q] Quit",
            Style::default().fg(C_DIM),
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
fn draw_main_tab(f: &mut UiFrame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    draw_main_left(f, app, cols[0]);
    draw_main_right(f, app, cols[1]);
}

fn draw_main_left(f: &mut UiFrame, app: &App, area: Rect) {
    match (app.device_model, app.device_state.mode) {
        (DeviceModel::Mv7Plus, InputMode::Manual) => draw_main_left_mv7plus(f, app, area),
        (DeviceModel::Mv7Plus, InputMode::Auto) => draw_main_left_mv7plus_auto(f, app, area),
        (DeviceModel::Mv6, InputMode::Manual) => draw_main_left_mv6_manual(f, app, area),
        (DeviceModel::Mv6, InputMode::Auto) => draw_main_left_mv6_auto(f, app, area),
        (DeviceModel::Mv6Gen2, InputMode::Manual) => draw_main_left_mv6_manual(f, app, area),
        (DeviceModel::Mv6Gen2, InputMode::Auto) => draw_main_left_mv6_gen2_auto(f, app, area),
        (DeviceModel::Mv7, InputMode::Manual) => draw_main_left_mv7_manual(f, app, area),
        (DeviceModel::Mv7, InputMode::Auto) => draw_main_left_mv7_auto(f, app, area),
        (DeviceModel::Mvx2uGen2, InputMode::Manual) => draw_main_left_gen2_manual(f, app, area),
        (DeviceModel::Mvx2uGen2, InputMode::Auto) => draw_main_left_gen2_auto(f, app, area),
        (DeviceModel::Mvx2u, InputMode::Auto) => draw_main_left_auto(f, app, area),
        (DeviceModel::Mvx2u, InputMode::Manual) => draw_main_left_manual(f, app, area),
    }
}

fn draw_main_left_mv7plus(f: &mut UiFrame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode
            Constraint::Length(3), // mute
            Constraint::Length(3), // gain
            Constraint::Length(4), // level meter
            Constraint::Length(4), // monitor mix (mic + playback level)
            Constraint::Min(0),    // spacer
        ])
        .horizontal_margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);

    draw_gain_gauge(f, app, rows[2]);

    draw_meter(f, app, rows[3]);
    draw_monitor_mix_gauge(f, app, rows[4]);
}

fn draw_main_left_mv7plus_auto(f: &mut UiFrame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode
            Constraint::Length(3), // mute
            Constraint::Length(4), // level meter
            Constraint::Length(4), // monitor mix (mic + playback level)
            Constraint::Min(0),    // spacer
        ])
        .horizontal_margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);
    draw_meter(f, app, rows[2]);
    draw_monitor_mix_gauge(f, app, rows[3]);
}

// MV7 Manual: Mode → Mute → Gain → Meter → MonitorMix → Lock
fn draw_main_left_mv7_manual(f: &mut UiFrame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode
            Constraint::Length(3), // mute
            Constraint::Length(3), // gain gauge
            Constraint::Length(4), // level meter
            Constraint::Length(3), // monitor mix
            Constraint::Length(3), // lock
            Constraint::Min(0),    // spacer
        ])
        .horizontal_margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);
    draw_gain_gauge(f, app, rows[2]);
    draw_meter(f, app, rows[3]);
    draw_monitor_mix_gauge(f, app, rows[4]);
    draw_config_lock_block(f, app, rows[5]);
}

// MV7 Auto: Mode → Mute → Auto Level (Position, Tone) → Meter → MonitorMix → Lock
fn draw_main_left_mv7_auto(f: &mut UiFrame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode
            Constraint::Length(3), // mute
            Constraint::Length(4), // auto controls (2 rows + 2 borders)
            Constraint::Length(4), // level meter
            Constraint::Length(3), // monitor mix
            Constraint::Length(3), // lock
            Constraint::Min(0),    // spacer
        ])
        .horizontal_margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);
    draw_auto_controls(f, app, rows[2], &[AutoRow::Position, AutoRow::Tone]);
    draw_meter(f, app, rows[3]);
    draw_monitor_mix_gauge(f, app, rows[4]);
    draw_config_lock_block(f, app, rows[5]);
}

fn draw_main_left_mv6_manual(f: &mut UiFrame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode
            Constraint::Length(3), // mute
            Constraint::Length(3), // gain gauge
            Constraint::Length(3), // gain lock
            Constraint::Length(4), // level meter
            Constraint::Length(4), // monitor mix (mic + playback level)
            Constraint::Length(3), // mic preset (MV6 Gen 2)
            Constraint::Min(0),    // spacer
        ])
        .horizontal_margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);

    draw_gain_gauge(f, app, rows[2]);

    draw_gain_lock_block(f, app, rows[3]);

    draw_meter(f, app, rows[4]);
    draw_monitor_mix_gauge(f, app, rows[5]);
    if app.device_model.has_mic_presets() {
        draw_mic_preset_block(f, app, rows[6]);
    }
}

/// MV6 Gen 2 built-in presets. None is highlighted when the mode, Denoiser and
/// Popper Stopper match none of them, as the mic itself reports.
fn draw_mic_preset_block(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::MicPreset);
    let focused = app.focus == Focus::MicPreset;
    let current = MicPreset::matching(&app.device_state);
    let mut spans = vec![Span::styled("Preset:  ", Style::default().fg(C_DIM))];
    for preset in MicPreset::ALL {
        spans.push(segmented_span(preset.label(), current == Some(preset)));
    }
    if current.is_none() {
        spans.push(Span::styled("  (custom)", Style::default().fg(C_DIM)));
    }
    spans.push(Span::styled(
        if focused { "  [Enter] cycle" } else { "" },
        Style::default().fg(C_DIM),
    ));
    let p = Paragraph::new(Line::from(spans)).block(focus_block("Mic Preset", focused));
    f.render_widget(p, area);
}

fn draw_main_left_mv6_auto(f: &mut UiFrame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode
            Constraint::Length(3), // mute
            Constraint::Length(4), // level meter
            Constraint::Length(4), // monitor mix (mic + playback level)
            Constraint::Min(0),    // spacer
        ])
        .horizontal_margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);
    draw_meter(f, app, rows[2]);
    draw_monitor_mix_gauge(f, app, rows[3]);
}

// MV6 Gen 2 Auto: the MV6 Auto layout plus its Auto Level controls, placed where
// the MVX2U Gen 1 has them (between Mute and the meter), the MV7+ Mic Level and
// Playback Level, and the Mic Preset last.
fn draw_main_left_mv6_gen2_auto(f: &mut UiFrame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode
            Constraint::Length(3), // mute
            Constraint::Length(4), // auto controls (2 rows + 2 borders)
            Constraint::Length(4), // level meter
            Constraint::Length(4), // monitor mix (mic + playback level)
            Constraint::Length(3), // mic preset
            Constraint::Min(0),    // spacer
        ])
        .horizontal_margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);
    draw_auto_controls(
        f,
        app,
        rows[2],
        &[AutoRow::TargetLevel, AutoRow::AdaptationRate],
    );
    draw_meter(f, app, rows[3]);
    draw_monitor_mix_gauge(f, app, rows[4]);
    draw_mic_preset_block(f, app, rows[5]);
}

// Gen 2 Manual: Mode → Mute → Gain → GainLock → Meter → MonitorMix → Phantom
fn draw_main_left_gen2_manual(f: &mut UiFrame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode
            Constraint::Length(3), // mute
            Constraint::Length(3), // gain gauge
            Constraint::Length(3), // gain lock
            Constraint::Length(4), // level meter
            Constraint::Length(4), // monitor mix (mic + playback level)
            Constraint::Length(3), // phantom
            Constraint::Min(0),    // spacer
        ])
        .horizontal_margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);

    draw_gain_gauge(f, app, rows[2]);

    draw_gain_lock_block(f, app, rows[3]);
    draw_meter(f, app, rows[4]);
    draw_monitor_mix_gauge(f, app, rows[5]);
    draw_phantom_block(f, app, rows[6]);
}

// Gen 2 Auto: Mode → Mute → Meter → MonitorMix → Phantom
fn draw_main_left_gen2_auto(f: &mut UiFrame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode
            Constraint::Length(3), // mute
            Constraint::Length(4), // level meter
            Constraint::Length(4), // monitor mix (mic + playback level)
            Constraint::Length(3), // phantom
            Constraint::Min(0),    // spacer
        ])
        .horizontal_margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);
    draw_meter(f, app, rows[2]);
    draw_monitor_mix_gauge(f, app, rows[3]);
    draw_phantom_block(f, app, rows[4]);
}

fn draw_main_left_manual(f: &mut UiFrame, app: &App, area: Rect) {
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
        .horizontal_margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);

    draw_gain_gauge(f, app, rows[2]);

    draw_main_shared(f, app, &rows[3..]);
}

fn draw_main_left_auto(f: &mut UiFrame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // mode          (1 line + 2 borders)
            Constraint::Length(3), // mute          (1 line + 2 borders)
            Constraint::Length(5), // auto controls (3 rows + 2 borders)
            Constraint::Length(4), // level meter   (bar + ruler + 2 borders)
            Constraint::Length(3), // monitor mix   (1 bar + 2 borders)
            Constraint::Length(3), // phantom       (1 line + 2 borders)
            Constraint::Length(3), // lock          (1 line + 2 borders)
            Constraint::Min(0),    // spacer
        ])
        .horizontal_margin(1)
        .split(area);

    draw_mode_block(f, app, rows[0]);
    draw_mute_block(f, app, rows[1]);
    draw_auto_controls(
        f,
        app,
        rows[2],
        &[AutoRow::Position, AutoRow::Tone, AutoRow::Gain],
    );
    draw_main_shared(f, app, &rows[3..]);
}

/// One row of the Auto Level controls block. Each model passes the rows it has.
#[derive(Clone, Copy)]
enum AutoRow {
    Position,
    Tone,
    /// MVX2U Gen 1 Auto Gain.
    Gain,
    /// MV6 Gen 2 Target Level: the same setting as `Gain`, under its own name.
    TargetLevel,
    AdaptationRate,
}

impl AutoRow {
    fn focus(self) -> Focus {
        match self {
            AutoRow::Position => Focus::AutoPosition,
            AutoRow::Tone => Focus::AutoTone,
            AutoRow::Gain | AutoRow::TargetLevel => Focus::AutoGain,
            AutoRow::AdaptationRate => Focus::AdaptationRate,
        }
    }

    fn label(self) -> &'static str {
        match self {
            AutoRow::Position => "Mic Position:",
            AutoRow::Tone => "Tone:",
            AutoRow::Gain => "Gain:",
            AutoRow::TargetLevel => "Target Level:",
            AutoRow::AdaptationRate => "Adaptation Rate:",
        }
    }

    /// Each option's label and whether it is the current value.
    fn options(self, app: &App) -> Vec<(&'static str, bool)> {
        let ds = &app.device_state;
        match self {
            AutoRow::Position => vec![
                ("Near", ds.auto_position == MicPosition::Near),
                ("Far", ds.auto_position == MicPosition::Far),
            ],
            AutoRow::Tone => vec![
                ("Dark", ds.auto_tone == AutoTone::Dark),
                ("Natural", ds.auto_tone == AutoTone::Natural),
                ("Bright", ds.auto_tone == AutoTone::Bright),
            ],
            AutoRow::Gain => vec![
                ("Quiet", ds.auto_gain == AutoGain::Quiet),
                ("Normal", ds.auto_gain == AutoGain::Normal),
                ("Loud", ds.auto_gain == AutoGain::Loud),
            ],
            AutoRow::TargetLevel => [AutoGain::Quiet, AutoGain::Normal, AutoGain::Loud]
                .iter()
                .map(|gain| (gain.target_level_label(), ds.auto_gain == *gain))
                .collect(),
            AutoRow::AdaptationRate => vec![
                ("Slow", !ds.adaptation_rate_fast),
                ("Fast", ds.adaptation_rate_fast),
            ],
        }
    }
}

/// Renders the Auto Level sub-controls, one `rows` entry per line.
///
/// Each row shows all options for that setting as a `segmented_span()` strip;
/// the focused row's label takes the focus colour.
fn draw_auto_controls(f: &mut UiFrame, app: &App, area: Rect, rows: &[AutoRow]) {
    let any_focused = rows.iter().any(|row| app.focus == row.focus());
    let block = focus_block("Auto Level Controls", any_focused);
    let inner_area = block.inner(area);
    f.render_widget(block, area);

    // Horizontal padding only: vertical padding cost two rows and squeezed the
    // Tone row out of 30-row terminals.
    let inner_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(1); rows.len()])
        .horizontal_margin(1)
        .split(inner_area);
    control_rows(f, area, &inner_rows, rows.iter().map(|row| row.focus()));
    // Options start in one column: pad every label to the longest plus a space.
    let label_width = rows.iter().map(|row| row.label().len()).max().unwrap_or(0) + 1;

    for (row, row_area) in rows.iter().zip(inner_rows.iter()) {
        let focused = app.focus == row.focus();
        let mut spans = vec![Span::styled(
            format!("{:<label_width$}", row.label()),
            Style::default().fg(if focused { C_FOCUS } else { C_DIM }),
        )];
        for (label, active) in row.options(app) {
            spans.push(segmented_span(label, active));
        }
        spans.push(Span::styled(
            if focused { "  [Enter] cycle" } else { "" },
            Style::default().fg(C_DIM),
        ));
        f.render_widget(Paragraph::new(Line::from(spans)), *row_area);
    }
}

/// One segment of a MOTIV-style segmented control: the active option is a
/// black-on-lime pill, the others gray segments. Push them with no gap between
/// so they read as one strip.
fn segmented_span(label: &str, active: bool) -> Span<'static> {
    let style = if active {
        Style::default()
            .fg(Color::Black)
            .bg(C_ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(C_TEXT).bg(C_BORDER)
    };
    Span::styled(format!(" {label} "), style)
}

/// Renders the manual Gain gauge, shared by every model. The locked styling
/// follows `App::gain_locked()`, the same check that makes adjust_focused() ignore ←/→.
fn draw_gain_gauge(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::Gain);
    let gain_focused = app.focus == Focus::Gain;
    let gain = app.device_state.gain_tenths;
    let max = app.device_model.max_gain_tenths();
    let gain_locked = app.gain_locked();
    let title = Line::from(vec![
        Span::styled("  Gain  ", focused_style(gain_focused)),
        Span::styled(
            format!(" {} ", format_gain(gain)),
            Style::default()
                .fg(if gain_locked { C_DIM } else { C_TEXT })
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
    ]);
    let gauge = Gauge::default()
        .block(focus_border(gain_focused).title(title))
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
        .ratio((f64::from(gain) / f64::from(max)).clamp(0.0, 1.0))
        .label(format!("{} / {}", format_gain(gain), format_gain(max)));
    f.render_widget(gauge, area);
}

/// Renders the Input Mode block (Manual / Auto toggle only).
fn draw_mode_block(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::Mode);
    let focused = app.focus == Focus::Mode;

    let mode = app.device_state.mode;
    let p = Paragraph::new(Line::from(vec![
        Span::styled("Mode:  ", Style::default().fg(C_DIM)),
        segmented_span(&InputMode::Auto.to_string(), mode == InputMode::Auto),
        segmented_span(&InputMode::Manual.to_string(), mode == InputMode::Manual),
        Span::styled(
            if focused { "  [Enter] toggle" } else { "" },
            Style::default().fg(C_DIM),
        ),
    ]))
    .block(focus_block("Input Mode", focused));
    f.render_widget(p, area);
}

/// Renders the Mute block as a standalone control. Mute is independent of
/// input mode — it silences output regardless of Manual or Auto state.
fn draw_mute_block(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::Mute);
    let focused = app.focus == Focus::Mute;
    let muted = app.device_state.muted;

    let p = Paragraph::new(Line::from(vec![
        Span::styled("Mute:  ", Style::default().fg(C_DIM)),
        // Muted is the alert state, red like the block and the Summary's YES.
        if muted {
            bool_span(true).patch_style(Style::default().fg(C_ERROR))
        } else {
            bool_span(false)
        },
        Span::styled(
            if focused { "  [Enter] toggle" } else { "" },
            Style::default().fg(C_DIM),
        ),
    ]))
    .block(alert_block("Mute", focused, muted.then_some(C_ERROR)));
    f.render_widget(p, area);
}

// prismblue, brighter than Monitor Mix blue, to tell Mic Level and Playback Level apart at a glance.
const C_PLAYBACK_LEVEL: Color = Color::Rgb(65, 211, 248);

/// Renders the Monitor Mix block. Models with two monitor levels
/// (`has_playback_mix()`) get MOTIV's Mic Level and Playback Level rows and
/// need 4 rows of `area`; the rest get one mic ↔ playback balance gauge in 3.
fn draw_monitor_mix_gauge(f: &mut UiFrame, app: &App, area: Rect) {
    if app.device_model.has_playback_mix() {
        draw_monitor_levels(f, app, area);
        return;
    }
    f.control(area, Focus::MonitorMix);
    let mix = app.device_state.monitor_mix;
    let mix_gauge = Gauge::default()
        .block(focus_block(
            MONITOR_MIX_TITLE,
            app.focus == Focus::MonitorMix,
        ))
        .gauge_style(Style::default().fg(C_BLUE).bg(C_SURFACE))
        .ratio(mix as f64 / 100.0)
        .label(format!("Mic ◄─{mix:3}%─► Playback"));
    f.render_widget(mix_gauge, area);
}

/// Mic Level and Playback Level rows in one block, laid out as in MOTIV.
fn draw_monitor_levels(f: &mut UiFrame, app: &App, area: Rect) {
    let levels = [
        (
            "Mic Level",
            Focus::MonitorMix,
            app.device_state.monitor_mix,
            C_BLUE,
        ),
        (
            "Playback Level",
            Focus::PlaybackMix,
            app.device_state.playback_mix,
            C_PLAYBACK_LEVEL,
        ),
    ];
    let block = focus_block(
        MONITOR_MIX_TITLE,
        levels.iter().any(|level| app.focus == level.1),
    );
    let inner_area = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1); 2])
        .horizontal_margin(1)
        .split(inner_area);
    // The whole block takes clicks, as the single-gauge Monitor Mix does.
    control_rows(f, area, &rows, levels.iter().map(|level| level.1));
    let label_width = levels.iter().map(|level| level.0.len()).max().unwrap_or(0) as u16 + 2;
    for ((label, focus, level, color), row_area) in levels.into_iter().zip(rows.iter()) {
        let [label_area, gauge_area] =
            Layout::horizontal([Constraint::Length(label_width), Constraint::Min(0)])
                .areas(*row_area);
        let label_color = if app.focus == focus { C_FOCUS } else { C_DIM };
        f.render_widget(
            Paragraph::new(Span::styled(label, Style::default().fg(label_color))),
            label_area,
        );
        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(color).bg(C_SURFACE))
            .ratio(level as f64 / 100.0)
            .label(format!("{level:3}%"));
        f.render_widget(gauge, gauge_area);
    }
}

const MONITOR_MIX_TITLE: &str = "Monitor Mix";

/// Renders the Phantom Power block as a standalone control.
/// Used by Gen 2 main tab and MVX2U via draw_main_shared.
fn draw_phantom_block(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::Phantom);
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
    .block(focus_block("Phantom Power", ph_focused));
    f.render_widget(phantom_p, area);
}

/// Renders the Gain Lock block, on models with `has_gain_lock()`.
fn draw_gain_lock_block(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::GainLock);
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
                Style::default().fg(C_WARN).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("Unlocked", Style::default().fg(C_TEXT))
        },
        Span::styled(
            if lock_focused { "  [Enter] toggle" } else { "" },
            Style::default().fg(C_DIM),
        ),
    ]))
    .block(alert_block(
        "Gain Lock",
        lock_focused,
        gain_locked.then_some(C_WARN),
    ));
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
fn draw_main_shared(f: &mut UiFrame, app: &App, rows: &[Rect]) {
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
    draw_config_lock_block(f, app, rows[3]);
}

/// Renders the Config Lock block. Used by MVX2U Gen 1 (via draw_main_shared) and MV7.
fn draw_config_lock_block(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::Lock);
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
                Style::default().fg(C_WARN).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("Unlocked", Style::default().fg(C_TEXT))
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
    .block(alert_block(
        "Config Lock",
        lock_focused,
        locked.then_some(C_WARN),
    ));
    f.render_widget(lock_p, area);
}

/// Rows of the Main tab's Summary, in display order. Models list the rows they
/// have in `summary_rows()` in this order (a test checks it), so a setting sits
/// in the same place on every model: Main-tab settings in left-column order,
/// then the sound settings in EQ and Dynamics tab order, then Reverb and LED.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SummaryRow {
    Mode,
    Muted,
    Gain,
    GainLock,
    Position,
    AutoGain,
    TargetLevel,
    Adaptation,
    Phantom,
    ConfigLock,
    MicPreset,
    Tone,
    Eq,
    Limiter,
    Compressor,
    Denoiser,
    PopperStopper,
    MuteButton,
    Hpf,
    ReverbOutput,
    ReverbMonitor,
    ReverbType,
    ReverbIntensity,
    LiveMeter,
    NightMode,
}

impl SummaryRow {
    /// Label, padded so the colons line up.
    fn label(self) -> &'static str {
        match self {
            SummaryRow::Mode => "Mode        : ",
            SummaryRow::Muted => "Muted       : ",
            SummaryRow::Gain => "Gain        : ",
            SummaryRow::GainLock => "Gain Lock   : ",
            SummaryRow::Position => "Position    : ",
            SummaryRow::AutoGain => "Auto Gain   : ",
            SummaryRow::TargetLevel => "Target Level: ",
            SummaryRow::Adaptation => "Adaptation  : ",
            SummaryRow::Phantom => "Phantom     : ",
            SummaryRow::ConfigLock => "Config Lock : ",
            SummaryRow::MicPreset => "Mic Preset  : ",
            SummaryRow::Tone => "Tone        : ",
            SummaryRow::Eq => "EQ          : ",
            SummaryRow::Limiter => "Limiter     : ",
            SummaryRow::Compressor => "Compressor  : ",
            SummaryRow::Denoiser => "Denoiser    : ",
            SummaryRow::PopperStopper => "Pop. Stopper: ",
            SummaryRow::MuteButton => "Mute Button : ",
            SummaryRow::Hpf => "HPF         : ",
            SummaryRow::ReverbOutput => "Reverb Out  : ",
            SummaryRow::ReverbMonitor => "Reverb Mon  : ",
            SummaryRow::ReverbType => "Reverb Type : ",
            SummaryRow::ReverbIntensity => "Reverb Int. : ",
            SummaryRow::LiveMeter => "Live Meter  : ",
            SummaryRow::NightMode => "Night Mode  : ",
        }
    }

    /// Group for the blank line between Main, sound and Reverb/LED rows.
    fn group(self) -> u8 {
        match self {
            SummaryRow::Mode
            | SummaryRow::Muted
            | SummaryRow::Gain
            | SummaryRow::GainLock
            | SummaryRow::Position
            | SummaryRow::AutoGain
            | SummaryRow::TargetLevel
            | SummaryRow::Adaptation
            | SummaryRow::Phantom
            | SummaryRow::ConfigLock
            | SummaryRow::MicPreset => 0,
            SummaryRow::Tone
            | SummaryRow::Eq
            | SummaryRow::Limiter
            | SummaryRow::Compressor
            | SummaryRow::Denoiser
            | SummaryRow::PopperStopper
            | SummaryRow::MuteButton
            | SummaryRow::Hpf => 1,
            SummaryRow::ReverbOutput
            | SummaryRow::ReverbMonitor
            | SummaryRow::ReverbType
            | SummaryRow::ReverbIntensity
            | SummaryRow::LiveMeter
            | SummaryRow::NightMode => 2,
        }
    }
}

/// The Summary rows a model has, in `SummaryRow` declaration order.
fn summary_rows(model: DeviceModel) -> &'static [SummaryRow] {
    use SummaryRow as R;
    match model {
        DeviceModel::Mvx2u => &[
            R::Mode,
            R::Muted,
            R::Gain,
            R::Position,
            R::AutoGain,
            R::Phantom,
            R::ConfigLock,
            R::Tone,
            R::Eq,
            R::Limiter,
            R::Compressor,
            R::Hpf,
        ],
        DeviceModel::Mvx2uGen2 => &[
            R::Mode,
            R::Muted,
            R::Gain,
            R::GainLock,
            R::Phantom,
            R::Tone,
            R::Limiter,
            R::Compressor,
            R::Denoiser,
            R::PopperStopper,
            R::Hpf,
        ],
        DeviceModel::Mv6 => &[
            R::Mode,
            R::Muted,
            R::Gain,
            R::GainLock,
            R::Tone,
            R::Denoiser,
            R::PopperStopper,
            R::MuteButton,
            R::Hpf,
        ],
        DeviceModel::Mv6Gen2 => &[
            R::Mode,
            R::Muted,
            R::Gain,
            R::GainLock,
            R::TargetLevel,
            R::Adaptation,
            R::MicPreset,
            R::Tone,
            R::Denoiser,
            R::PopperStopper,
            R::MuteButton,
            R::Hpf,
        ],
        DeviceModel::Mv7 => &[
            R::Mode,
            R::Muted,
            R::Gain,
            R::Position,
            R::ConfigLock,
            R::Tone,
            R::Eq,
            R::Compressor,
            R::LiveMeter,
            R::NightMode,
        ],
        DeviceModel::Mv7Plus => &[
            R::Mode,
            R::Muted,
            R::Gain,
            R::Tone,
            R::Limiter,
            R::Compressor,
            R::Denoiser,
            R::PopperStopper,
            R::MuteButton,
            R::Hpf,
            R::ReverbOutput,
            R::ReverbMonitor,
            R::ReverbType,
            R::ReverbIntensity,
        ],
    }
}

/// Whether Auto Level takes this setting over on the model, so the Summary
/// shows "auto" instead of the stored value.
fn summary_auto_managed(model: DeviceModel, row: SummaryRow) -> bool {
    match model {
        DeviceModel::Mvx2u => matches!(
            row,
            SummaryRow::Eq | SummaryRow::Hpf | SummaryRow::Limiter | SummaryRow::Compressor
        ),
        DeviceModel::Mvx2uGen2 => matches!(row, SummaryRow::Limiter | SummaryRow::Compressor),
        DeviceModel::Mv7 => matches!(row, SummaryRow::Eq | SummaryRow::Compressor),
        DeviceModel::Mv6 | DeviceModel::Mv6Gen2 | DeviceModel::Mv7Plus => false,
    }
}

/// The Tone slider's value: "Natural", or how far it leans either way.
fn tone_label(tone: i8) -> String {
    let pct = i32::from(tone) * 10;
    if pct < 0 {
        format!("{}% Dark", pct.abs())
    } else if pct > 0 {
        format!("{pct}% Bright")
    } else {
        "Natural".to_string()
    }
}

/// A row's value, or None when the row doesn't apply in the current mode.
fn summary_value(app: &App, row: SummaryRow) -> Option<Span<'static>> {
    let ds = &app.device_state;
    let model = app.device_model;
    let manual = ds.mode == InputMode::Manual;
    let text = |value: String| Span::styled(value, Style::default().fg(C_TEXT));
    let on_off = |on: bool| {
        if on {
            Span::styled("ON", Style::default().fg(C_SUCCESS))
        } else {
            Span::styled("OFF", Style::default().fg(C_DIM))
        }
    };
    let lock = |locked: bool| {
        if locked {
            Span::styled("LOCKED", Style::default().fg(C_WARN))
        } else {
            Span::styled("Unlocked", Style::default().fg(C_TEXT))
        }
    };
    if !manual && summary_auto_managed(model, row) {
        return Some(Span::styled("auto", Style::default().fg(C_DISABLED)));
    }
    let span = match row {
        SummaryRow::Mode => text(ds.mode.to_string()),
        SummaryRow::Muted => {
            if ds.muted {
                Span::styled("YES", Style::default().fg(C_ERROR))
            } else {
                Span::styled("NO", Style::default().fg(C_DIM))
            }
        }
        SummaryRow::Gain if manual => text(format_gain(ds.gain_tenths)),
        SummaryRow::GainLock if manual => lock(ds.mv6_gain_locked),
        SummaryRow::Position if !manual => text(ds.auto_position.to_string()),
        SummaryRow::AutoGain if !manual => text(ds.auto_gain.to_string()),
        SummaryRow::TargetLevel if !manual => text(ds.auto_gain.target_level_label().to_string()),
        SummaryRow::Adaptation if !manual => text(
            if ds.adaptation_rate_fast {
                "Fast"
            } else {
                "Slow"
            }
            .to_string(),
        ),
        SummaryRow::Gain
        | SummaryRow::GainLock
        | SummaryRow::Position
        | SummaryRow::AutoGain
        | SummaryRow::TargetLevel
        | SummaryRow::Adaptation => return None,
        SummaryRow::Phantom => {
            if ds.phantom_power {
                Span::styled("48V ON", Style::default().fg(C_SUCCESS))
            } else {
                Span::styled("OFF", Style::default().fg(C_DIM))
            }
        }
        SummaryRow::ConfigLock => lock(ds.locked),
        SummaryRow::MicPreset => text(
            MicPreset::matching(ds)
                .map_or("Custom", MicPreset::label)
                .to_string(),
        ),
        SummaryRow::Tone => match model {
            // Auto Level's Dark / Natural / Bright; manual mode has the EQ instead.
            DeviceModel::Mvx2u | DeviceModel::Mv7 if manual => return None,
            DeviceModel::Mvx2u | DeviceModel::Mv7 => text(ds.auto_tone.to_string()),
            // The Gen 2's Tone slider is an Auto Level control; manual has the EQ.
            DeviceModel::Mvx2uGen2 if manual => return None,
            DeviceModel::Mv6Gen2 if ds.tone_advanced => text("5-band EQ".to_string()),
            DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => text(tone_label(ds.tone)),
        },
        SummaryRow::Eq => match model {
            DeviceModel::Mv7 => text(ds.eq_preset.to_string()),
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => {
                if ds.eq_enabled {
                    Span::styled("Enabled", Style::default().fg(C_SUCCESS))
                } else {
                    Span::styled("Bypass", Style::default().fg(C_DIM))
                }
            }
        },
        SummaryRow::Limiter => on_off(ds.limiter_enabled),
        SummaryRow::Compressor => text(ds.compressor.to_string()),
        SummaryRow::Denoiser => {
            if ds.denoiser_enabled && model.has_denoiser_level() {
                Span::styled(
                    format!("ON ({}%)", ds.denoiser_level),
                    Style::default().fg(C_SUCCESS),
                )
            } else {
                on_off(ds.denoiser_enabled)
            }
        }
        SummaryRow::PopperStopper => on_off(ds.popper_stopper_enabled),
        SummaryRow::MuteButton => mute_btn_span(ds.mute_btn_disabled),
        SummaryRow::Hpf => text(ds.hpf.to_string()),
        SummaryRow::ReverbOutput => on_off(ds.reverb_on_output),
        SummaryRow::ReverbMonitor => on_off(ds.reverb_monitoring),
        SummaryRow::ReverbType => text(ds.reverb_type.to_string()),
        SummaryRow::ReverbIntensity => text(format!("{}%", ds.reverb_intensity)),
        SummaryRow::LiveMeter => on_off(ds.led_live_meter),
        SummaryRow::NightMode => on_off(ds.led_night_mode),
    };
    Some(span)
}

fn draw_main_right(f: &mut UiFrame, app: &App, area: Rect) {
    let mut lines = vec![Line::from("")];
    let mut last_group = None;
    for &row in summary_rows(app.device_model) {
        let Some(value) = summary_value(app, row) else {
            continue;
        };
        if last_group.is_some_and(|group| group != row.group()) {
            lines.push(Line::from(""));
        }
        last_group = Some(row.group());
        lines.push(Line::from(vec![
            Span::styled(row.label(), Style::default().fg(C_DIM)),
            value,
        ]));
    }

    let p = Paragraph::new(lines)
        .block(card_block("Summary", false))
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
        // Clamp into the row so the end labels ("-60", "0") sit flush with its edges.
        let col_start = (center - half)
            .min(inner_width as isize - label.len() as isize)
            .max(0) as usize;
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

fn draw_meter(f: &mut UiFrame, app: &App, area: Rect) {
    use crate::meter::{METER_FLOOR_DB, METER_SILENT};

    // ── Read from rolling windows ─────────────────────────────────────────────
    // Both values come from the shared PeakWindow; fall back to METER_SILENT
    // when the lock is unavailable (should never happen in practice) or the
    // window holds nothing recent (no callbacks yet, or the stream stopped).
    let (short_raw, long_raw) = if app.demo_mode {
        (METER_SILENT, METER_SILENT)
    } else {
        let now = std::time::Instant::now();
        match app.peak_window.try_lock() {
            Ok(pw) => (
                pw.short.max(now).unwrap_or(METER_SILENT),
                pw.long.max(now).unwrap_or(METER_SILENT),
            ),
            Err(_) => (METER_SILENT, METER_SILENT),
        }
    };

    // ── Silent / no-device state ──────────────────────────────────────────────
    if short_raw == METER_SILENT {
        let label = if app.demo_mode { "no device" } else { "---" };
        let empty = Gauge::default()
            .block(focus_block("Level", false))
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
        .block(focus_block("Level", false))
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
    .block(focus_border(false));
    f.render_widget(readout, cols[1]);

    // ── dB ruler ─────────────────────────────────────────────────────────────
    // The ruler sits in rows[1] flush below the bar border. Its inner width
    // must match the bar's inner width: cols[0].width minus 2 border chars,
    // then indented by 1 to align with the bar's left border.
    let bar_inner_width = cols[0].width.saturating_sub(2) as usize;
    let ruler_str = build_ruler(bar_inner_width);

    let ruler = Paragraph::new(Line::from(Span::styled(
        ruler_str,
        Style::default().fg(C_DIM),
    )))
    // 1-char left margin aligns the ruler under the bar (skips the left border char).
    .block(Block::default().padding(ratatui::widgets::Padding::new(1, 0, 0, 0)));
    f.render_widget(ruler, rows[1]);
}

// ─────────────────────────────────────────────────────────────────────────────
// MV6 / MV7+ EQ Tab — Tone control
// ─────────────────────────────────────────────────────────────────────────────
fn draw_mv6_eq_tab(f: &mut UiFrame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .horizontal_margin(1)
        .split(area);
    draw_tone_gauge(f, app, rows[0]);
}

// MV6 Gen 2 EQ: Tone Mode on top, then the Tone slider (Simple) or the 5-band
// editor shared with the MVX2U Gen 2 (Advanced), as MOTIV lays it out.
fn draw_mv6_gen2_eq_tab(f: &mut UiFrame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .horizontal_margin(1)
        .split(area);
    draw_tone_mode_block(f, app, rows[0]);
    if app.device_state.tone_advanced {
        draw_eq_bands(f, app, rows[1]);
    } else {
        draw_tone_gauge(
            f,
            app,
            rows[1].intersection(Rect {
                height: 3,
                ..rows[1]
            }),
        );
    }
}

/// The MV6 Gen 2 Simple / Advanced switch, styled like the Input Mode block.
fn draw_tone_mode_block(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::ToneMode);
    let focused = app.focus == Focus::ToneMode;
    let advanced = app.device_state.tone_advanced;
    let p = Paragraph::new(Line::from(vec![
        Span::styled("Tone:  ", Style::default().fg(C_DIM)),
        segmented_span("Simple", !advanced),
        segmented_span("Advanced (5-band EQ)", advanced),
        Span::styled(
            if focused { "  [Enter] toggle" } else { "" },
            Style::default().fg(C_DIM),
        ),
    ]))
    .block(focus_block("Tone Mode", focused));
    f.render_widget(p, area);
}

/// The Tone slider (Dark ↔ Natural ↔ Bright), drawn into a 3-row `area`.
fn draw_tone_gauge(f: &mut UiFrame, app: &App, area: Rect) {
    let ds = &app.device_state;
    let tone_foc = app.focus == Focus::Tone;
    let tone = ds.tone;
    let pct = tone as i32 * 10;
    let tone_label = tone_label(tone);
    let tone_ratio = (tone as f64 + 10.0) / 20.0;

    let tone_gauge = slider_gauge("Tone", &tone_label, tone_foc, C_BLUE)
        .ratio(tone_ratio)
        .label(format!("Dark ◄─{:+}─► Bright", pct));
    f.control(area, Focus::Tone);
    f.render_widget(tone_gauge, area);
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared Dynamics / Reverb card helpers
//
// Every model's Dynamics tab is assembled from the same two card shapes, so the
// labels, wording, and focus styling live here once instead of being repeated
// in each per-model draw_*_dynamics_tab(). Adding a model means calling these,
// never copying them.
// ─────────────────────────────────────────────────────────────────────────────

/// The bordered frame shared by every Dynamics/Reverb card.
fn card_block(title: &str, focused: bool) -> Block<'static> {
    focus_block(title, focused).padding(Padding::horizontal(1))
}

/// Card for a boolean control toggled with Enter.
///
/// `status_label` is the text in front of the ON/OFF span ("Status: ",
/// "On output: ", …); `description` wraps to the card width.
fn draw_toggle_card(
    f: &mut UiFrame,
    focused: bool,
    title: &str,
    status_label: &str,
    value: bool,
    description: &str,
    area: Rect,
) {
    let status = vec![toggle_status_line(status_label, value)];
    let card = StatusCard {
        title,
        status,
        description,
        hint: "[Enter] toggle",
    };
    draw_status_card(f, focused, card, area);
}

fn toggle_status_line(label: &str, value: bool) -> Line<'_> {
    Line::from(vec![
        Span::styled(label, Style::default().fg(C_DIM)),
        bool_span(value),
    ])
}

/// The parts of a Dynamics card: its status lines (current values), a dim
/// description, and the key hint, which takes the focus colour when focused.
struct StatusCard<'a> {
    title: &'a str,
    status: Vec<Line<'a>>,
    description: &'a str,
    hint: &'a str,
}

fn draw_status_card(f: &mut UiFrame, focused: bool, card: StatusCard, area: Rect) {
    let mut lines = vec![Line::from("")];
    lines.extend(card.status);
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        card.description,
        Style::default().fg(C_DIM),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        card.hint,
        Style::default().fg(if focused { C_FOCUS } else { C_DIM }),
    )));

    f.render_widget(
        Paragraph::new(lines)
            .block(card_block(card.title, focused))
            .wrap(Wrap { trim: true }),
        area,
    );
}

/// Card for an enum control cycled with Enter: one row per variant, the current
/// one marked with ▶. `options` pairs each label with whether it is selected.
fn draw_enum_card(
    f: &mut UiFrame,
    focused: bool,
    title: &str,
    options: &[(String, bool)],
    area: Rect,
) {
    let mut lines = vec![Line::from("")];
    lines.extend(options.iter().map(|(label, selected)| {
        Line::from(vec![
            Span::styled(
                if *selected { "▶ " } else { "  " },
                Style::default().fg(if focused { C_FOCUS } else { C_TEXT }),
            ),
            Span::styled(
                label.as_str(),
                if *selected {
                    Style::default()
                        .fg(if focused { C_FOCUS } else { C_TEXT })
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(C_DIM)
                },
            ),
        ])
    }));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "[Enter] cycle",
        Style::default().fg(if focused { C_FOCUS } else { C_DIM }),
    )));

    f.render_widget(
        Paragraph::new(lines).block(card_block(title, focused)),
        area,
    );
}

/// Label/selected pairs for a set of enum variants, in display order.
fn enum_options<T: PartialEq + ToString>(variants: &[T], current: T) -> Vec<(String, bool)> {
    variants
        .iter()
        .map(|variant| (variant.to_string(), *variant == current))
        .collect()
}

// ── The individual cards, shared by every model that has the control ──────────

fn draw_denoiser_card(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::Denoiser);
    let ds = &app.device_state;
    let focused = app.focus == Focus::Denoiser;
    let description = "Reduces background noise in real time.";
    if !app.device_model.has_denoiser_level() {
        draw_toggle_card(
            f,
            focused,
            "Denoiser",
            "Status: ",
            ds.denoiser_enabled,
            description,
            area,
        );
        return;
    }

    // MV6 Gen 2: ←/→ sets the level. It reads as a value like Status, dimmed
    // while the Denoiser is off (the mic keeps it for when it is back on).
    // The 10-cell bar fits the narrowest card (80 columns).
    let level = ds.denoiser_level.min(100);
    let filled = usize::from(level.div_ceil(10));
    let (value_style, bar_style) = if ds.denoiser_enabled {
        (
            Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD),
            Style::default().fg(C_ACCENT),
        )
    } else {
        (Style::default().fg(C_DIM), Style::default().fg(C_DISABLED))
    };
    let status = vec![
        toggle_status_line("Status: ", ds.denoiser_enabled),
        Line::from(vec![
            Span::styled("Level:  ", Style::default().fg(C_DIM)),
            Span::styled(format!("{level}%"), value_style),
            // Shown next to the value, like the Tone slider's hint, so it
            // fits the narrowest card.
            Span::styled(
                if focused { " ◄ ►" } else { "" },
                Style::default().fg(C_FOCUS),
            ),
        ]),
        Line::from(vec![
            Span::styled("█".repeat(filled), bar_style),
            Span::styled("░".repeat(10 - filled), Style::default().fg(C_DISABLED)),
        ]),
    ];
    let card = StatusCard {
        title: "Denoiser",
        status,
        description,
        hint: "[Enter] toggle",
    };
    draw_status_card(f, focused, card, area);
}

fn draw_popper_stopper_card(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::PopperStopper);
    draw_toggle_card(
        f,
        app.focus == Focus::PopperStopper,
        "Popper Stopper",
        "Status: ",
        app.device_state.popper_stopper_enabled,
        "Reduces plosive sounds (p, b, t).",
        area,
    );
}

/// Says what pressing the physical button does, never ON/OFF or Enabled:
/// "Mute Button: ON" reads as the mic being muted.
fn draw_mute_btn_card(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::MuteBtnDisable);
    let card = StatusCard {
        title: "Mute Button",
        status: vec![Line::from(vec![
            Span::styled("Pressing it: ", Style::default().fg(C_DIM)),
            mute_btn_span(app.device_state.mute_btn_disabled),
        ])],
        description: "Whether pressing the mic's mute button mutes it.",
        hint: "[Enter] toggle",
    };
    draw_status_card(f, app.focus == Focus::MuteBtnDisable, card, area);
}

/// The physical mute button's state for the card and the Summary.
fn mute_btn_span(disabled: bool) -> Span<'static> {
    if disabled {
        Span::styled("Ignored", Style::default().fg(C_WARN))
    } else {
        Span::styled("Works", Style::default().fg(C_TEXT))
    }
}

fn draw_limiter_card(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::Limiter);
    draw_toggle_card(
        f,
        app.focus == Focus::Limiter,
        "Limiter",
        "Status: ",
        app.device_state.limiter_enabled,
        "Prevents clipping by capping the output level at 0 dBFS.",
        area,
    );
}

fn draw_compressor_card(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::Compressor);
    let options = enum_options(
        &[
            CompressorPreset::Off,
            CompressorPreset::Light,
            CompressorPreset::Medium,
            CompressorPreset::Heavy,
        ],
        app.device_state.compressor,
    );
    draw_enum_card(
        f,
        app.focus == Focus::Compressor,
        "Compressor",
        &options,
        area,
    );
}

fn draw_hpf_card(f: &mut UiFrame, app: &App, area: Rect) {
    f.control(area, Focus::Hpf);
    let options = enum_options(
        &[HpfFrequency::Off, HpfFrequency::Hz75, HpfFrequency::Hz150],
        app.device_state.hpf,
    );
    draw_enum_card(
        f,
        app.focus == Focus::Hpf,
        "High-Pass Filter",
        &options,
        area,
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// MV6 / MV6 Gen 2 Dynamics Tab — Denoiser, Popper Stopper, Mute Button, HPF
// ─────────────────────────────────────────────────────────────────────────────
fn draw_mv6_dynamics_tab(f: &mut UiFrame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, 4); 4])
        .horizontal_margin(1)
        .split(area);

    draw_denoiser_card(f, app, cols[0]);
    draw_popper_stopper_card(f, app, cols[1]);
    draw_mute_btn_card(f, app, cols[2]);
    draw_hpf_card(f, app, cols[3]);
}

// ─────────────────────────────────────────────────────────────────────────────
// MVX2U Gen 2 EQ Tab — same visual style as Gen 1, no master enable, no per-band enable
// ─────────────────────────────────────────────────────────────────────────────
fn draw_gen2_eq_tab(f: &mut UiFrame, app: &App, area: Rect) {
    // Gen 2: Auto mode shows the Tone slider; Manual mode shows the 5-band EQ.
    if app.device_state.mode == InputMode::Auto {
        draw_mv6_eq_tab(f, app, area);
        return;
    }
    draw_eq_bands(f, app, area.inner(Margin::new(1, 1)));
}

/// The "Selected Band: Band N (freq)" header text shared by both EQ layouts.
fn selected_band_spans(app: &App, focused: bool) -> Vec<Span<'static>> {
    let freq_label = format_band_freq(app.eq_selected_band);
    vec![
        Span::styled(
            "Selected Band:  ",
            Style::default().fg(if focused { C_FOCUS } else { C_DIM }),
        ),
        Span::styled(
            format!("Band {} ", app.eq_selected_band + 1),
            Style::default()
                .fg(if focused { C_FOCUS } else { C_TEXT })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("({freq_label})"), Style::default().fg(C_DIM)),
        Span::styled(
            if focused {
                "  ◄ ► to change band"
            } else {
                ""
            },
            Style::default().fg(C_DIM),
        ),
    ]
}

fn format_band_freq(band: usize) -> String {
    let freq = EQ_BAND_FREQS[band];
    if freq >= 1000 {
        format!("{}k Hz", freq / 1000)
    } else {
        format!("{freq} Hz")
    }
}

/// Rows above an EQ band's text: the gain bar (8) and a blank row.
const EQ_BAR_ROWS: u16 = 9;

/// One EQ band column: gain bar, frequency, the On row when `enabled` is given
/// (the MVX2U Gen 1, at row `EQ_BAR_ROWS + 1`), and gain.
fn eq_band_column(
    app: &App,
    band: usize,
    enabled: Option<bool>,
    focused: bool,
) -> Paragraph<'static> {
    let eq = &app.device_state.eq_bands[band];
    let gain_foc = app.focus == Focus::EqGain(band);
    // Boosts and cuts share the accent; a flat band stays dim.
    let gain_color = if eq.gain_db == 0 { C_DIM } else { C_ACCENT };

    // Vertical gain bar: −8.0 dB = bottom (row 0), +6.0 dB = top (row 7).
    // gain_db is in tenths, so the 14 dB span is −80..+60.
    let bar_height = EQ_BAR_ROWS as i32 - 2;
    let bar_pos = ((eq.gain_db as i32 + 80) * bar_height / 140).clamp(0, bar_height);
    let mut lines: Vec<Line> = (0..=bar_height)
        .rev()
        .map(|row| {
            Line::from(if row == bar_pos {
                Span::styled("━━━━", Style::default().fg(gain_color))
            } else if row == bar_height / 2 {
                Span::styled("────", Style::default().fg(C_BORDER))
            } else {
                Span::styled("    ", Style::default())
            })
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Freq: ", Style::default().fg(C_DIM)),
        Span::styled(format_band_freq(band), Style::default().fg(C_TEXT)),
    ]));
    if let Some(enabled) = enabled {
        let en_foc = app.focus == Focus::EqBandEnable(band);
        lines.push(Line::from(vec![
            Span::styled(
                "On:   ",
                Style::default().fg(if en_foc { C_FOCUS } else { C_DIM }),
            ),
            bool_span(enabled),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled(
            "Gain: ",
            Style::default().fg(if gain_foc { C_FOCUS } else { C_DIM }),
        ),
        Span::styled(
            format!("{:+.1} dB", eq.gain_db as f32 / 10.0),
            if gain_foc {
                Style::default().fg(C_FOCUS).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(gain_color)
            },
        ),
    ]));
    Paragraph::new(lines)
        .block(eq_band_block(band, band == app.eq_selected_band, focused))
        .style(Style::default().bg(C_BG))
}

/// One EQ band column's frame; the selected band gets a thick border.
fn eq_band_block(band: usize, selected: bool, focused: bool) -> Block<'static> {
    let border_color = if focused {
        C_FOCUS
    } else if selected {
        C_TEXT
    } else {
        C_BORDER
    };
    Block::default()
        .title(Span::styled(
            format!(" Band {} ", band + 1),
            if selected {
                Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD)
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
        .padding(Padding::horizontal(1))
}

/// The 5-band EQ editor (band selector + one column per band) without master or
/// per-band enables. Shared by the MVX2U Gen 2 and the MV6 Gen 2 (Advanced).
fn draw_eq_bands(f: &mut UiFrame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // band selector header
            Constraint::Min(0),    // band columns
        ])
        .split(area);

    f.control(chunks[0], Focus::EqBandSelect);

    // ── Header: band selector only (no master enable) ─────────────────────────
    let band_sel_focused = app.focus == Focus::EqBandSelect;
    let header = Paragraph::new(Line::from(selected_band_spans(app, band_sel_focused)))
        .block(focus_border(band_sel_focused));
    f.render_widget(header, chunks[0]);

    // ── Band columns ──────────────────────────────────────────────────────────
    let band_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Percentage(20); 5])
        .split(chunks[1]);

    for (i, band_area) in band_cols.iter().enumerate() {
        f.control(*band_area, Focus::EqGain(i));
        let focused = app.focus == Focus::EqGain(i);
        f.render_widget(eq_band_column(app, i, None, focused), *band_area);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// MVX2U Gen 2 Dynamics Tab — Limiter + Compressor (Gen 1 style) + Denoiser + Popper Stopper + HPF
// ─────────────────────────────────────────────────────────────────────────────
fn draw_gen2_dynamics_tab(f: &mut UiFrame, app: &App, area: Rect) {
    // Auto mode: only Denoiser, Popper Stopper, HPF are available.
    // Manual mode: all five controls.
    let show_limiter_comp = app.device_state.mode != InputMode::Auto;
    let num_cols = if show_limiter_comp { 5usize } else { 3usize };

    let constraints: Vec<Constraint> = (0..num_cols)
        .map(|_| Constraint::Ratio(1, num_cols as u32))
        .collect();

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .horizontal_margin(1)
        .split(area);

    let mut col = 0usize;

    if show_limiter_comp {
        // Limiter and Compressor render exactly as they do on Gen 1.
        draw_limiter_card(f, app, cols[col]);
        col += 1;
        draw_compressor_card(f, app, cols[col]);
        col += 1;
    }

    draw_denoiser_card(f, app, cols[col]);
    col += 1;
    draw_popper_stopper_card(f, app, cols[col]);
    col += 1;
    draw_hpf_card(f, app, cols[col]);
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab locked / not-available notices
// ─────────────────────────────────────────────────────────────────────────────

/// Shown on EQ/Dynamics when an MVX2U Gen 1 or MV7 is in Auto Level mode.
fn draw_tab_locked_notice(f: &mut UiFrame, area: Rect, tab_name: &str) {
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

fn render_notice(f: &mut UiFrame, area: Rect, lines: Vec<Line>) {
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
fn draw_eq_tab(f: &mut UiFrame, app: &App, area: Rect) {
    if app.device_model == DeviceModel::Mv6Gen2 {
        draw_mv6_gen2_eq_tab(f, app, area);
        return;
    }
    if matches!(app.device_model, DeviceModel::Mv6 | DeviceModel::Mv7Plus) {
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
    if app.device_model == DeviceModel::Mv7 {
        draw_mv7_eq_tab(f, app, area);
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // master enable + band selector
            Constraint::Min(0),    // band columns
        ])
        .horizontal_margin(1)
        .split(area);

    f.control(chunks[0], Focus::EqBandSelect);
    let header_inner = chunks[0].inner(Margin::new(1, 1));
    // "EQ:  " followed by the five-cell ON/OFF span.
    f.control(
        Rect::new(header_inner.x, header_inner.y, 10, 1).intersection(header_inner),
        Focus::EqEnable,
    );

    // ── EQ master enable header ───────────────────────────────────────────────
    let eq_en_focused = app.focus == Focus::EqEnable;
    let band_sel_focused = app.focus == Focus::EqBandSelect;
    let mut spans = vec![
        Span::styled("EQ:  ", Style::default().fg(C_DIM)),
        bool_span(app.device_state.eq_enabled),
        Span::styled("   ", Style::default()),
    ];
    spans.extend(selected_band_spans(app, band_sel_focused));
    let header =
        Paragraph::new(Line::from(spans)).block(focus_border(eq_en_focused || band_sel_focused));
    f.render_widget(header, chunks[0]);

    // ── Band columns ──────────────────────────────────────────────────────────
    let band_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Percentage(20); 5])
        .split(chunks[1]);

    for (i, (band, band_area)) in app
        .device_state
        .eq_bands
        .iter()
        .zip(band_cols.iter())
        .enumerate()
    {
        f.control(*band_area, Focus::EqGain(i));
        // The enable row sits below the bar and the frequency.
        let band_inner = band_area.inner(Margin::new(2, 1));
        f.control(
            text_row(band_inner, EQ_BAR_ROWS + 1),
            Focus::EqBandEnable(i),
        );
        let focused = app.focus == Focus::EqBandEnable(i) || app.focus == Focus::EqGain(i);
        f.render_widget(
            eq_band_column(app, i, Some(band.enabled), focused),
            *band_area,
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// MV7+ Dynamics Tab — Limiter, Compressor, Denoiser, Popper Stopper, Mute Button, HPF
// ─────────────────────────────────────────────────────────────────────────────
fn draw_mv7plus_dynamics_tab(f: &mut UiFrame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, 6); 6])
        .horizontal_margin(1)
        .split(area);

    draw_limiter_card(f, app, cols[0]);
    draw_compressor_card(f, app, cols[1]);
    draw_denoiser_card(f, app, cols[2]);
    draw_popper_stopper_card(f, app, cols[3]);
    draw_mute_btn_card(f, app, cols[4]);
    draw_hpf_card(f, app, cols[5]);
}

// ─────────────────────────────────────────────────────────────────────────────
// Reverb Tab (MV7+ only)
// ─────────────────────────────────────────────────────────────────────────────
fn draw_reverb_tab(f: &mut UiFrame, app: &App, area: Rect) {
    let ds = &app.device_state;

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .horizontal_margin(1)
        .split(area);

    let rev_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, 3); 3])
        .split(rows[0]);

    f.control(rev_cols[0], Focus::ReverbOutput);
    f.control(rev_cols[1], Focus::ReverbMonitor);
    f.control(rev_cols[2], Focus::ReverbPreset);
    f.control(rows[1], Focus::ReverbIntensity);

    draw_toggle_card(
        f,
        app.focus == Focus::ReverbOutput,
        "Reverb Output",
        "On output: ",
        ds.reverb_on_output,
        "Applies reverb to speaker/headphones.",
        rev_cols[0],
    );

    draw_toggle_card(
        f,
        app.focus == Focus::ReverbMonitor,
        "Reverb Monitor",
        "Monitoring: ",
        ds.reverb_monitoring,
        "Applies reverb to direct monitor mix.",
        rev_cols[1],
    );

    let reverb_types = enum_options(
        &[ReverbType::Plate, ReverbType::Hall, ReverbType::Studio],
        ds.reverb_type,
    );
    draw_enum_card(
        f,
        app.focus == Focus::ReverbPreset,
        "Reverb Type",
        &reverb_types,
        rev_cols[2],
    );

    // Reverb Intensity — full-width bar across the bottom
    let rint_foc = app.focus == Focus::ReverbIntensity;
    let intensity = ds.reverb_intensity;
    let rint_gauge = slider_gauge(
        "Reverb Intensity",
        &format!("{intensity}%"),
        rint_foc,
        C_BLUE,
    )
    .ratio(intensity as f64 / 100.0)
    .label(format!("Dry ◄─{:3}%─► Wet", intensity));
    f.render_widget(rint_gauge, rows[1]);
}

// ─────────────────────────────────────────────────────────────────────────────
// LED Tab (MV7+ only)
// ─────────────────────────────────────────────────────────────────────────────

fn draw_led_color_gauge(
    f: &mut UiFrame,
    app: &App,
    focus: Focus,
    value: u8,
    label: &str,
    color: Color,
    area: Rect,
) {
    f.control(area, focus);
    let focused = app.focus == focus;
    let gauge = slider_gauge(label, &value.to_string(), focused, color)
        .ratio(value as f64 / 255.0)
        .label(format!("0 ◄─{:3}─► 255", value));
    f.render_widget(gauge, area);
}

fn draw_led_cycle_row(
    f: &mut UiFrame,
    app: &App,
    focus: Focus,
    label: &str,
    value: &str,
    area: Rect,
) {
    f.control(area, focus);
    let focused = app.focus == focus;
    let p = Paragraph::new(Line::from(vec![
        Span::styled(format!("{label}:  "), Style::default().fg(C_DIM)),
        Span::styled(
            format!("[ {value} ]"),
            Style::default()
                .fg(if focused { C_FOCUS } else { C_TEXT })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if focused { "  [Enter] cycle" } else { "" },
            Style::default().fg(C_DIM),
        ),
    ]))
    .block(card_block(label, focused));
    f.render_widget(p, area);
}

fn draw_led_tab(f: &mut UiFrame, app: &App, area: Rect) {
    if app.device_model == DeviceModel::Mv7 {
        draw_mv7_led_tab(f, app, area);
        return;
    }
    let ds = &app.device_state;

    let custom_color_zones: usize = match ds.led_behavior {
        LedBehavior::Solid if ds.led_solid_theme == LedSolidTheme::Custom => 1,
        LedBehavior::Pulsing if ds.led_pulsing_theme == LedPulsingTheme::Custom => 1,
        LedBehavior::Live if ds.led_live_theme == LedLiveTheme::Custom => 3,
        _ => 0,
    };

    // 3 control rows + one 3-line row per color zone
    let color_rows_height = custom_color_zones as u16 * 3;
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),                 // Behavior
            Constraint::Length(3),                 // Brightness
            Constraint::Length(3),                 // Theme
            Constraint::Length(color_rows_height), // color zones (0 if not Custom)
            Constraint::Min(0),                    // padding
        ])
        .horizontal_margin(1)
        .split(area);

    // Behavior row
    draw_led_cycle_row(
        f,
        app,
        Focus::LedBehavior,
        "Behavior",
        &ds.led_behavior.to_string(),
        sections[0],
    );

    // Brightness row
    draw_led_cycle_row(
        f,
        app,
        Focus::LedBrightness,
        "Brightness",
        &ds.led_brightness.to_string(),
        sections[1],
    );

    // Theme row (label and options are context-sensitive)
    let theme_str = match ds.led_behavior {
        LedBehavior::Live => ds.led_live_theme.to_string(),
        LedBehavior::Solid => ds.led_solid_theme.to_string(),
        LedBehavior::Pulsing => ds.led_pulsing_theme.to_string(),
    };
    draw_led_cycle_row(f, app, Focus::LedTheme, "Theme", &theme_str, sections[2]);

    // Color zone rows — only rendered when Custom theme is active
    if custom_color_zones == 0 {
        return;
    }

    let zone_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(3); custom_color_zones])
        .split(sections[3]);

    match ds.led_behavior {
        LedBehavior::Solid => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(vec![Constraint::Ratio(1, 3); 3])
                .split(zone_areas[0]);
            draw_led_color_gauge(
                f,
                app,
                Focus::LedSolidR,
                ds.led_solid_rgb[0],
                "Solid R",
                Color::Rgb(220, 60, 60),
                cols[0],
            );
            draw_led_color_gauge(
                f,
                app,
                Focus::LedSolidG,
                ds.led_solid_rgb[1],
                "Solid G",
                Color::Rgb(60, 200, 60),
                cols[1],
            );
            draw_led_color_gauge(
                f,
                app,
                Focus::LedSolidB,
                ds.led_solid_rgb[2],
                "Solid B",
                Color::Rgb(60, 100, 220),
                cols[2],
            );
        }
        LedBehavior::Pulsing => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(vec![Constraint::Ratio(1, 3); 3])
                .split(zone_areas[0]);
            draw_led_color_gauge(
                f,
                app,
                Focus::LedPulsingR,
                ds.led_pulsing_rgb[0],
                "Pulsing R",
                Color::Rgb(220, 60, 60),
                cols[0],
            );
            draw_led_color_gauge(
                f,
                app,
                Focus::LedPulsingG,
                ds.led_pulsing_rgb[1],
                "Pulsing G",
                Color::Rgb(60, 200, 60),
                cols[1],
            );
            draw_led_color_gauge(
                f,
                app,
                Focus::LedPulsingB,
                ds.led_pulsing_rgb[2],
                "Pulsing B",
                Color::Rgb(60, 100, 220),
                cols[2],
            );
        }
        LedBehavior::Live => {
            // zone_areas[0] = edge, [1] = middle, [2] = interior
            let edge_cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(vec![Constraint::Ratio(1, 3); 3])
                .split(zone_areas[0]);
            draw_led_color_gauge(
                f,
                app,
                Focus::LedLiveEdgeR,
                ds.led_live_edge_rgb[0],
                "Edge R",
                Color::Rgb(220, 60, 60),
                edge_cols[0],
            );
            draw_led_color_gauge(
                f,
                app,
                Focus::LedLiveEdgeG,
                ds.led_live_edge_rgb[1],
                "Edge G",
                Color::Rgb(60, 200, 60),
                edge_cols[1],
            );
            draw_led_color_gauge(
                f,
                app,
                Focus::LedLiveEdgeB,
                ds.led_live_edge_rgb[2],
                "Edge B",
                Color::Rgb(60, 100, 220),
                edge_cols[2],
            );

            let mid_cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(vec![Constraint::Ratio(1, 3); 3])
                .split(zone_areas[1]);
            draw_led_color_gauge(
                f,
                app,
                Focus::LedLiveMiddleR,
                ds.led_live_middle_rgb[0],
                "Middle R",
                Color::Rgb(220, 60, 60),
                mid_cols[0],
            );
            draw_led_color_gauge(
                f,
                app,
                Focus::LedLiveMiddleG,
                ds.led_live_middle_rgb[1],
                "Middle G",
                Color::Rgb(60, 200, 60),
                mid_cols[1],
            );
            draw_led_color_gauge(
                f,
                app,
                Focus::LedLiveMiddleB,
                ds.led_live_middle_rgb[2],
                "Middle B",
                Color::Rgb(60, 100, 220),
                mid_cols[2],
            );

            let int_cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(vec![Constraint::Ratio(1, 3); 3])
                .split(zone_areas[2]);
            draw_led_color_gauge(
                f,
                app,
                Focus::LedLiveInteriorR,
                ds.led_live_interior_rgb[0],
                "Interior R",
                Color::Rgb(220, 60, 60),
                int_cols[0],
            );
            draw_led_color_gauge(
                f,
                app,
                Focus::LedLiveInteriorG,
                ds.led_live_interior_rgb[1],
                "Interior G",
                Color::Rgb(60, 200, 60),
                int_cols[1],
            );
            draw_led_color_gauge(
                f,
                app,
                Focus::LedLiveInteriorB,
                ds.led_live_interior_rgb[2],
                "Interior B",
                Color::Rgb(60, 100, 220),
                int_cols[2],
            );
        }
    }
}

/// MV7 LED panel: two switches, drawn with the same rows as the MV7+ LED tab.
fn draw_mv7_led_tab(f: &mut UiFrame, app: &App, area: Rect) {
    let ds = &app.device_state;
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Live Meter
            Constraint::Length(3), // Night Mode
            Constraint::Min(0),    // padding
        ])
        .horizontal_margin(1)
        .split(area);
    let on_off = |on: bool| if on { "ON" } else { "OFF" };

    draw_led_cycle_row(
        f,
        app,
        Focus::LedLiveMeter,
        "Live Meter",
        on_off(ds.led_live_meter),
        sections[0],
    );
    draw_led_cycle_row(
        f,
        app,
        Focus::LedNightMode,
        "Night Mode",
        on_off(ds.led_night_mode),
        sections[1],
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Dynamics Tab
// ─────────────────────────────────────────────────────────────────────────────
fn draw_dynamics_tab(f: &mut UiFrame, app: &App, area: Rect) {
    if app.device_model == DeviceModel::Mv7Plus {
        draw_mv7plus_dynamics_tab(f, app, area);
        return;
    }
    if matches!(app.device_model, DeviceModel::Mv6 | DeviceModel::Mv6Gen2) {
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
        .constraints(vec![Constraint::Ratio(1, 3); 3])
        .horizontal_margin(1)
        .split(area);

    if app.device_model == DeviceModel::Mv7 {
        // The limiter MOTIV shows for the MV7 runs in the app, not on the mic,
        // and the MV7 has no HPF; the compressor is its only dynamics control.
        draw_compressor_card(f, app, cols[0]);
        return;
    }
    draw_limiter_card(f, app, cols[0]);
    draw_compressor_card(f, app, cols[1]);
    draw_hpf_card(f, app, cols[2]);
}

// ─────────────────────────────────────────────────────────────────────────────
// MV7 EQ Tab — one four-way preset (High Pass and Presence Boost switches)
// ─────────────────────────────────────────────────────────────────────────────
fn draw_mv7_eq_tab(f: &mut UiFrame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, 3); 3])
        .horizontal_margin(1)
        .split(area);

    let options = enum_options(
        &[
            EqPreset::Flat,
            EqPreset::HighPass,
            EqPreset::PresenceBoost,
            EqPreset::HighPassPresenceBoost,
        ],
        app.device_state.eq_preset,
    );
    f.control(cols[0], Focus::EqPreset);
    draw_enum_card(f, app.focus == Focus::EqPreset, "EQ", &options, cols[0]);
}

// ─────────────────────────────────────────────────────────────────────────────
// Presets Tab
// ─────────────────────────────────────────────────────────────────────────────
fn draw_presets_tab(f: &mut UiFrame, app: &App, area: Rect) {
    // Each slot gets a fixed-height card: name row (3) + actions row (3) = 6 lines each.
    let slot_constraints: Vec<Constraint> = (0..4).map(|_| Constraint::Length(7)).collect();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(slot_constraints)
        .horizontal_margin(1)
        .split(area);

    for i in 0..4 {
        draw_preset_card(f, app, i, rows[i]);
    }
}

fn draw_preset_card(f: &mut UiFrame, app: &App, index: usize, area: Rect) {
    // Split the card into name row and actions row.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Length(4)])
        .split(area);

    draw_preset_name_row(f, app, index, rows[0]);
    draw_preset_actions_row(f, app, index, rows[1]);
}

fn draw_preset_name_row(f: &mut UiFrame, app: &App, index: usize, area: Rect) {
    f.control(area, Focus::PresetName(index));
    let focused = app.focus == Focus::PresetName(index);
    let editing = app.editing_preset_name && app.editing_preset_index == index;

    let (name_text, border_color, title_style) = match &app.presets[index] {
        Some(slot) => {
            let display = if editing {
                format!("{}_", app.name_draft) // show cursor
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
                Style::default().fg(C_DIM)
            };
            (String::from("Empty"), color, style)
        }
    };

    let hint = if editing {
        Span::styled(
            "  [Enter] save  [Esc/click outside] cancel",
            Style::default().fg(C_ACCENT),
        )
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

fn draw_preset_actions_row(f: &mut UiFrame, app: &App, index: usize, area: Rect) {
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
                Style::default().fg(if focused { C_TEXT } else { C_DIM }),
            ),
        ])
    };

    let border_color = if focused { C_FOCUS } else { C_BORDER };

    let border = Block::default()
        .borders(Borders::BOTTOM | Borders::LEFT | Borders::RIGHT)
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Rounded
        })
        .border_style(Style::default().fg(border_color))
        .padding(Padding::horizontal(1));
    let action_row = text_row(border.inner(area), 1);
    let keys: &[KeyCode] = if filled {
        &[KeyCode::Enter, KeyCode::Char('s'), KeyCode::Char('d')]
    } else {
        &[KeyCode::Char('s')]
    };
    let mut x = action_row.x;
    // Each action is a shortcut span plus a label span. Derive hit widths from
    // those same spans so Load, Save, and Delete can never share a target.
    for (spans, key) in actions.spans.as_chunks::<2>().0.iter().zip(keys) {
        let width = spans.iter().map(Span::width).sum::<usize>() as u16;
        f.hits.add(
            Rect::new(x, action_row.y, width, 1).intersection(action_row),
            Target::PresetAction(index, *key),
        );
        x = x.saturating_add(width);
    }

    let block = Paragraph::new(vec![Line::from(""), actions])
        .block(border)
        .style(Style::default().bg(C_BG));
    f.render_widget(block, area);
}

// ─────────────────────────────────────────────────────────────────────────────
// Info Tab
// ─────────────────────────────────────────────────────────────────────────────
fn draw_info_tab(f: &mut UiFrame, app: &App, area: Rect) {
    let ds = &app.device_state;
    let model = app.device_model;
    let border = focus_border(false).padding(Padding::horizontal(1));

    let (vid_pid, gain_range) = match model {
        DeviceModel::Mvx2u => ("14ED:1013", "0–60 dB"),
        DeviceModel::Mvx2uGen2 => ("14ED:1033", "0–60 dB"),
        DeviceModel::Mv6 => ("14ED:1026", "0–36 dB"),
        DeviceModel::Mv6Gen2 => ("14ED:1035", "0–36 dB"),
        DeviceModel::Mv7 => ("14ED:1012", "0–36 dB (1.5 dB steps)"),
        DeviceModel::Mv7Plus => ("14ED:1019", "0–36 dB"),
    };

    let mut lines = vec![
        Line::from(Span::styled(
            "  Device Information",
            Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD),
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
    ];

    // Device name (user-set) sits under Model so the panel reads "what it is and
    // what you call it", then the device-reported identifiers below. Name and
    // firmware are read over HID from the lock-class identity features; each is
    // shown only when the device reported it (otherwise the field stays "Unknown"
    // and the row is hidden), so we never display an empty field.
    // Where renaming works the row is always shown, so a device with no name
    // can be given one.
    if model.has_device_rename() {
        let focused = app.focus == Focus::DeviceName;
        let editing = app.editing_device_name;
        f.control(
            text_row(border.inner(area), lines.len() as u16),
            Focus::DeviceName,
        );
        let (name, name_style) = if editing {
            (
                format!("{}_", app.name_draft), // show cursor
                Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
            )
        } else {
            (ds.device_name.clone(), focused_style(focused))
        };
        let hint = if editing {
            Span::styled(
                "  [Enter] save  [Esc/click outside] cancel",
                Style::default().fg(C_ACCENT),
            )
        } else if focused {
            Span::styled("  [Enter] rename", Style::default().fg(C_DIM))
        } else {
            Span::raw("")
        };
        lines.push(Line::from(vec![
            Span::styled("  Device Name  : ", Style::default().fg(C_DIM)),
            Span::styled(name, name_style),
            hint,
        ]));
    } else if ds.device_name != "Unknown" {
        lines.push(Line::from(vec![
            Span::styled("  Device Name  : ", Style::default().fg(C_DIM)),
            Span::styled(&*ds.device_name, Style::default().fg(C_TEXT)),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("  Serial No.   : ", Style::default().fg(C_DIM)),
        Span::styled(ds.display_serial(), Style::default().fg(C_TEXT)),
    ]));

    if ds.firmware_version != "Unknown" {
        lines.push(Line::from(vec![
            Span::styled("  Firmware     : ", Style::default().fg(C_DIM)),
            Span::styled(&*ds.firmware_version, Style::default().fg(C_TEXT)),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("  USB VID/PID  : ", Style::default().fg(C_DIM)),
        Span::styled(vid_pid, Style::default().fg(C_TEXT)),
    ]));

    // Device Actions before Capabilities: the long static list must not push
    // the only button on this tab off a short terminal.
    if app.supports_factory_reset() {
        let reset_focused = app.focus == Focus::FactoryReset;
        let (reset_label, reset_style) = if app.confirming_factory_reset {
            (
                "  [ Factory Reset ]  ← Press Enter to confirm",
                Style::default()
                    .fg(Color::Black)
                    .bg(C_ERROR)
                    .add_modifier(Modifier::BOLD),
            )
        } else if reset_focused {
            (
                "  [ Factory Reset ]",
                Style::default()
                    .fg(Color::Black)
                    .bg(C_ERROR)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("  [ Factory Reset ]", Style::default().fg(C_ERROR))
        };
        let reset_row = text_row(border.inner(area), lines.len() as u16 + 3);
        f.control(
            Rect::new(reset_row.x, reset_row.y, reset_label.len() as u16, 1)
                .intersection(reset_row),
            Focus::FactoryReset,
        );
        lines.extend([
            Line::from(""),
            Line::from(Span::styled(
                "  Device Actions",
                Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(reset_label, reset_style)),
            Line::from(Span::styled(
                "    Resets all settings to factory defaults. Device will disconnect.",
                Style::default().fg(C_DIM),
            )),
        ]);
    }

    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "  Capabilities",
            Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD),
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
    ]);

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
            ("  Monitor Mix  : ", "Mic Level, Playback Level 0–100%"),
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
            ("  Monitor Mix  : ", "Mic Level, Playback Level 0–100%"),
            ("  Mute Button  : ", "Works / Ignored"),
        ],
        DeviceModel::Mv6Gen2 => &[
            ("  Denoiser     : ", "On / Off, level 1–100%"),
            ("  Popper Stop. : ", "On / Off"),
            (
                "  Tone         : ",
                "Simple: Dark (−100%) → Natural → Bright (+100%)",
            ),
            (
                "  5-band EQ    : ",
                "Advanced tone mode, −8 to +6 dB in 2 dB steps",
            ),
            ("  HPF          : ", "Off / 75 Hz / 150 Hz"),
            ("  Auto Level   : ", "On / Off"),
            ("  Target Level : ", "Low / Med / High (Auto mode)"),
            ("  Adaptation   : ", "Slow / Fast (Auto mode)"),
            ("  Mic Preset   : ", "Speech / Singing / Instrument"),
            ("  Monitor Mix  : ", "Mic Level, Playback Level 0–100%"),
            ("  Mute Button  : ", "Works / Ignored"),
        ],
        DeviceModel::Mv7 => &[
            (
                "  EQ           : ",
                "Flat / High Pass / Presence Boost / both",
            ),
            ("  Compressor   : ", "Off / Light / Medium / Heavy"),
            ("  Monitor Mix  : ", "0–100%"),
            ("  Auto Level   : ", "On / Off"),
            ("  Config Lock  : ", "On / Off"),
            ("  LED Panel    : ", "Live Meter / Night Mode"),
        ],
        DeviceModel::Mv7Plus => &[
            ("  Denoiser     : ", "On / Off"),
            ("  Popper Stop. : ", "On / Off"),
            (
                "  Tone         : ",
                "Dark (−100%) → Natural → Bright (+100%)",
            ),
            ("  HPF          : ", "Off / 75 Hz / 150 Hz"),
            ("  Compressor   : ", "Off / Light / Medium / Heavy"),
            ("  Limiter      : ", "On / Off"),
            ("  Mute Button  : ", "Works / Ignored"),
            ("  Monitor Mix  : ", "Mic Level, Playback Level 0–100%"),
            (
                "  Reverb       : ",
                "Plate / Hall / Studio; intensity 0–100%",
            ),
            (
                "  LED Panel    : ",
                "Behavior / Brightness / Theme / Custom RGB",
            ),
            ("  Auto Level   : ", "On / Off"),
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
        .block(border)
        .style(Style::default().bg(C_BG));
    f.render_widget(p, area);
}

// ─────────────────────────────────────────────────────────────────────────────
// Help Overlay
// ─────────────────────────────────────────────────────────────────────────────
/// One help overlay row: the key column padded so every description lines up.
fn help_row(key: &str, description: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {key:<19}"), Style::default().fg(C_TEXT)),
        Span::styled(description, Style::default().fg(C_DIM)),
    ])
}

fn draw_help_overlay(f: &mut UiFrame, area: Rect) {
    let popup_width = 58u16.min(area.width.saturating_sub(4));
    let popup_height = 32u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(popup_width)) / 2;
    let y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    f.render_widget(Clear, popup_area);

    let lines = vec![
        Line::from(Span::styled(
            "  Keyboard & Mouse",
            Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Navigation",
            Style::default()
                .fg(C_TEXT)
                .add_modifier(Modifier::UNDERLINED),
        )]),
        help_row("Tab / Shift+Tab", "Switch section"),
        help_row("↑ / k", "Focus previous control"),
        help_row("↓ / j", "Focus next control"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Adjustment",
            Style::default()
                .fg(C_TEXT)
                .add_modifier(Modifier::UNDERLINED),
        )]),
        help_row("← / h", "Decrease value"),
        help_row("→ / l", "Increase value"),
        help_row("Enter / Space", "Toggle boolean / cycle option"),
        help_row("f", "Flatten 5-band EQ (EQ tab)"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  General",
            Style::default()
                .fg(C_TEXT)
                .add_modifier(Modifier::UNDERLINED),
        )]),
        help_row("r", "Refresh state from device"),
        help_row("?", "Toggle this help"),
        help_row("q / Ctrl+C", "Quit"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Presets tab",
            Style::default()
                .fg(C_TEXT)
                .add_modifier(Modifier::UNDERLINED),
        )]),
        help_row("Enter (on name)", "Rename preset"),
        help_row("Enter (on actions)", "Load preset to device"),
        help_row("s", "Save current settings to slot"),
        help_row("d / Delete", "Delete preset"),
        Line::from(""),
        Line::from(Span::styled(
            "  Click: focus; click selected: activate",
            Style::default().fg(C_DIM),
        )),
        Line::from(Span::styled(
            "  Wheel: adjust; click outside name: cancel",
            Style::default().fg(C_DIM),
        )),
        Line::from(Span::styled(
            "  Press ? or Esc to close",
            Style::default().fg(C_DIM),
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
            .border_style(Style::default().fg(C_DIM))
            .style(Style::default().bg(C_SURFACE)),
    );
    f.render_widget(help, popup_area);
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const MODELS: [DeviceModel; 6] = [
        DeviceModel::Mvx2u,
        DeviceModel::Mvx2uGen2,
        DeviceModel::Mv6,
        DeviceModel::Mv6Gen2,
        DeviceModel::Mv7,
        DeviceModel::Mv7Plus,
    ];

    #[test]
    fn summary_rows_keep_the_shared_order() {
        for model in MODELS {
            // Strictly increasing: in the shared order, and no row listed twice.
            let rows = summary_rows(model);
            assert!(rows.windows(2).all(|pair| pair[0] < pair[1]), "{model:?}");
        }
    }

    // ── Rendering smoke test ──────────────────────────────────────────────────

    #[test]
    fn every_tab_renders_for_every_model_and_mode() {
        use ratatui::{Terminal, backend::TestBackend};

        for model in MODELS {
            for mode in [InputMode::Manual, InputMode::Auto] {
                for tab in Tab::ALL {
                    let mut app = App {
                        device_model: model,
                        active_tab: tab,
                        help_visible: tab == Tab::Info,
                        ..App::default()
                    };
                    app.device_state.mode = mode;
                    // Off-grid and out-of-range gain must still render.
                    app.device_state.gain_tenths = 9999;
                    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
                    terminal
                        .draw(|f| {
                            draw(f, &app);
                        })
                        .unwrap_or_else(|e| panic!("{model:?} {mode:?} {tab:?}: {e}"));
                }
            }
        }
    }

    // ── Render golden file ────────────────────────────────────────────────────

    const GOLDEN_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui_renders.golden");

    /// FNV-1a over every cell's symbol and style. Unlike `DefaultHasher`, it
    /// stays the same across Rust releases.
    fn buffer_hash(buffer: &ratatui::buffer::Buffer) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for cell in &buffer.content {
            let text = format!(
                "{}{:?}{:?}{:?}",
                cell.symbol(),
                cell.fg,
                cell.bg,
                cell.modifier
            );
            for byte in text.bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }
        hash
    }

    fn render_hash(app: &App) -> u64 {
        use ratatui::{Terminal, backend::TestBackend};

        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
        terminal
            .draw(|f| {
                draw(f, app);
            })
            .expect("draw");
        buffer_hash(terminal.backend().buffer())
    }

    /// Every focus `focus_next()` reaches from the current one, in order.
    fn focus_ring(app: &mut App) -> Vec<Focus> {
        let first = app.focus;
        let mut ring = Vec::new();
        while !ring.contains(&app.focus) {
            ring.push(app.focus);
            app.focus_next();
        }
        app.focus = first;
        ring
    }

    /// One line per model, mode, tab, focus and mute/lock/tone state, with a
    /// hash of the rendered screen.
    fn render_all() -> String {
        use std::fmt::Write;

        let mut out = String::new();
        for model in MODELS {
            let tone_modes: &[bool] = match model {
                DeviceModel::Mv6Gen2 => &[false, true],
                DeviceModel::Mvx2u
                | DeviceModel::Mvx2uGen2
                | DeviceModel::Mv6
                | DeviceModel::Mv7
                | DeviceModel::Mv7Plus => &[false],
            };
            let states = [InputMode::Manual, InputMode::Auto]
                .into_iter()
                .flat_map(|mode| {
                    tone_modes.iter().flat_map(move |&adv| {
                        [(false, false), (true, true)]
                            .map(|(muted, locked)| (mode, adv, muted, locked))
                    })
                });
            for (mode, tone_advanced, muted, locked) in states {
                for tab in Tab::ALL {
                    let mut app = App {
                        device_model: model,
                        active_tab: if tab == Tab::Main {
                            Tab::Info
                        } else {
                            Tab::Main
                        },
                        ..App::default()
                    };
                    let state = &mut app.device_state;
                    state.mode = mode;
                    state.tone_advanced = tone_advanced;
                    state.muted = muted;
                    state.locked = locked;
                    state.mv6_gain_locked = locked;
                    state.gain_tenths = 240;
                    for (band, gain_db) in state.eq_bands.iter_mut().zip([-80, -20, 0, 20, 60]) {
                        band.gain_db = gain_db;
                    }
                    if app.is_tab_hidden(tab) {
                        continue;
                    }
                    // A locked tab can't be selected and shows only its notice.
                    let tab_locked = app.is_tab_locked(tab);
                    app.select_tab(tab);
                    app.active_tab = tab;
                    let bands: &[usize] = if tab == Tab::Eq && app.eq_bands_shown() {
                        &[0, 2]
                    } else {
                        &[0]
                    };
                    let start = app.focus;
                    for &band in bands {
                        app.eq_selected_band = band;
                        app.focus = start;
                        let ring = if tab_locked {
                            vec![app.focus]
                        } else {
                            focus_ring(&mut app)
                        };
                        for focus in ring {
                            app.focus = focus;
                            writeln!(
                                out,
                                "{model:?} {mode:?} adv={tone_advanced} muted={muted} \
                                 locked={locked} {tab:?} band={band} {focus:?} {:016x}",
                                render_hash(&app)
                            )
                            .expect("write to String");
                        }
                    }
                }
            }
        }
        out
    }

    /// Proves a refactor leaves every screen unchanged. After an intended visual
    /// change, regenerate with `UPDATE_GOLDEN=1 cargo test renders_match_golden`
    /// and check the screens it lists in `--demo`.
    #[test]
    fn renders_match_golden() {
        use std::collections::HashSet;

        let actual = render_all();
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(GOLDEN_PATH, &actual).expect("write golden file");
            return;
        }
        let golden = std::fs::read_to_string(GOLDEN_PATH).unwrap_or_default();
        let golden_lines: HashSet<&str> = golden.lines().collect();
        let actual_lines: HashSet<&str> = actual.lines().collect();
        let mut diff: Vec<String> = golden
            .lines()
            .filter(|line| !actual_lines.contains(line))
            .map(|line| format!("- {line}"))
            .collect();
        diff.extend(
            actual
                .lines()
                .filter(|line| !golden_lines.contains(line))
                .map(|line| format!("+ {line}")),
        );
        assert!(
            diff.is_empty(),
            "{} golden lines differ; rerun with UPDATE_GOLDEN=1 if intended:\n{}",
            diff.len(),
            diff[..diff.len().min(40)].join("\n")
        );
    }

    fn header_text(app: &App) -> String {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
        terminal
            .draw(|f| {
                draw(f, app);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer[(x, 1)].symbol().to_string())
            .collect()
    }

    #[test]
    fn header_shows_connection_state() {
        let connected = App::default();
        assert!(header_text(&connected).contains("[CONNECTED]"));

        let unplugged = App {
            device_connected: false,
            ..App::default()
        };
        let text = header_text(&unplugged);
        assert!(text.contains("[DISCONNECTED]"), "{text}");
        assert!(!text.contains("[CONNECTED]"), "{text}");

        let demo = App {
            demo_mode: true,
            device_connected: false,
            ..App::default()
        };
        assert!(header_text(&demo).contains("[DEMO"));
    }

    // ── enum_options ──────────────────────────────────────────────────────────
    //
    // Single source of truth for the ▶ marker on every Enter-cycled control
    // (HPF, Compressor, Reverb Type) across all four models, so a bug here is a
    // bug on every Dynamics and Reverb tab at once.

    #[test]
    fn enum_options_marks_exactly_the_current_variant() {
        let opts = enum_options(
            &[HpfFrequency::Off, HpfFrequency::Hz75, HpfFrequency::Hz150],
            HpfFrequency::Hz75,
        );
        assert_eq!(opts.len(), 3);
        assert_eq!(opts.iter().filter(|(_, selected)| *selected).count(), 1);
        assert!(opts[1].1, "Hz75 is the current value");
        assert!(!opts[0].1);
        assert!(!opts[2].1);
    }

    #[test]
    fn enum_options_preserves_declaration_order_and_labels() {
        let opts = enum_options(
            &[
                CompressorPreset::Off,
                CompressorPreset::Light,
                CompressorPreset::Medium,
                CompressorPreset::Heavy,
            ],
            CompressorPreset::Heavy,
        );
        let labels: Vec<&str> = opts.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                CompressorPreset::Off.to_string(),
                CompressorPreset::Light.to_string(),
                CompressorPreset::Medium.to_string(),
                CompressorPreset::Heavy.to_string(),
            ]
        );
        assert!(opts[3].1, "last variant is the current value");
    }

    #[test]
    fn enum_options_marks_nothing_when_current_is_absent() {
        // Guards the Gen 1 Auto case, where a control can hold a value the
        // rendered list does not offer: no row should be marked rather than
        // defaulting the marker onto the first one.
        let opts = enum_options(
            &[HpfFrequency::Hz75, HpfFrequency::Hz150],
            HpfFrequency::Off,
        );
        assert!(opts.iter().all(|(_, selected)| !*selected));
    }

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
    fn build_ruler_end_labels_sit_at_both_edges() {
        // "-60" at the left edge and "0" at the right (it used to be dropped).
        for width in [20, 40, 80, 200] {
            let ruler = build_ruler(width);
            assert!(
                ruler.starts_with("-60"),
                "'-60' not at left edge: {ruler:?}"
            );
            assert!(ruler.ends_with(" 0"), "'0' not at right edge: {ruler:?}");
        }
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
