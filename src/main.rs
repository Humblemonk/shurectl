//! shurectl — Interactive TUI configurator for Shure USB audio interfaces and microphones
//!
//! Supports:
//!   - Shure MVX2U Gen 1 (XLR-to-USB audio interface)
//!   - Shure MVX2U Gen 2 (XLR-to-USB interface with updated DSP)
//!   - Shure MV6          (USB gaming microphone)
//!   - Shure MV6 Gen 2    (USB gaming microphone, Gen 2)
//!   - Shure MV7          (USB/XLR dynamic microphone, original)
//!   - Shure MV7+         (USB/XLR dynamic microphone)
//!
//! Usage:
//!   shurectl                   # Connect to device, launch TUI
//!   shurectl --demo            # Demo MVX2U (default model) without a device
//!   shurectl --demo mv6        # Demo MV6 without a device
//!   shurectl --demo mvx2u-gen2 # Demo MVX2U Gen 2 without a device
//!   shurectl --demo mv6-gen2   # Demo MV6 Gen 2 without a device
//!   shurectl --demo mv7        # Demo MV7 without a device
//!   shurectl --demo mv7plus    # Demo MV7+ without a device
//!   shurectl --list            # List detected devices and exit
//!   shurectl --device PATH     # Open a specific device by HID path
//!   shurectl --mute            # Toggle mute (no TUI)
//!   shurectl --mute on         # Mute (no TUI)
//!   shurectl --mute off        # Unmute (no TUI)
//!   shurectl --mute status     # Print on/off (no TUI)
//!   shurectl --preset 1        # Load preset slot 1 (no TUI)

mod app;
mod device;
#[cfg(any(test, feature = "qa"))]
#[cfg_attr(not(feature = "qa"), allow(dead_code))]
mod fake_hid;
mod firmware;
#[cfg(any(test, feature = "qa"))]
#[cfg_attr(not(feature = "qa"), allow(dead_code))]
mod harness;
mod meter;
mod mouse;
mod presets;
mod protocol;
mod ui;

use std::io;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    cursor::Show,
    event::{
        self, DisableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
        MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use app::{App, DeviceAction, STATE_UNREAD_HINT};
use device::ShureDevice;
use meter::{MeterStatus, start_meter};
use presets::PresetSlot;
use protocol::{DeviceModel, InputMode, format_gain};

/// Mute action for the `--mute` flag.
#[derive(Debug, Clone, PartialEq)]
enum MuteAction {
    /// Flip the current mute state.
    Toggle,
    /// Mute the microphone.
    On,
    /// Unmute the microphone.
    Off,
    /// Print the current state (`on` or `off`) without changing it.
    Status,
}

impl std::str::FromStr for MuteAction {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "toggle" => Ok(MuteAction::Toggle),
            "on" => Ok(MuteAction::On),
            "off" => Ok(MuteAction::Off),
            "status" => Ok(MuteAction::Status),
            other => {
                anyhow::bail!(
                    "unknown mute action \"{other}\". Valid options: toggle, on, off, status"
                )
            }
        }
    }
}

#[derive(Parser)]
#[command(
    name = "shurectl",
    version,
    about = "shurectl — TUI configurator for Shure USB audio interfaces and microphones"
)]
struct Cli {
    /// Run in demo mode without a real device.
    /// Optionally specify which device model to simulate: mvx2u (default), mvx2u-gen2, mv6,
    /// mv6-gen2, mv7, mv7plus.
    #[arg(long, short, num_args = 0..=1, default_missing_value = "mvx2u", value_name = "MODEL")]
    demo: Option<String>,

    /// List connected Shure devices and exit
    #[arg(long, short)]
    list: bool,

    /// Open a specific device by its HID path.
    /// Without this flag, the only detected device is opened (error if multiple found).
    /// Use --list to see available paths.
    #[arg(long, short = 'D')]
    device: Option<String>,

    /// Set mute state without launching the TUI.
    /// toggle (default): flip current state. on: mute. off: unmute.
    /// status: print the current state (on or off) without changing it.
    #[arg(long, short = 'm', num_args = 0..=1, default_missing_value = "toggle", value_name = "ACTION")]
    mute: Option<MuteAction>,

    /// Load a saved preset slot (1-4) onto the device without launching the TUI.
    #[arg(long, short = 'p', value_name = "SLOT", conflicts_with = "mute",
          value_parser = clap::value_parser!(u8).range(1..=presets::PRESET_COUNT as i64))]
    preset: Option<u8>,

    /// Maintainers only: drive the app headlessly with JSON lines on stdin and
    /// stdout, for tests/e2e.rs and agent QA. Needs --demo MODEL or
    /// --fake-device MODEL; never opens real hardware.
    #[cfg(feature = "qa")]
    #[arg(long, conflicts_with_all = ["device", "list", "mute", "preset"])]
    qa: bool,

    /// With --qa: run against a fake device of this model, which logs every
    /// packet it receives.
    #[cfg(feature = "qa")]
    #[arg(long, value_name = "MODEL", requires = "qa", conflicts_with = "demo")]
    fake_device: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    #[cfg(feature = "qa")]
    if cli.qa {
        let (model, fake) = match (&cli.demo, &cli.fake_device) {
            (Some(model), None) => (model, false),
            (None, Some(model)) => (model, true),
            _ => anyhow::bail!("--qa needs --demo MODEL or --fake-device MODEL"),
        };
        return harness::run_qa(parse_demo_model(model)?, fake);
    }

    if let Some(action) = cli.mute {
        let path = cli.device.as_deref();
        return cmd_mute(action, path);
    }

    if let Some(slot_number) = cli.preset {
        return cmd_preset(slot_number.into(), cli.device.as_deref());
    }

    if cli.list {
        let devs = device::list_devices();
        if devs.is_empty() {
            println!("No supported Shure devices found.");
            #[cfg(target_os = "linux")]
            println!("Check that the device is plugged in and the udev rule is installed.");
            #[cfg(not(target_os = "linux"))]
            println!("Check that the device is plugged in and accessible.");
        } else {
            println!("Found {} Shure device(s):", devs.len());
            for d in devs {
                // Show the factory serial (printed on the device) when we can read
                // it; fall back to the USB descriptor serial if the device can't be
                // opened or doesn't report one. This briefly opens each device.
                let serial = device::ShureDevice::open_path(&d.path)
                    .ok()
                    .and_then(|dev| dev.read_factory_serial())
                    .unwrap_or(d.serial);
                println!(
                    "  {} | {} | S/N: {}",
                    d.path,
                    d.model.display_name(),
                    serial
                );
            }
        }
        return Ok(());
    }

    let (mut device, demo_mode, demo_model) = if let Some(ref model_str) = cli.demo {
        let model = match parse_demo_model(model_str) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        };
        (None, true, Some(model))
    } else {
        let open_result = if let Some(ref path) = cli.device {
            ShureDevice::open_path(path)
        } else {
            ShureDevice::open()
        };
        match open_result {
            Ok(d) => (Some(d), false, None),
            Err(e) => {
                eprintln!("Warning: {e}");
                eprintln!("Launching in demo mode. Use --demo to suppress this warning.");
                (None, true, None)
            }
        }
    };

    let device_model = demo_model
        .or_else(|| device.as_ref().map(|d| d.model))
        .unwrap_or(DeviceModel::Mvx2u);

    let mut app = App {
        demo_mode,
        device_model,
        ..App::default()
    };

    load_initial_state(&mut app, device.as_ref());

    app.presets = presets::load_all_presets();

    let _meter_stream = if !demo_mode {
        match start_meter(Arc::clone(&app.peak_window)) {
            MeterStatus::Running(s) => Some(s),
            MeterStatus::Failed(e) => {
                app.set_err(format!("Meter unavailable: {e}"));
                None
            }
        }
    } else {
        None
    };

    install_terminal_restore_hook();
    enable_raw_mode()?;
    let cleanup = TerminalCleanup;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableClickCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let latest_firmware =
        (!demo_mode && firmware::is_compared(device_model)).then(firmware::spawn_check);
    let result = run_event_loop(&mut terminal, &mut app, &mut device, latest_firmware);

    drop(terminal);
    drop(cleanup);

    if let Err(e) = result {
        eprintln!("Error: {e}");
    }

    Ok(())
}

/// Read the device's state into `app` at startup, or say it is demo mode.
fn load_initial_state(app: &mut App, device: Option<&ShureDevice>) {
    if let Some(dev) = device {
        match reload_state(app, dev) {
            Ok(skipped) => {
                app.set_ok(format!(
                    "Connected to {} — state loaded.{}",
                    dev.model.display_name(),
                    skipped_note(&skipped)
                ));
            }
            Err(e) => {
                app.device_connected = !is_disconnect(&e);
                app.state_unread = true;
                app.set_err(format!(
                    "Connected but failed to read state: {e}. Press r to retry."
                ));
            }
        }
    } else {
        app.set_ok(format!(
            "Demo mode ({}) — changes will not be sent to a device.",
            app.device_model.display_name()
        ));
    }
}

/// Restore the terminal before a panic message prints. Release builds abort on
/// panic, so without this the terminal stays in raw mode on the alternate
/// screen with the cursor hidden, and the message is lost with that screen.
fn install_terminal_restore_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Nothing useful to do if restoring fails while already panicking.
        drop(TerminalCleanup);
        default_hook(info);
    }));
}

/// Apply a mute action to the connected device without launching the TUI.
///
/// Opens the device (or the specific path if `--device` was given), reads
/// current state to resolve `Toggle`, then sends a single `set_mute()`.
/// Prints the resulting state to stdout so the caller can see what happened.
/// `Status` prints a bare `on`/`off` instead, for status-bar scripts.
fn cmd_mute(action: MuteAction, device_path: Option<&str>) -> Result<()> {
    let dev = open_cli_device(device_path)?;
    let read_muted = || -> Result<bool> {
        Ok(dev
            .get_state()
            .context("Could not read device state")?
            .state
            .muted)
    };

    let muted = match action {
        MuteAction::On => true,
        MuteAction::Off => false,
        MuteAction::Toggle => !read_muted()?,
        MuteAction::Status => {
            println!("{}", if read_muted()? { "on" } else { "off" });
            return Ok(());
        }
    };

    dev.set_mute(muted).context("Could not set mute")?;
    println!("Mute → {}", if muted { "ON" } else { "OFF" });
    Ok(())
}

/// Load preset slot `slot_number` (1-based) onto the device without launching the TUI.
///
/// Reads the current state first so settings the preset doesn't cover keep
/// their values, then sends everything the same way the Presets tab does.
fn cmd_preset(slot_number: usize, device_path: Option<&str>) -> Result<()> {
    let slot = presets::load_preset(slot_number - 1)?
        .with_context(|| format!("Preset slot {slot_number} is empty."))?;
    let dev = open_cli_device(device_path)?;
    let model = dev.model;
    let mut state = dev
        .get_state()
        .context("Could not read device state")?
        .state;
    let write_monitor_mix = apply_preset_to_state(&slot, &mut state, model);
    let refused = apply_preset_to_device(&Some(dev), &slot, &state, model, write_monitor_mix)
        .context("Could not load preset")?;
    println!(
        "Loaded \"{}\".{}{}",
        slot.name,
        refused_note(&refused),
        unread_note(&slot, &state, model)
    );
    Ok(())
}

fn open_cli_device(device_path: Option<&str>) -> Result<ShureDevice> {
    match device_path {
        Some(path) => ShureDevice::open_path(path),
        None => ShureDevice::open(),
    }
    .context("Could not open device")
}

/// Parse a `--demo` model string into a `DeviceModel`.
/// Accepts case-insensitive variants of the supported model names.
fn parse_demo_model(s: &str) -> Result<DeviceModel> {
    match s.to_ascii_lowercase().replace('_', "-").as_str() {
        "mvx2u" => Ok(DeviceModel::Mvx2u),
        "mvx2u-gen2" | "mvx2ugen2" => Ok(DeviceModel::Mvx2uGen2),
        "mv6" => Ok(DeviceModel::Mv6),
        "mv6-gen2" | "mv6gen2" => Ok(DeviceModel::Mv6Gen2),
        "mv7" => Ok(DeviceModel::Mv7),
        "mv7plus" | "mv7+" => Ok(DeviceModel::Mv7Plus),
        other => {
            anyhow::bail!(
                "unknown demo model \"{other}\". Valid options: mvx2u, mvx2u-gen2, mv6, mv6-gen2, mv7, mv7plus"
            )
        }
    }
}

/// Mouse reporting for clicks and the wheel only. crossterm's `EnableMouseCapture`
/// also turns on any-motion tracking (`?1003h`), so every pointer move woke the
/// event loop for a full redraw. Nothing here uses motion or dragging.
/// `DisableMouseCapture` still turns everything off on exit.
struct EnableClickCapture;

impl crossterm::Command for EnableClickCapture {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        // Normal tracking (press, release, wheel), with SGR coordinates so
        // columns past 223 still report correctly.
        f.write_str("\x1b[?1000h\x1b[?1006h")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        crossterm::Command::execute_winapi(&crossterm::event::EnableMouseCapture)
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        crossterm::Command::is_ansi_code_supported(&crossterm::event::EnableMouseCapture)
    }
}

/// Longest a wheel adjustment waits for more wheel events before it is sent.
/// A trackpad flick sends dozens of events, each of which would otherwise be a
/// blocking HID write that makes the device lag behind the gesture.
const SCROLL_FLUSH_INTERVAL: Duration = Duration::from_millis(50);

/// Restore the terminal on normal exit, startup errors, and unwinding.
struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            LeaveAlternateScreen,
            Show
        );
    }
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    device: &mut Option<ShureDevice>,
    latest_firmware: Option<mpsc::Receiver<firmware::LatestFirmware>>,
) -> Result<()> {
    let tick_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();
    let mut loop_state = LoopState::new(Instant::now());

    loop {
        if let Some(latest) = latest_firmware
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            app.latest_firmware = Some(latest);
        }
        let mut hits = mouse::HitMap::default();
        let mut fits = true;
        terminal.draw(|f| {
            fits = ui::fits(f.area());
            hits = ui::draw(f, app);
        })?;

        let timeout = if loop_state.pending_scroll.is_some() {
            Duration::ZERO
        } else {
            tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_default()
        };

        let event = if event::poll(timeout)? {
            Some(event::read()?)
        } else {
            None
        };
        step(
            app,
            device,
            &hits,
            fits,
            event,
            Instant::now(),
            &mut loop_state,
        );

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

/// Event-loop state carried between `step()` calls.
struct LoopState {
    last_presence_poll: Instant,
    last_mute_poll: Instant,
    seen_back: bool,
    pending_scroll: Option<PendingScroll>,
}

/// A wheel adjustment not written yet. Wheel adjustments carry absolute values,
/// so the newest one of a burst on one control supersedes the rest.
struct PendingScroll {
    focus: app::Focus,
    action: DeviceAction,
    /// When the burst began.
    started: Instant,
    /// The state before the burst's first tick, put back if its write fails.
    before: protocol::DeviceState,
}

/// Write the pending wheel adjustment, if any.
fn flush_scroll(app: &mut App, device: &mut Option<ShureDevice>, loop_state: &mut LoopState) {
    if let Some(pending) = loop_state.pending_scroll.take() {
        apply_action(app, device, pending.action, Some(pending.before));
    }
}

impl LoopState {
    fn new(now: Instant) -> Self {
        Self {
            last_presence_poll: now,
            last_mute_poll: now,
            seen_back: false,
            pending_scroll: None,
        }
    }
}

/// One pass of the event loop after a frame is drawn: route `event` (if any)
/// through `handle_key()` or the mouse hit map into `apply_action()`, flush a
/// pending wheel adjustment, and poll for the device every
/// `PRESENCE_POLL_INTERVAL`. `fits` is `ui::fits()` for the frame just drawn:
/// below the minimum size only the notice is on screen, so keys only quit and
/// mouse events are dropped. The mute state is read every
/// `MUTE_POLL_INTERVAL`. `now` is passed in so the `--qa` harness can run the
/// same code on a fake clock.
fn step(
    app: &mut App,
    device: &mut Option<ShureDevice>,
    hits: &mouse::HitMap,
    fits: bool,
    event: Option<Event>,
    now: Instant,
    loop_state: &mut LoopState,
) {
    let queued = event.is_some();
    if let Some(event) = event {
        let scroll_target = match event {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp | MouseEventKind::ScrollDown,
                column,
                row,
                ..
            }) => Some(hits.target_at(column, row)),
            Event::Key(_)
            | Event::Mouse(_)
            | Event::Resize(_, _)
            | Event::FocusGained
            | Event::FocusLost
            | Event::Paste(_) => None,
        };
        // Write a pending wheel adjustment before this event can change the
        // screen, unless it is another tick on the same control: writes stay in
        // input order, and each one's `before` holds only its own change. A
        // button release, resize or focus change can't act, so the burst goes on.
        let can_act = scroll_target.is_some()
            || matches!(
                event,
                Event::Key(KeyEvent {
                    kind: KeyEventKind::Press,
                    ..
                }) | Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(_),
                    ..
                })
            );
        let same_control = matches!(
            (&loop_state.pending_scroll, scroll_target),
            (Some(pending), Some(Some(mouse::Target::Control(focus)))) if pending.focus == focus
        );
        if can_act && !same_control {
            flush_scroll(app, device, loop_state);
        }
        let before = can_act.then(|| app.device_state.clone());
        let action = match event {
            Event::Key(key) if key.kind == KeyEventKind::Press && !fits => {
                handle_key_too_small(app, key.code, key.modifiers);
                None
            }
            // Nothing on the notice is clickable, and a click would still
            // cancel a name edit or a Factory Reset confirmation.
            Event::Mouse(_) if !fits => None,
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                app.armed_preset_action = None;
                handle_key(app, key.code, key.modifiers)
            }
            Event::Mouse(event) => mouse::handle_mouse(app, event, hits)
                .and_then(|key| handle_key(app, key, KeyModifiers::NONE)),
            Event::Key(_)
            | Event::Resize(_, _)
            | Event::FocusGained
            | Event::FocusLost
            | Event::Paste(_) => None,
        };
        match action {
            Some(action) if scroll_target.is_some() => {
                // Only a burst on this control can still be pending.
                let (started, before) = match (loop_state.pending_scroll.take(), before) {
                    (Some(pending), _) => (pending.started, pending.before),
                    (None, before) => (now, before.unwrap_or_else(|| app.device_state.clone())),
                };
                loop_state.pending_scroll = Some(PendingScroll {
                    focus: app.focus,
                    action,
                    started,
                    before,
                });
            }
            Some(action) => apply_action(app, device, action, before),
            None => {}
        }
    }

    let poll_due = now.duration_since(loop_state.last_presence_poll) >= PRESENCE_POLL_INTERVAL;
    let mute_poll_due = now.duration_since(loop_state.last_mute_poll) >= MUTE_POLL_INTERVAL;
    // A pending wheel write goes out before a poll, so if it fails, putting
    // back its `before` state can't undo a mute change the poll just read.
    if loop_state.pending_scroll.as_ref().is_some_and(|pending| {
        !queued
            || app.should_quit
            || poll_due
            || mute_poll_due
            || now.duration_since(pending.started) >= SCROLL_FLUSH_INTERVAL
    }) {
        flush_scroll(app, device, loop_state);
    }

    if mute_poll_due {
        loop_state.last_mute_poll = now;
        poll_mute(app, device);
    }

    if poll_due {
        loop_state.last_presence_poll = now;
        poll_presence(app, device, &mut loop_state.seen_back);
    }
}

fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) -> Option<DeviceAction> {
    // Until the device's settings have been read, the state holds defaults
    // (36 dB gain), and a write based on it could jump far from the real value.
    // Refuse such writes and undo the change the key made to the screen.
    let unread = app
        .state_unread
        .then(|| (app.device_state.clone(), app.focus));
    let action = key_action(app, code, mods)?;
    match unread {
        Some((state, focus)) if !reads_no_state(&action) => {
            app.device_state = state;
            app.focus = focus;
            app.set_err(STATE_UNREAD_HINT);
            None
        }
        Some(_) | None => Some(action),
    }
}

/// While the too-small notice hides what a key would change, keys only quit:
/// Ctrl+C always, and `q` unless it is being typed into a name. Any key still
/// disarms a half-clicked preset action, as it does normally.
fn handle_key_too_small(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.armed_preset_action = None;
    let typed = app.editing_name() && !mods.contains(KeyModifiers::CONTROL);
    app.should_quit = is_quit_key(code, mods) && !typed;
}

/// The keys that quit: `q` and Ctrl+C.
fn is_quit_key(code: KeyCode, mods: KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('q') | KeyCode::Char('Q'))
        || (code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL))
}

/// The action a key asks for, after applying its UI-only effects.
fn key_action(app: &mut App, code: KeyCode, mods: KeyModifiers) -> Option<DeviceAction> {
    // ── Preset or device name editing mode ────────────────────────────────────
    // Handled before the quit keys so `q` can be typed in a name, and Ctrl+C
    // doesn't quit while editing.
    if app.editing_name() {
        let (max_len, device_name) = if app.editing_device_name {
            (protocol::DEVICE_NAME_MAX_LEN, true)
        } else {
            (40, false)
        };
        match code {
            KeyCode::Enter if device_name => {
                app.editing_device_name = false;
                let name = std::mem::take(&mut app.name_draft);
                if name.is_empty() {
                    app.set_err("The device name can't be empty.");
                } else {
                    return Some(DeviceAction::RenameDevice(name));
                }
            }
            KeyCode::Enter => {
                app.editing_preset_name = false;
                let name = std::mem::take(&mut app.name_draft);
                let i = app.editing_preset_index;
                if let Some(slot) = &mut app.presets[i] {
                    slot.name = name;
                    return Some(DeviceAction::PersistPresetName(i));
                }
            }
            KeyCode::Esc => {
                app.editing_preset_name = false;
                app.editing_device_name = false;
                app.name_draft.clear();
            }
            KeyCode::Backspace => {
                app.name_draft.pop();
            }
            KeyCode::Char(c)
                if !mods.contains(KeyModifiers::CONTROL)
                    && app.name_draft.len() < max_len
                    && (!device_name || protocol::is_device_name_char(c)) =>
            {
                app.name_draft.push(c);
            }
            _ => {}
        }
        return None;
    }

    if is_quit_key(code, mods) {
        app.should_quit = true;
        return None;
    }

    if app.confirming_factory_reset {
        match code {
            KeyCode::Enter => {
                app.confirming_factory_reset = false;
                return Some(DeviceAction::FactoryReset);
            }
            _ => {
                app.confirming_factory_reset = false;
                app.set_ok("Factory reset cancelled.");
                return None;
            }
        }
    }

    if app.help_visible {
        if matches!(code, KeyCode::Char('?') | KeyCode::Esc) {
            app.help_visible = false;
        }
        return None;
    }

    match code {
        KeyCode::Char('?') => {
            app.help_visible = true;
            None
        }
        KeyCode::Char('r') => Some(DeviceAction::Refresh),
        KeyCode::Tab => {
            app.next_tab();
            None
        }
        // Terminals send Shift+Tab as its own back-tab key, not Tab with SHIFT set.
        KeyCode::BackTab => {
            app.prev_tab();
            None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.focus_prev();
            None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.focus_next();
            None
        }
        KeyCode::Left | KeyCode::Char('h') => app.adjust_focused(-1),
        KeyCode::Right | KeyCode::Char('l') => app.adjust_focused(1),
        KeyCode::Enter | KeyCode::Char(' ') => {
            if let app::Focus::PresetName(i) = app.focus
                && let Some(slot) = &app.presets[i]
            {
                // Keep an independent draft so cancelling never changes the preset.
                app.name_draft.clone_from(&slot.name);
                app.editing_preset_name = true;
                app.editing_preset_index = i;
                return None;
            }
            if app.focus == app::Focus::DeviceName {
                // Only focusable on models that support renaming.
                if app.device_model.has_device_rename() {
                    // An independent draft, so cancelling never changes the name.
                    app.name_draft = match app.device_state.device_name.as_str() {
                        "Unknown" => String::new(),
                        name => name.to_string(),
                    };
                    app.editing_device_name = true;
                }
                return None;
            }
            if app.focus == app::Focus::FactoryReset {
                // The button is only drawn where a reset exists; on other models
                // Enter on the Info tab does nothing.
                if app.supports_factory_reset() {
                    app.confirming_factory_reset = true;
                    app.set_err(
                        "! This will erase all device settings. Press Enter to confirm, any other key to cancel.",
                    );
                }
                return None;
            }
            app.toggle_focused()
        }
        KeyCode::Char('s') if app.active_tab == app::Tab::Presets => {
            if let app::Focus::PresetName(i) | app::Focus::PresetActions(i) = app.focus {
                Some(DeviceAction::SavePreset(i))
            } else {
                None
            }
        }
        KeyCode::Char('f') if app.can_flatten_eq() => Some(DeviceAction::FlattenEq),
        KeyCode::Char('d') | KeyCode::Delete if app.active_tab == app::Tab::Presets => {
            if let app::Focus::PresetActions(i) = app.focus {
                if app.presets[i].is_some() {
                    Some(DeviceAction::DeletePreset(i))
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Send `action` to the device: the only place that writes to it. `before` is
/// the state before the key changed the screen, put back if the write clearly
/// didn't land; `None` when no key changed it (a presence poll's reconnect).
fn apply_action(
    app: &mut App,
    device: &mut Option<ShureDevice>,
    action: DeviceAction,
    before: Option<protocol::DeviceState>,
) {
    // handle_key() refuses these first and undoes the screen change. This also
    // catches a write queued before the state became unread, such as a wheel
    // adjustment pending across a reconnect whose readback failed.
    if app.state_unread && !reads_no_state(&action) {
        app.set_err(STATE_UNREAD_HINT);
        return;
    }
    if matches!(action, DeviceAction::SetMv6MuteBtnDisable(_))
        && app.device_state.mute_btn_disabled.is_none()
    {
        app.report_unavailable_mute_button();
        return;
    }
    let mode_before = app.device_state.mode;
    let result = match &action {
        DeviceAction::Refresh => refresh(app, device),
        DeviceAction::Reconnect => reconnect(app, device),
        DeviceAction::SetGain(gain_tenths) => {
            app.set_ok(format!("Gain → {}", format_gain(*gain_tenths)));
            send_if_connected(device, |d| d.set_gain(*gain_tenths))
        }
        DeviceAction::SetMode(mode) => {
            app.set_ok(format!("Mode → {}", mode));
            let state = &app.device_state;
            send_if_connected(device, |d| {
                d.set_mode(
                    *mode == InputMode::Auto,
                    state.auto_position,
                    state.auto_tone,
                )
            })
            .and_then(|()| resync_mv7_state(app, device))
        }
        DeviceAction::SetAutoPosition(pos) => {
            app.set_ok(format!("Mic Position → {}", pos));
            let tone = app.device_state.auto_tone;
            send_if_connected(device, |d| d.set_auto_position(*pos, tone))
                .and_then(|()| resync_mv7_state(app, device))
        }
        DeviceAction::SetAutoTone(tone) => {
            app.set_ok(format!("Tone → {}", tone));
            let position = app.device_state.auto_position;
            send_if_connected(device, |d| d.set_auto_tone(position, *tone))
                .and_then(|()| resync_mv7_state(app, device))
        }
        DeviceAction::SetAutoGain(gain) => {
            app.set_ok(match app.device_model {
                DeviceModel::Mv6Gen2 => format!("Target Level → {}", gain.target_level_label()),
                DeviceModel::Mvx2u
                | DeviceModel::Mvx2uGen2
                | DeviceModel::Mv6
                | DeviceModel::Mv7
                | DeviceModel::Mv7Plus => format!("Auto Gain → {gain}"),
            });
            send_if_connected(device, |d| d.set_auto_gain(gain))
        }
        DeviceAction::SetMute(m) => {
            app.set_ok(format!("Mute → {}", if *m { "ON" } else { "OFF" }));
            send_if_connected(device, |d| d.set_mute(*m))
        }
        DeviceAction::SetPhantom(p) => {
            app.set_ok(format!("Phantom → {}", if *p { "48V ON" } else { "OFF" }));
            send_if_connected(device, |d| d.set_phantom(*p))
        }
        DeviceAction::SetLock(locked) => {
            if *locked {
                app.set_ok("Device locked.");
            } else {
                app.set_ok("Device unlocked.");
            }
            send_if_connected(device, |d| d.set_lock(*locked))
        }
        DeviceAction::SetLimiter(en) => {
            app.set_ok(format!("Limiter → {}", if *en { "ON" } else { "OFF" }));
            send_if_connected(device, |d| d.set_limiter(*en))
        }
        DeviceAction::SetCompressor(preset) => {
            app.set_ok(format!("Compressor → {}", preset));
            send_if_connected(device, |d| d.set_compressor(preset))
        }
        DeviceAction::SetHpf(freq) => {
            app.set_ok(format!("HPF → {}", freq));
            send_if_connected(device, |d| d.set_hpf(freq))
        }
        DeviceAction::SetEqEnable(en) => {
            app.set_ok(format!("EQ → {}", if *en { "Enabled" } else { "Bypass" }));
            send_if_connected(device, |d| d.set_eq_enable(*en))
        }
        DeviceAction::SetEqBandEnable(band, en) => {
            app.set_ok(format!(
                "EQ Band {} → {}",
                band + 1,
                if *en { "ON" } else { "OFF" }
            ));
            send_if_connected(device, |d| d.set_eq_band_enable(*band, *en))
        }
        DeviceAction::SetEqBandGain(band, gain_tenths) => {
            let db = *gain_tenths as f32 / 10.0;
            app.set_ok(format!("EQ Band {} → {:+.1} dB", band + 1, db));
            send_if_connected(device, |d| d.set_eq_band_gain(*band, *gain_tenths))
        }
        DeviceAction::FlattenEq => {
            for band in app.device_state.eq_bands.iter_mut() {
                band.gain_db = 0;
            }
            app.set_ok("EQ flattened.");
            send_if_connected(device, |d| {
                for band in 0..5 {
                    d.set_eq_band_gain(band, 0)?;
                }
                Ok(())
            })
        }
        // ── Named for the MV6, which had them first, and the MV6 Gen 2's ──────
        DeviceAction::SetMv6Denoiser(en) => {
            app.set_ok(format!("Denoiser → {}", if *en { "ON" } else { "OFF" }));
            send_if_connected(device, |d| d.set_mv6_denoiser(*en))
        }
        DeviceAction::SetAdaptationRate(fast) => {
            app.set_ok(format!(
                "Adaptation Rate → {}",
                if *fast { "Fast" } else { "Slow" }
            ));
            send_if_connected(device, |d| d.set_adaptation_rate(*fast))
        }
        DeviceAction::SetMicPreset(preset) => {
            app.set_ok(format!("Mic Preset → {preset}"));
            send_if_connected(device, |d| d.set_mic_preset(*preset))
        }
        DeviceAction::SetToneMode(advanced) => {
            app.set_ok(if *advanced {
                "Tone → Advanced (5-band EQ)"
            } else {
                "Tone → Simple"
            });
            send_if_connected(device, |d| d.set_tone_mode(*advanced))
        }
        DeviceAction::SetDenoiserLevel(level) => {
            app.set_ok(format!("Denoiser Level → {level}%"));
            send_if_connected(device, |d| d.set_denoiser_level(*level))
        }
        DeviceAction::SetMv6PopperStopper(en) => {
            app.set_ok(format!(
                "Popper Stopper → {}",
                if *en { "ON" } else { "OFF" }
            ));
            send_if_connected(device, |d| d.set_mv6_popper_stopper(*en))
        }
        DeviceAction::SetMv6MuteBtnDisable(disabled) => {
            app.set_ok(format!(
                "Mute button → {}",
                if *disabled { "Ignored" } else { "Works" }
            ));
            send_if_connected(device, |d| d.set_mv6_mute_btn_disable(*disabled))
        }
        DeviceAction::SetMv6Tone(tone) => {
            let pct = *tone as i32 * 10;
            let label = if pct < 0 {
                format!("{}% Dark", pct.abs())
            } else if pct > 0 {
                format!("{}% Bright", pct)
            } else {
                "Natural".to_string()
            };
            app.set_ok(format!("Tone → {label}"));
            send_if_connected(device, |d| d.set_mv6_tone(*tone))
        }
        DeviceAction::SetMv6GainLock(locked) => {
            app.set_ok(if *locked {
                "Gain locked.".to_string()
            } else {
                "Gain unlocked.".to_string()
            });
            send_if_connected(device, |d| d.set_mv6_gain_lock(*locked))
        }
        DeviceAction::SetMonitorMix(m) => {
            app.set_ok(if app.device_model.has_playback_mix() {
                format!("Mic Level → {m}%")
            } else {
                format!("Monitor Mix → {m}%")
            });
            send_if_connected(device, |d| d.set_monitor_mix(*m))
        }
        DeviceAction::SetMv7PlaybackMix(m) => {
            app.set_ok(format!("Playback Level → {m}%"));
            send_if_connected(device, |d| d.set_mv7_playback_mix(*m))
        }
        // ── MV7+ exclusive actions ────────────────────────────────────────────
        DeviceAction::SetMv7ReverbOutput(en) => {
            app.set_ok(format!(
                "Reverb output → {}",
                if *en { "ON" } else { "OFF" }
            ));
            send_if_connected(device, |d| d.set_mv7_reverb_output(*en))
        }
        DeviceAction::SetMv7ReverbMonitor(en) => {
            app.set_ok(format!(
                "Reverb monitoring → {}",
                if *en { "ON" } else { "OFF" }
            ));
            send_if_connected(device, |d| d.set_mv7_reverb_monitor(*en))
        }
        DeviceAction::SetMv7ReverbPreset(rtype) => {
            app.set_ok(format!("Reverb type → {}", rtype));
            send_if_connected(device, |d| d.set_mv7_reverb_type(rtype))
        }
        DeviceAction::SetMv7ReverbIntensity(intensity) => {
            app.set_ok(format!("Reverb intensity → {}%", intensity));
            send_if_connected(device, |d| d.set_mv7_reverb_intensity(*intensity))
        }
        DeviceAction::SetMv7LedBehavior(behavior) => {
            app.set_ok(format!("LED behavior → {behavior}"));
            send_if_connected(device, |d| d.set_mv7_led_behavior(*behavior))
        }
        DeviceAction::SetMv7LedBrightness(brightness) => {
            app.set_ok(format!("LED brightness → {brightness}"));
            send_if_connected(device, |d| d.set_mv7_led_brightness(*brightness))
        }
        DeviceAction::SetMv7LedLiveTheme(theme) => {
            app.set_ok(format!("LED live theme → {theme}"));
            send_if_connected(device, |d| d.set_mv7_led_live_theme(*theme))
        }
        DeviceAction::SetMv7LedSolidTheme(theme) => {
            app.set_ok(format!("LED solid theme → {theme}"));
            send_if_connected(device, |d| d.set_mv7_led_solid_theme(*theme))
        }
        DeviceAction::SetMv7LedPulsingTheme(theme) => {
            app.set_ok(format!("LED pulsing theme → {theme}"));
            send_if_connected(device, |d| d.set_mv7_led_pulsing_theme(*theme))
        }
        DeviceAction::SetMv7LedSolidRgb(rgb) => {
            app.set_ok(format!(
                "LED solid color → #{:02X}{:02X}{:02X}",
                rgb[0], rgb[1], rgb[2]
            ));
            send_if_connected(device, |d| d.set_mv7_led_solid_color(*rgb))
        }
        DeviceAction::SetMv7LedPulsingRgb(rgb) => {
            app.set_ok(format!(
                "LED pulsing color → #{:02X}{:02X}{:02X}",
                rgb[0], rgb[1], rgb[2]
            ));
            send_if_connected(device, |d| d.set_mv7_led_pulsing_color(*rgb))
        }
        DeviceAction::SetMv7LedLiveEdgeRgb(rgb) => {
            app.set_ok(format!(
                "LED live edge → #{:02X}{:02X}{:02X}",
                rgb[0], rgb[1], rgb[2]
            ));
            send_if_connected(device, |d| d.set_mv7_led_live_edge(*rgb))
        }
        DeviceAction::SetMv7LedLiveMiddleRgb(rgb) => {
            app.set_ok(format!(
                "LED live middle → #{:02X}{:02X}{:02X}",
                rgb[0], rgb[1], rgb[2]
            ));
            send_if_connected(device, |d| d.set_mv7_led_live_middle(*rgb))
        }
        DeviceAction::SetMv7LedLiveInteriorRgb(rgb) => {
            app.set_ok(format!(
                "LED live interior → #{:02X}{:02X}{:02X}",
                rgb[0], rgb[1], rgb[2]
            ));
            send_if_connected(device, |d| d.set_mv7_led_live_interior(*rgb))
        }
        // ── MV7 exclusive actions ─────────────────────────────────────────────
        DeviceAction::SetEqPreset(preset) => {
            app.set_ok(format!("EQ → {preset}"));
            send_if_connected(device, |d| d.set_eq_preset(*preset))
        }
        DeviceAction::SetLedLiveMeter(en) => {
            app.set_ok(format!("Live Meter → {}", if *en { "ON" } else { "OFF" }));
            send_if_connected(device, |d| d.set_led_live_meter(*en))
        }
        DeviceAction::SetLedNightMode(en) => {
            app.set_ok(format!("Night Mode → {}", if *en { "ON" } else { "OFF" }));
            send_if_connected(device, |d| d.set_led_night_mode(*en))
        }
        // ── Preset actions ────────────────────────────────────────────────────
        DeviceAction::SavePreset(i) => {
            let name = app.presets[*i]
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_else(|| format!("Preset {}", i + 1));
            let slot = PresetSlot::from_device_state(name, &app.device_state, app.device_model);
            match presets::save_preset(*i, &slot) {
                Ok(()) => {
                    app.set_ok(format!("Saved to \"{}\".", slot.name));
                    app.presets[*i] = Some(slot);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        DeviceAction::LoadPreset(i) => {
            if let Some(slot) = &app.presets[*i].clone() {
                let write_monitor_mix =
                    apply_preset_to_state(slot, &mut app.device_state, app.device_model);
                apply_preset_to_device(
                    device,
                    slot,
                    &app.device_state,
                    app.device_model,
                    write_monitor_mix,
                )
                .and_then(|refused| {
                    let message = format!(
                        "Loaded \"{}\".{}{}",
                        slot.name,
                        refused_note(&refused),
                        unread_note(slot, &app.device_state, app.device_model)
                    );
                    if refused.is_empty() {
                        app.set_ok(message);
                    } else {
                        app.set_err(message);
                    }
                    resync_mv7_state(app, device)
                })
            } else {
                app.set_err(format!("Preset slot {} is empty.", i + 1));
                Ok(())
            }
        }
        DeviceAction::DeletePreset(i) => match presets::delete_preset(*i) {
            Ok(()) => {
                let name = app.presets[*i]
                    .as_ref()
                    .map(|s| s.name.as_str())
                    .unwrap_or("preset");
                app.set_ok(format!("Deleted \"{}\".", name));
                app.presets[*i] = None;
                Ok(())
            }
            Err(e) => Err(e),
        },
        DeviceAction::PersistPresetName(i) => {
            if let Some(slot) = &app.presets[*i] {
                match presets::save_preset(*i, slot) {
                    Ok(()) => {
                        app.set_ok(format!("Renamed to \"{}\".", slot.name));
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            } else {
                Ok(())
            }
        }
        DeviceAction::RenameDevice(name) => send_if_connected(device, |d| d.set_device_name(name))
            .map(|()| {
                app.device_state.device_name.clone_from(name);
                app.set_ok(format!("Renamed device to \"{name}\"."));
            }),
        DeviceAction::FactoryReset => send_if_connected(device, |d| d.factory_reset()).map(|()| {
            // The presence poll sees the reboot as an unplug and reconnects.
            app.set_ok("Factory reset sent — device is restarting and reconnects automatically.");
        }),
    };

    if let Err(e) = result {
        recover_from_failure(app, device, &action, before, &e);
    }

    // A readback, a preset or an undone write may have changed the mode under
    // the current tab.
    app.settle_focus(mode_before);
}

/// What an action does, which decides whether it may run before the device's
/// settings have been read (`reads_no_state()`) and how a failure is undone
/// (`recover_from_failure()`).
enum ActionKind {
    /// Reads the device or reconnects to it, and reports its own failures.
    Connection,
    /// Changes preset files only.
    PresetFile,
    /// Saves the shown state as a preset.
    SavePreset,
    /// One write whose value doesn't come from the shown state.
    FixedWrite,
    /// One write built on the shown state.
    Write,
    /// Several writes, which can stop partway.
    Writes,
}

fn action_kind(action: &DeviceAction) -> ActionKind {
    match action {
        DeviceAction::Refresh | DeviceAction::Reconnect => ActionKind::Connection,
        DeviceAction::DeletePreset(_) | DeviceAction::PersistPresetName(_) => {
            ActionKind::PresetFile
        }
        DeviceAction::SavePreset(_) => ActionKind::SavePreset,
        DeviceAction::RenameDevice(_) | DeviceAction::FactoryReset => ActionKind::FixedWrite,
        DeviceAction::LoadPreset(_) | DeviceAction::FlattenEq => ActionKind::Writes,
        DeviceAction::SetGain(_)
        | DeviceAction::SetMode(_)
        | DeviceAction::SetAutoPosition(_)
        | DeviceAction::SetAutoTone(_)
        | DeviceAction::SetAutoGain(_)
        | DeviceAction::SetMute(_)
        | DeviceAction::SetPhantom(_)
        | DeviceAction::SetLock(_)
        | DeviceAction::SetLimiter(_)
        | DeviceAction::SetCompressor(_)
        | DeviceAction::SetHpf(_)
        | DeviceAction::SetEqEnable(_)
        | DeviceAction::SetEqBandEnable(_, _)
        | DeviceAction::SetEqBandGain(_, _)
        | DeviceAction::SetMv6Denoiser(_)
        | DeviceAction::SetMv6PopperStopper(_)
        | DeviceAction::SetMv6MuteBtnDisable(_)
        | DeviceAction::SetMv6Tone(_)
        | DeviceAction::SetMv6GainLock(_)
        | DeviceAction::SetMonitorMix(_)
        | DeviceAction::SetMv7PlaybackMix(_)
        | DeviceAction::SetAdaptationRate(_)
        | DeviceAction::SetDenoiserLevel(_)
        | DeviceAction::SetToneMode(_)
        | DeviceAction::SetMicPreset(_)
        | DeviceAction::SetMv7ReverbOutput(_)
        | DeviceAction::SetMv7ReverbMonitor(_)
        | DeviceAction::SetMv7ReverbPreset(_)
        | DeviceAction::SetMv7ReverbIntensity(_)
        | DeviceAction::SetMv7LedBehavior(_)
        | DeviceAction::SetMv7LedBrightness(_)
        | DeviceAction::SetMv7LedLiveTheme(_)
        | DeviceAction::SetMv7LedSolidTheme(_)
        | DeviceAction::SetMv7LedPulsingTheme(_)
        | DeviceAction::SetMv7LedSolidRgb(_)
        | DeviceAction::SetMv7LedPulsingRgb(_)
        | DeviceAction::SetMv7LedLiveEdgeRgb(_)
        | DeviceAction::SetMv7LedLiveMiddleRgb(_)
        | DeviceAction::SetMv7LedLiveInteriorRgb(_)
        | DeviceAction::SetEqPreset(_)
        | DeviceAction::SetLedLiveMeter(_)
        | DeviceAction::SetLedNightMode(_) => ActionKind::Write,
    }
}

/// Whether `action` is safe before the device's settings have been read: it
/// reads them, or it doesn't depend on them.
fn reads_no_state(action: &DeviceAction) -> bool {
    match action_kind(action) {
        ActionKind::Connection | ActionKind::PresetFile | ActionKind::FixedWrite => true,
        ActionKind::SavePreset | ActionKind::Write | ActionKind::Writes => false,
    }
}

/// After `action` failed, make the screen show what the device has. A single
/// write the device clearly didn't take (it is gone, or the MV7 rejected it) is
/// undone; the device being gone also covers a re-read after the write, which a
/// reconnect repeats anyway. Anything else (a timeout, a re-read that failed
/// after the write landed, several writes that stopped partway) leaves the
/// device's settings unknown: they are read again, or marked unread until `r`.
fn recover_from_failure(
    app: &mut App,
    device: &Option<ShureDevice>,
    action: &DeviceAction,
    before: Option<protocol::DeviceState>,
    e: &anyhow::Error,
) {
    let mut gone = is_disconnect(e);
    let read_back = match action_kind(action) {
        ActionKind::Connection | ActionKind::PresetFile | ActionKind::SavePreset => None,
        ActionKind::FixedWrite | ActionKind::Write if gone || is_rejected(e) => {
            if let Some(state) = before {
                app.device_state = state;
            }
            None
        }
        ActionKind::FixedWrite | ActionKind::Write | ActionKind::Writes => {
            let read = match device {
                Some(dev) if !gone => match reload_state(app, dev) {
                    Ok(_) => true,
                    Err(read_err) => {
                        gone = is_disconnect(&read_err);
                        false
                    }
                },
                Some(_) | None => false,
            };
            app.state_unread |= !read;
            Some(read)
        }
    };
    if gone {
        app.device_connected = false;
    }
    app.set_err(if gone {
        format!("Device error: {e} — {RECONNECT_HINT}")
    } else {
        match read_back {
            Some(true) => format!("Device error: {e}. Settings read back from the device."),
            Some(false) => format!("Device error: {e}. {STATE_UNREAD_HINT}"),
            None => format!("Device error: {e}"),
        }
    });
}

/// Whether `e` means the device is gone (a HID read or write failed), as
/// opposed to a device that is present but refused a command.
fn is_disconnect(e: &anyhow::Error) -> bool {
    e.downcast_ref::<device::Disconnected>().is_some()
}

fn is_rejected(e: &anyhow::Error) -> bool {
    e.downcast_ref::<device::Rejected>().is_some()
}

/// Re-read the device state. If that fails, the handle has usually gone stale
/// because the device was unplugged; look for the same device again, and if it
/// is back, switch to the new handle and load its state.
///
/// Only a HID failure, or not finding the device again, marks it disconnected.
/// A device that is present but did not answer (an MV7 busy with MOTIV) stays
/// connected; the status bar carries the error.
fn refresh(app: &mut App, device: &mut Option<ShureDevice>) -> Result<()> {
    let Some(dev) = device.as_ref() else {
        app.set_ok("Demo mode — no device to refresh.");
        return Ok(());
    };
    let read_err = match reload_state(app, dev) {
        Ok(skipped) => {
            app.device_connected = true;
            app.set_ok(format!(
                "State refreshed from device.{}",
                skipped_note(&skipped)
            ));
            return Ok(());
        }
        Err(e) => e,
    };
    app.device_connected = !is_disconnect(&read_err);
    reconnect(app, device).map_err(|e| anyhow::anyhow!("{read_err}. Reconnect failed: {e}"))
}

/// Open the same device again and load its state from the new handle. Used by
/// `refresh()` and by the presence poll once an unplugged device is back.
fn reconnect(app: &mut App, device: &mut Option<ShureDevice>) -> Result<()> {
    let Some(dev) = device.as_ref() else {
        return Ok(());
    };
    let reopened = match dev.reopen() {
        Ok(reopened) => reopened,
        Err(e) => {
            if is_disconnect(&e) {
                app.device_connected = false;
            }
            return Err(e);
        }
    };
    let loaded = reload_state(app, &reopened);
    let name = reopened.model.display_name();
    // Keep the new handle even if its readback failed: the old one may be dead.
    *device = Some(reopened);
    match loaded {
        Ok(skipped) => {
            app.device_connected = true;
            app.set_ok(format!(
                "Reconnected to {name} — state loaded.{}",
                skipped_note(&skipped)
            ));
            Ok(())
        }
        Err(e) => {
            app.device_connected = !is_disconnect(&e);
            // The shown state came from the old handle and may be out of date
            // (a factory reset, or another app changing settings while unplugged).
            app.state_unread = true;
            Err(e)
        }
    }
}

/// How often the event loop checks that the device is still plugged in.
const PRESENCE_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Status-bar wording for a lost device, shared by the poll and failed commands.
const RECONNECT_HINT: &str = "it reconnects automatically when plugged back in.";

/// What one presence poll does.
#[derive(Debug, PartialEq)]
enum PollStep {
    Nothing,
    MarkDisconnected,
    /// The device is back but was not seen on the previous poll: give it one
    /// more interval to boot before reading its state.
    WaitForBoot,
    Reconnect,
}

/// Decide a presence poll from whether the app thinks the device is connected,
/// whether it is plugged in now, and whether the previous poll already saw it
/// plugged back in.
fn poll_step(connected: bool, present: bool, seen_back: bool) -> PollStep {
    match (connected, present) {
        (true, true) | (false, false) => PollStep::Nothing,
        (true, false) => PollStep::MarkDisconnected,
        (false, true) if seen_back => PollStep::Reconnect,
        (false, true) => PollStep::WaitForBoot,
    }
}

/// Check that the device is still plugged in, from the OS device list, and
/// reconnect it once it is back. Runs alongside `r`, which does the same on
/// demand. A failed reconnect is retried on the next poll while the device is
/// present and still marked disconnected.
fn poll_presence(app: &mut App, device: &mut Option<ShureDevice>, seen_back: &mut bool) {
    let Some(dev) = device.as_ref() else {
        return; // demo mode
    };
    // A failed enumeration says nothing about the device; try again next poll.
    let Ok(present) = dev.is_present() else {
        return;
    };
    match poll_step(app.device_connected, present, *seen_back) {
        PollStep::Nothing | PollStep::WaitForBoot => {}
        PollStep::MarkDisconnected => mark_disconnected(app, dev.model),
        PollStep::Reconnect => apply_action(app, device, DeviceAction::Reconnect, None),
    }
    *seen_back = present && !app.device_connected;
}

/// Show the device as lost, found by a poll rather than a command.
fn mark_disconnected(app: &mut App, model: DeviceModel) {
    app.device_connected = false;
    app.set_err(format!(
        "{} disconnected — {RECONNECT_HINT}",
        model.display_name()
    ));
}

/// How often the event loop reads the mic's mute state.
const MUTE_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Read the mic's mute state and show it, so a press of its mute button (or
/// another app muting it) shows without `r`. Nothing changes when the device
/// gives no usable answer; the next poll asks again. A HID failure marks the
/// device disconnected, as the presence poll does. Skipped in demo mode, while
/// disconnected, and while the state is unread, where `r` is the way back.
fn poll_mute(app: &mut App, device: &Option<ShureDevice>) {
    let Some(dev) = device.as_ref() else {
        return; // demo mode
    };
    if !app.device_connected || app.state_unread {
        return;
    }
    match dev.read_mute() {
        Ok(Some(muted)) if muted != app.device_state.muted => {
            app.device_state.muted = muted;
            app.set_ok(format!(
                "Mute → {} (changed on the device)",
                if muted { "ON" } else { "OFF" }
            ));
        }
        Err(e) if is_disconnect(&e) => mark_disconnected(app, dev.model),
        // Anything else leaves the mute state unknown for now, not changed.
        Ok(Some(_) | None) | Err(_) => {}
    }
}

/// Status-bar suffix for optional settings a readback couldn't read, so they
/// can be seen and reported without writing to the terminal.
fn skipped_note(skipped: &[String]) -> String {
    match skipped {
        [] => String::new(),
        [only] => format!(" Couldn't read an optional setting: {only}."),
        [first, ..] => format!(
            " Couldn't read {} optional settings, first: {first}.",
            skipped.len()
        ),
    }
}

/// Replace the app's state with a full readback from the device. Returns the
/// optional settings it couldn't read, for the caller's status message. If
/// the readback fails, the app's state is left as it was.
fn reload_state(app: &mut App, dev: &ShureDevice) -> Result<Vec<String>> {
    let readback = dev.get_state()?;
    app.device_state = readback.state;
    app.state_unread = false;
    app.device_state.serial_number = dev.serial_number.clone();
    Ok(readback.skipped)
}

/// The MV7 stores mode, mic position and tone as one setting, and changing it
/// changes other state too: entering Auto Level resets EQ and compressor, and
/// leaving it reports a new gain. Re-read the whole state afterwards so the UI
/// shows what the mic actually has. No-op for other models and in demo mode.
fn resync_mv7_state(app: &mut App, device: &Option<ShureDevice>) -> Result<()> {
    match device {
        Some(dev) => match dev.model {
            DeviceModel::Mv7 => {
                let skipped = reload_state(app, dev)?;
                // Keep the caller's message ("Mode → …") and add the note to it.
                app.status_message.push_str(&skipped_note(&skipped));
                Ok(())
            }
            DeviceModel::Mvx2u
            | DeviceModel::Mvx2uGen2
            | DeviceModel::Mv6
            | DeviceModel::Mv6Gen2
            | DeviceModel::Mv7Plus => Ok(()),
        },
        None => Ok(()),
    }
}

fn send_if_connected<T: Default, F>(device: &Option<ShureDevice>, f: F) -> Result<T>
where
    F: FnOnce(&ShureDevice) -> Result<T>,
{
    match device {
        Some(dev) => f(dev),
        None => Ok(T::default()),
    }
}

/// Status suffix naming the settings a preset load could not write.
fn refused_note(refused: &[String]) -> String {
    if refused.is_empty() {
        String::new()
    } else {
        format!(" The MV7 refused: {}.", refused.join(", "))
    }
}

/// Status suffix for a saved Mute Button the load left alone because the
/// device's couldn't be read (`PresetSlot::apply_to_device_state()`).
fn unread_note(slot: &PresetSlot, state: &protocol::DeviceState, model: DeviceModel) -> String {
    if !model.has_mute_button()
        || slot.mute_btn_disabled.is_none()
        || state.mute_btn_disabled.is_some()
    {
        return String::new();
    }
    match state.mute_button_needs_firmware(model) {
        Some(firmware) => format!(" Mute Button not loaded: it needs firmware {firmware}+."),
        None => " Mute Button not loaded: its state could not be read.".into(),
    }
}

/// Apply `slot` to `state`, fitted to `model`. Returns whether the preset's
/// Monitor Mix applies, for `apply_preset_to_device()`.
fn apply_preset_to_state(
    slot: &PresetSlot,
    state: &mut protocol::DeviceState,
    model: DeviceModel,
) -> bool {
    let monitor_mix_before = state.monitor_mix;
    slot.apply_to_device_state(state);
    // A balance and a Mic Level share the field but not the meaning: keep the
    // device's own Monitor Mix rather than translate one into the other.
    let monitor_mix_applies = !slot
        .monitor_mix_is_mic_level
        .is_some_and(|mic_level| mic_level != model.has_playback_mix());
    if !monitor_mix_applies {
        state.monitor_mix = monitor_mix_before;
    }
    // Show the gain and EQ the device will actually get: presets are
    // shared across models, so one saved elsewhere may be out of
    // range or off this model's step grid.
    state.gain_tenths = model.snap_gain_tenths(state.gain_tenths);
    for band in &mut state.eq_bands {
        band.gain_db = model.snap_eq_gain_tenths(band.gain_db);
    }
    // A lock flag from a model with Gain Lock would sit hidden here
    // and be saved into this model's presets, then lock the gain
    // wherever one of those is loaded.
    if !model.has_gain_lock() {
        state.mv6_gain_locked = false;
    }
    monitor_mix_applies
}

/// Writes a preset's Playback Level when it saved one. A preset's 0 means "not
/// saved", so the shown level (possibly stale, or 0 if it was never read and
/// would silence playback) is never written in its place.
fn write_playback_level(
    d: &ShureDevice,
    slot: &PresetSlot,
    state: &protocol::DeviceState,
) -> Result<()> {
    if slot.playback_mix > 0 {
        d.set_mv7_playback_mix(state.playback_mix)?;
    }
    Ok(())
}

/// Writes a preset's Mute Button when it saved one and the device's was read
/// (`unread_note()` says when it wasn't).
fn write_mute_button(
    d: &ShureDevice,
    slot: &PresetSlot,
    state: &protocol::DeviceState,
) -> Result<()> {
    if let (Some(_), Some(disabled)) = (slot.mute_btn_disabled, state.mute_btn_disabled) {
        d.set_mv6_mute_btn_disable(disabled)?;
    }
    Ok(())
}

/// Send `slot`'s settings to the device, as `apply_preset_to_state()` put them
/// in `state`. Skipped: settings the preset didn't save (from a model without
/// them), which the device keeps rather than get the shown state's copy, which
/// may be stale; Monitor Mix when `write_monitor_mix` is false (the preset's was
/// of the other kind); and a Playback Level of 0 (see `write_playback_level()`).
/// Returns the settings the device refused; only the MV7 refuses any, and it
/// is sent only the settings that differ (`ShureDevice::load_mv7_preset()`).
fn apply_preset_to_device(
    device: &Option<ShureDevice>,
    slot: &PresetSlot,
    state: &protocol::DeviceState,
    model: DeviceModel,
    write_monitor_mix: bool,
) -> Result<Vec<String>> {
    send_if_connected(device, |d| match model {
        // Built on the mic's own fresh readback, so an unsaved setting keeps its value.
        DeviceModel::Mv7 => d.load_mv7_preset(
            |target| {
                apply_preset_to_state(slot, target, model);
            },
            write_monitor_mix,
        ),
        DeviceModel::Mvx2u
        | DeviceModel::Mvx2uGen2
        | DeviceModel::Mv6
        | DeviceModel::Mv6Gen2
        | DeviceModel::Mv7Plus => {
            apply_binary_preset(d, slot, state, model, write_monitor_mix).map(|()| Vec::new())
        }
    })
}

/// `apply_preset_to_device()` for the binary models.
fn apply_binary_preset(
    d: &ShureDevice,
    slot: &PresetSlot,
    state: &protocol::DeviceState,
    model: DeviceModel,
    write_monitor_mix: bool,
) -> Result<()> {
    d.set_mode(
        state.mode == InputMode::Auto,
        state.auto_position,
        state.auto_tone,
    )?;
    // Order: mode → gain → HPF. Mute is live state, which a preset leaves alone.
    // A gain-locked device ignores gain writes (tested on the MV6, MV6 Gen 2
    // and MVX2U Gen 2, 2026-10-10), so unlock, write the gain, then lock
    // straight away so a later failed write can't leave the device unlocked
    // behind a locked UI. A preset without Gain Lock re-locks to the shown
    // state's setting.
    if model.has_gain_lock() {
        d.set_mv6_gain_lock(false)?;
    }
    d.set_gain(state.gain_tenths)?;
    if model.has_gain_lock() {
        d.set_mv6_gain_lock(state.mv6_gain_locked)?;
    }
    if model.has_hpf() && slot.hpf.is_some() {
        d.set_hpf(&state.hpf)?;
    }
    match model {
        DeviceModel::Mvx2u => {
            if slot.auto_position.is_some() {
                d.set_auto_position(state.auto_position, state.auto_tone)?;
            }
            if slot.auto_tone.is_some() {
                d.set_auto_tone(state.auto_position, state.auto_tone)?;
            }
            if slot.auto_gain.is_some() {
                d.set_auto_gain(&state.auto_gain)?;
            }
            if slot.phantom_power.is_some() {
                d.set_phantom(state.phantom_power)?;
            }
            write_limiter_and_compressor(d, slot, state)?;
            if slot.eq_enabled.is_some() {
                d.set_eq_enable(state.eq_enabled)?;
            }
            write_eq_bands(d, slot, state, true)?;
        }
        DeviceModel::Mvx2uGen2 => {
            if slot.phantom_power.is_some() {
                d.set_phantom(state.phantom_power)?;
            }
            write_playback_level(d, slot, state)?;
            write_limiter_and_compressor(d, slot, state)?;
            write_voice_settings(d, slot, state)?;
            write_eq_bands(d, slot, state, false)?;
        }
        DeviceModel::Mv6 => write_mv6_settings(d, slot, state)?,
        DeviceModel::Mv6Gen2 => {
            write_mv6_settings(d, slot, state)?;
            if slot.auto_gain.is_some() {
                d.set_auto_gain(&state.auto_gain)?;
            }
            if slot.adaptation_rate_fast.is_some() {
                d.set_adaptation_rate(state.adaptation_rate_fast)?;
            }
            if slot.denoiser_level.is_some() {
                d.set_denoiser_level(state.denoiser_level)?;
            }
            write_eq_bands(d, slot, state, false)?;
            if slot.tone_advanced.is_some() {
                d.set_tone_mode(state.tone_advanced)?;
            }
        }
        DeviceModel::Mv7 => {
            anyhow::bail!("The MV7 loads presets through ShureDevice::load_mv7_preset()")
        }
        DeviceModel::Mv7Plus => {
            write_voice_settings(d, slot, state)?;
            write_mute_button(d, slot, state)?;
            write_limiter_and_compressor(d, slot, state)?;
            write_playback_level(d, slot, state)?;
            // The MV7+ saves its reverb and LED settings together.
            if slot.reverb_on_output.is_some() {
                d.set_mv7_reverb_output(state.reverb_on_output)?;
                d.set_mv7_reverb_monitor(state.reverb_monitoring)?;
                d.set_mv7_reverb_type(&state.reverb_type)?;
                d.set_mv7_reverb_intensity(state.reverb_intensity)?;
            }
            if slot.led_behavior.is_some() {
                d.set_mv7_led_behavior(state.led_behavior)?;
                d.set_mv7_led_brightness(state.led_brightness)?;
                d.set_mv7_led_live_theme(state.led_live_theme)?;
                d.set_mv7_led_solid_theme(state.led_solid_theme)?;
                d.set_mv7_led_pulsing_theme(state.led_pulsing_theme)?;
                d.set_mv7_led_solid_color(state.led_solid_rgb)?;
                d.set_mv7_led_pulsing_color(state.led_pulsing_rgb)?;
                d.set_mv7_led_live_edge(state.led_live_edge_rgb)?;
                d.set_mv7_led_live_middle(state.led_live_middle_rgb)?;
                d.set_mv7_led_live_interior(state.led_live_interior_rgb)?;
            }
        }
    }
    if write_monitor_mix {
        d.set_monitor_mix(state.monitor_mix)?;
    }
    Ok(())
}

/// The MV6 settings the MV6 Gen 2 shares: Denoiser, Popper Stopper, Tone,
/// Mute Button and Playback Level, where the preset saved them.
fn write_mv6_settings(
    d: &ShureDevice,
    slot: &PresetSlot,
    state: &protocol::DeviceState,
) -> Result<()> {
    write_voice_settings(d, slot, state)?;
    write_mute_button(d, slot, state)?;
    write_playback_level(d, slot, state)
}

/// Limiter and Compressor, where the preset saved them.
fn write_limiter_and_compressor(
    d: &ShureDevice,
    slot: &PresetSlot,
    state: &protocol::DeviceState,
) -> Result<()> {
    if slot.limiter_enabled.is_some() {
        d.set_limiter(state.limiter_enabled)?;
    }
    if slot.compressor.is_some() {
        d.set_compressor(&state.compressor)?;
    }
    Ok(())
}

/// Denoiser, Popper Stopper and Tone, where the preset saved them.
fn write_voice_settings(
    d: &ShureDevice,
    slot: &PresetSlot,
    state: &protocol::DeviceState,
) -> Result<()> {
    if slot.denoiser_enabled.is_some() {
        d.set_mv6_denoiser(state.denoiser_enabled)?;
    }
    if slot.popper_stopper_enabled.is_some() {
        d.set_mv6_popper_stopper(state.popper_stopper_enabled)?;
    }
    if slot.tone.is_some() {
        d.set_mv6_tone(state.tone)?;
    }
    Ok(())
}

/// The five EQ bands' gains, and on a model with per-band switches (the MVX2U
/// Gen 1, `band_switches`) each band's switch where the preset saved one. Other
/// models have no switch address, so it is never sent to them, even from a Gen 1
/// preset or an older file that holds switches.
fn write_eq_bands(
    d: &ShureDevice,
    slot: &PresetSlot,
    state: &protocol::DeviceState,
    band_switches: bool,
) -> Result<()> {
    let Some(saved) = &slot.eq_bands else {
        return Ok(());
    };
    for (band, (eq, saved)) in state.eq_bands.iter().zip(saved).enumerate() {
        if band_switches && saved.enabled.is_some() {
            d.set_eq_band_enable(band, eq.enabled)?;
        }
        d.set_eq_band_gain(band, eq.gain_db)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fake_hid::FakeDevice;
    use protocol::{DeviceState, EqBand, EqPreset};

    #[test]
    fn a_failed_write_is_undone_only_when_it_clearly_did_not_land() {
        let before = DeviceState::default();
        let shown = DeviceState {
            muted: true,
            ..before.clone()
        };
        let mut app = App {
            device_state: shown.clone(),
            ..App::default()
        };
        let mute = DeviceAction::SetMute(true);

        // The MV7 rejected it: the old value is certain.
        let rejected = anyhow::Error::from(device::Rejected("rejected".into()));
        recover_from_failure(&mut app, &None, &mute, Some(before.clone()), &rejected);
        assert!(!app.device_state.muted);
        assert!(!app.state_unread);

        // Unplugged: the write never reached it.
        app.device_state = shown.clone();
        let gone = anyhow::Error::from(device::Disconnected("gone".into()));
        recover_from_failure(&mut app, &None, &mute, Some(before.clone()), &gone);
        assert!(!app.device_state.muted);
        assert!(!app.device_connected);

        // No reply: the mic may have taken it, so nothing is assumed.
        app.device_connected = true;
        app.device_state = shown.clone();
        let timeout = anyhow::anyhow!("No reply from the MV7");
        recover_from_failure(&mut app, &None, &mute, Some(before.clone()), &timeout);
        assert!(app.state_unread, "writes wait for a readback");
        assert!(app.status_message.contains(STATE_UNREAD_HINT));

        // With the device still there, it is read back instead.
        let fake = FakeDevice::new(DeviceModel::Mv6Gen2);
        let mut app = App {
            device_model: DeviceModel::Mv6Gen2,
            device_state: shown,
            ..App::default()
        };
        recover_from_failure(&mut app, &Some(fake.open()), &mute, Some(before), &timeout);
        assert!(!app.device_state.muted);
        assert!(!app.state_unread);
        assert!(app.status_message.contains("read back"));
    }

    #[test]
    fn a_preset_load_that_stops_partway_is_not_rolled_back() {
        // Mute and gain may already have landed; the state is unknown.
        let loaded = DeviceState {
            gain_tenths: 300,
            ..DeviceState::default()
        };
        let mut app = App {
            device_state: loaded.clone(),
            ..App::default()
        };
        let gone = anyhow::Error::from(device::Disconnected("gone".into()));
        recover_from_failure(
            &mut app,
            &None,
            &DeviceAction::LoadPreset(0),
            Some(DeviceState::default()),
            &gone,
        );
        assert_eq!(app.device_state, loaded);
        assert!(app.state_unread, "the reconnect reads it back");

        // A preset file failing to save touches no device state.
        let mut app = App::default();
        let disk = anyhow::anyhow!("disk full");
        recover_from_failure(&mut app, &None, &DeviceAction::SavePreset(0), None, &disk);
        assert!(!app.state_unread);
    }

    /// Findings of the f3281ae review: Gen 1 band switches went to models
    /// without them, and a preset without a Playback Level wrote the shown one.
    #[test]
    fn a_preset_load_writes_nothing_the_model_lacks_or_the_preset_left_out() {
        // A Gen 1 preset holds band switches; the MV6 Gen 2 has no address for them.
        let gen1 =
            PresetSlot::from_device_state("Gen 1", &DeviceState::default(), DeviceModel::Mvx2u);
        assert!(
            gen1.eq_bands
                .is_some_and(|bands| bands[0].enabled.is_some())
        );
        let mv6_gen2 = FakeDevice::with_state(
            DeviceModel::Mv6Gen2,
            DeviceState {
                playback_mix: 70,
                ..FakeDevice::seed_state(DeviceModel::Mv6Gen2)
            },
        );
        // The shown Playback Level is stale; the preset (from a Gen 1) has none.
        let shown = DeviceState {
            playback_mix: 20,
            ..DeviceState::default()
        };
        let after = load_on_fake(&mv6_gen2, &gen1, shown);
        assert_eq!(after.playback_mix, 70);
        let unknown: Vec<String> = mv6_gen2
            .log_since(0)
            .into_iter()
            .filter_map(|packet| match packet.kind {
                fake_hid::PacketKind::Set { feature, .. }
                    if feature.starts_with("unrecognised") =>
                {
                    Some(feature)
                }
                _ => None,
            })
            .collect();
        assert!(
            unknown.is_empty(),
            "SETs to addresses the MV6 Gen 2 lacks: {unknown:?}"
        );
    }

    #[test]
    fn a_re_read_that_finds_the_device_gone_says_so() {
        let fake = FakeDevice::new(DeviceModel::Mv6Gen2);
        let device = Some(fake.open());
        let mut app = App {
            device_model: DeviceModel::Mv6Gen2,
            ..App::default()
        };
        fake.faults_mut(|faults| faults.fail_io = true);
        let timeout = anyhow::anyhow!("No reply");
        recover_from_failure(
            &mut app,
            &device,
            &DeviceAction::SetMute(true),
            None,
            &timeout,
        );
        assert!(!app.device_connected);
        assert!(app.state_unread);
        assert!(
            app.status_message.contains(RECONNECT_HINT),
            "{}",
            app.status_message
        );
    }

    /// Finding of the 09e9569 review: a wheel adjustment still pending when the
    /// next key changed the screen took that key's change in with it.
    #[test]
    fn a_pending_wheel_adjustment_goes_out_before_the_next_key() {
        let fake = FakeDevice::new(DeviceModel::Mv7);
        let mut device = Some(fake.open());
        let mut app = App {
            device_model: DeviceModel::Mv7,
            ..App::default()
        };
        load_initial_state(&mut app, device.as_ref());
        let mut hits = mouse::HitMap::default();
        hits.control(ratatui::layout::Rect::new(1, 5, 70, 3), app::Focus::Gain);
        let now = Instant::now();
        let mut loop_state = LoopState::new(now);
        let key = |code| {
            Some(Event::Key(crossterm::event::KeyEvent::new(
                code,
                KeyModifiers::NONE,
            )))
        };
        let wheel = Some(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 30,
            row: 6,
            modifiers: KeyModifiers::NONE,
        }));

        step(
            &mut app,
            &mut device,
            &hits,
            true,
            wheel,
            now,
            &mut loop_state,
        );
        assert!(loop_state.pending_scroll.is_some());
        // ↑ to Mute writes the pending gain first.
        step(
            &mut app,
            &mut device,
            &hits,
            true,
            key(KeyCode::Up),
            now,
            &mut loop_state,
        );
        assert!(loop_state.pending_scroll.is_none());
        assert_eq!(app.focus, app::Focus::Mute);
        // The mic rejects the mute: only the mute is undone, the gain stands.
        fake.faults_mut(|faults| faults.refuse = 1);
        step(
            &mut app,
            &mut device,
            &hits,
            true,
            key(KeyCode::Enter),
            now,
            &mut loop_state,
        );
        assert!(!app.device_state.muted);
        assert_eq!(app.device_state.gain_tenths, 255);
        let mic = device
            .as_ref()
            .expect("device")
            .get_state()
            .expect("readback")
            .state;
        assert!(!mic.muted);
        assert_eq!(mic.gain_tenths, 255);
    }

    /// Load `slot` on `fake` the way the Presets tab does, from a shown state
    /// that may be stale, and return what the device then has.
    fn load_on_fake(fake: &FakeDevice, slot: &PresetSlot, shown: DeviceState) -> DeviceState {
        let device = Some(fake.open());
        let model = fake.model();
        let mut state = shown;
        let write_monitor_mix = apply_preset_to_state(slot, &mut state, model);
        apply_preset_to_device(&device, slot, &state, model, write_monitor_mix).expect("load");
        let dev = device.as_ref().expect("device");
        dev.get_state().expect("readback").state
    }

    #[test]
    fn unread_mute_button_is_never_saved_loaded_or_written() {
        let model = DeviceModel::Mv7Plus;
        let fake = FakeDevice::new(model);
        fake.faults_mut(|faults| faults.refused_get = Some("MUTE_BUTTON".into()));
        let mut device = Some(fake.open());
        let readback = device
            .as_ref()
            .expect("device")
            .get_state()
            .expect("readback");
        assert_eq!(readback.state.mute_btn_disabled, None);
        assert_eq!(readback.state.gain_tenths, 240);
        assert_eq!(readback.state.factory_serial, "FAKE0001");
        // The fake's firmware isn't one known to predate the setting, so this
        // is a read failure the status bar reports.
        assert_eq!(readback.skipped.len(), 1);
        assert!(readback.skipped[0].contains("Mute Button"));

        let saved = PresetSlot::from_device_state("Unknown button", &readback.state, model);
        assert_eq!(saved.mute_btn_disabled, None);
        let mut app = App {
            device_model: model,
            device_state: readback.state,
            focus: app::Focus::MuteBtnDisable,
            ..App::default()
        };
        let start = fake.log_len();
        assert!(app.toggle_focused().is_none());
        assert_eq!(
            app.status_message,
            "Mute Button unavailable: its state could not be read"
        );
        // With nothing read yet, the real fix is a readback, so say that instead.
        app.state_unread = true;
        assert!(app.toggle_focused().is_none());
        assert_eq!(app.status_message, STATE_UNREAD_HINT);
        app.state_unread = false;
        apply_action(
            &mut app,
            &mut device,
            DeviceAction::SetMv6MuteBtnDisable(true),
            None,
        );
        assert!(fake.log_since(start).is_empty());

        let slot = PresetSlot {
            mute_btn_disabled: Some(true),
            ..saved
        };
        apply_preset_to_state(&slot, &mut app.device_state, model);
        assert_eq!(app.device_state.mute_btn_disabled, None);
        assert_eq!(
            unread_note(&slot, &app.device_state, model),
            " Mute Button not loaded: its state could not be read."
        );
        let old_firmware = DeviceState {
            firmware_version: "1.3.0.13".into(),
            ..app.device_state.clone()
        };
        assert_eq!(
            unread_note(&slot, &old_firmware, model),
            " Mute Button not loaded: it needs firmware 1.4.0+."
        );
        // A model without the setting reads none, and has nothing to load it into.
        assert_eq!(unread_note(&slot, &old_firmware, DeviceModel::Mvx2u), "");
        apply_preset_to_device(&device, &slot, &app.device_state, model, true)
            .expect("load other settings");
        assert!(!fake.log_since(start).iter().any(|packet| {
            matches!(&packet.kind, fake_hid::PacketKind::Set { feature, .. }
                if feature == "MUTE_BUTTON")
        }));
        fake.faults_mut(|faults| faults.refused_get = None);
        let state = device
            .as_ref()
            .expect("device")
            .get_state()
            .expect("readback")
            .state;
        assert_eq!(state.mute_btn_disabled, Some(false));
        assert_eq!(unread_note(&slot, &state, model), "");
    }

    #[test]
    fn a_preset_leaves_settings_it_did_not_save_on_the_device() {
        // An MVX2U Gen 2 preset has no Mute Button or Denoiser level. The shown
        // state is stale (defaults), but the MV6 Gen 2 keeps its own values.
        let gen2 =
            PresetSlot::from_device_state("Gen 2", &DeviceState::default(), DeviceModel::Mvx2uGen2);
        let mv6_gen2 = FakeDevice::with_state(
            DeviceModel::Mv6Gen2,
            DeviceState {
                mute_btn_disabled: Some(true),
                denoiser_level: 80,
                ..FakeDevice::seed_state(DeviceModel::Mv6Gen2)
            },
        );
        let after = load_on_fake(&mv6_gen2, &gen2, DeviceState::default());
        assert_eq!(after.mute_btn_disabled, Some(true));
        assert_eq!(after.denoiser_level, 80);

        // Its EQ bands carry no on/off switch, which only the Gen 1 has.
        let gen1 = FakeDevice::with_state(
            DeviceModel::Mvx2u,
            DeviceState {
                eq_bands: [EqBand {
                    enabled: true,
                    gain_db: 0,
                }; 5],
                ..FakeDevice::seed_state(DeviceModel::Mvx2u)
            },
        );
        let after = load_on_fake(&gen1, &gen2, DeviceState::default());
        assert!(after.eq_bands.iter().all(|band| band.enabled));

        // The MV7 builds the preset on its own readback: an MV7+ preset has no
        // EQ preset, so the mic keeps Presence Boost however stale the screen is.
        let mv7plus =
            PresetSlot::from_device_state("MV7+", &DeviceState::default(), DeviceModel::Mv7Plus);
        let mv7 = FakeDevice::with_state(
            DeviceModel::Mv7,
            DeviceState {
                eq_preset: EqPreset::PresenceBoost,
                ..FakeDevice::seed_state(DeviceModel::Mv7)
            },
        );
        let after = load_on_fake(&mv7, &mv7plus, DeviceState::default());
        assert_eq!(after.eq_preset, EqPreset::PresenceBoost);
    }

    #[test]
    fn cli_parses_mute_status_and_preset_slot() {
        let cli = Cli::try_parse_from(["shurectl", "--mute", "status"]).expect("mute status");
        assert_eq!(cli.mute, Some(MuteAction::Status));
        let cli = Cli::try_parse_from(["shurectl", "--preset", "4"]).expect("preset 4");
        assert_eq!(cli.preset, Some(4));
        for bad in [["shurectl", "--preset", "0"], ["shurectl", "--preset", "5"]] {
            assert!(
                Cli::try_parse_from(bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
        assert!(Cli::try_parse_from(["shurectl", "--preset", "1", "--mute"]).is_err());
    }

    fn device_name_focused(model: DeviceModel, name: &str) -> App {
        let mut app = App {
            device_model: model,
            active_tab: app::Tab::Info,
            focus: app::Focus::DeviceName,
            ..App::default()
        };
        app.device_state.device_name = name.to_owned();
        app
    }

    #[test]
    fn empty_device_name_is_rejected() {
        let mut app = device_name_focused(DeviceModel::Mvx2uGen2, "Unknown");
        handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            app.name_draft.is_empty(),
            "\"Unknown\" is not a name to edit"
        );
        assert!(handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).is_none());
        assert!(!app.editing_device_name);
        assert!(app.status_is_error);
    }

    #[test]
    fn device_name_is_not_editable_without_rename_support() {
        let mut app = device_name_focused(DeviceModel::Mv7, "Studio");
        assert!(handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).is_none());
        assert!(!app.editing_device_name);
    }

    #[test]
    fn writes_wait_until_the_device_state_is_read() {
        let mut app = App {
            state_unread: true,
            ..App::default()
        };
        app.device_state.mode = InputMode::Manual;
        app.focus = app::Focus::Gain;
        let shown = app.device_state.clone();
        assert!(handle_key(&mut app, KeyCode::Right, KeyModifiers::NONE).is_none());
        assert!(app.status_is_error);
        assert!(
            app.status_message.contains("press r"),
            "{}",
            app.status_message
        );
        assert_eq!(app.device_state, shown, "the refused change is undone");
        // Toggling the mode also moves focus; both are undone.
        app.focus = app::Focus::Mode;
        assert!(handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).is_none());
        assert_eq!(app.device_state, shown);
        assert_eq!(app.focus, app::Focus::Mode);
        assert!(matches!(
            handle_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE),
            Some(DeviceAction::Refresh)
        ));
        // A write that skipped handle_key() (a queued wheel adjustment) is
        // refused too.
        app.set_ok("");
        apply_action(&mut app, &mut None, DeviceAction::SetGain(100), None);
        assert_eq!(app.status_message, STATE_UNREAD_HINT);
    }

    #[test]
    fn refused_note_names_the_refused_settings() {
        assert_eq!(refused_note(&[]), "");
        assert_eq!(
            refused_note(&["Mode (locked)".to_string(), "Compressor".to_string()]),
            " The MV7 refused: Mode (locked), Compressor."
        );
    }

    fn editing_preset_name(name: &str) -> App {
        let mut presets: [Option<PresetSlot>; presets::PRESET_COUNT] = Default::default();
        presets[0] = Some(PresetSlot::from_device_state(
            name,
            &protocol::DeviceState::default(),
            DeviceModel::Mvx2u,
        ));
        App {
            presets,
            editing_preset_name: true,
            editing_preset_index: 0,
            name_draft: name.to_owned(),
            ..App::default()
        }
    }

    #[test]
    fn too_small_terminal_keys_only_quit_and_keep_a_name_draft() {
        // `q` would be typed into the name, so it neither quits nor edits.
        let mut app = editing_preset_name("Quiet");
        app.armed_preset_action = Some((0, KeyCode::Char('d')));
        handle_key_too_small(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(!app.should_quit);
        assert_eq!(app.name_draft, "Quiet");
        assert!(app.editing_preset_name);
        assert_eq!(app.armed_preset_action, None);
        handle_key_too_small(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.should_quit);

        let mut app = App::default();
        handle_key_too_small(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.should_quit);
        handle_key_too_small(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(app.should_quit);
    }

    /// A Gen 1 balance of 0 (all mic) must not load as a Mic Level of 0, which
    /// mutes the mic in the headphones, and the reverse. Older presets that
    /// don't say which kind they hold load as before.
    #[test]
    fn preset_monitor_mix_loads_only_onto_the_same_kind() {
        let preset = |mic_level: Option<bool>| PresetSlot {
            monitor_mix_is_mic_level: mic_level,
            ..PresetSlot::from_device_state(
                "P",
                &protocol::DeviceState::default(),
                DeviceModel::Mvx2u,
            )
        };
        // Each case: (preset, model loaded onto, Monitor Mix applies?).
        let balance = preset(Some(false));
        let mic_level = preset(Some(true));
        let older = preset(None);
        for (slot, model, applies) in [
            (&balance, DeviceModel::Mvx2uGen2, false),
            (&mic_level, DeviceModel::Mvx2u, false),
            (&balance, DeviceModel::Mv7, true),
            (&mic_level, DeviceModel::Mv7Plus, true),
            (&older, DeviceModel::Mvx2uGen2, true),
        ] {
            let mut state = protocol::DeviceState {
                monitor_mix: 80,
                ..protocol::DeviceState::default()
            };
            assert_eq!(
                apply_preset_to_state(slot, &mut state, model),
                applies,
                "{model:?}"
            );
            // The preset holds 0; a skipped one leaves the device's 80.
            let expected = if applies { 0 } else { 80 };
            assert_eq!(state.monitor_mix, expected, "{model:?}");
        }
    }

    /// A preset saved on an MV6 with gain lock on must not freeze the gain of a
    /// model that has no Gain Lock control to undo it.
    #[test]
    fn mv6_gain_lock_preset_leaves_mv7plus_gain_adjustable() {
        let mv6_state = protocol::DeviceState {
            mode: InputMode::Manual,
            mv6_gain_locked: true,
            ..protocol::DeviceState::default()
        };
        let mut presets: [Option<PresetSlot>; presets::PRESET_COUNT] = Default::default();
        presets[0] = Some(PresetSlot::from_device_state(
            "MV6",
            &mv6_state,
            DeviceModel::Mv6,
        ));
        let mut app = App {
            device_model: DeviceModel::Mv7Plus,
            presets,
            ..App::default()
        };
        apply_action(&mut app, &mut None, DeviceAction::LoadPreset(0), None);
        assert!(
            !app.device_state.mv6_gain_locked,
            "the flag must not linger to be saved into MV7+ presets"
        );
        app.focus = app::Focus::Gain;
        assert!(!app.gain_locked());
        assert!(matches!(
            app.adjust_focused(-1),
            Some(DeviceAction::SetGain(_))
        ));
    }

    /// Without a device (demo mode) Refresh has nothing to read or reconnect.
    #[test]
    fn refresh_without_device_reports_demo_mode() {
        let mut app = App::default();
        let mut device = None;
        apply_action(&mut app, &mut device, DeviceAction::Refresh, None);
        assert!(!app.status_is_error);
        assert_eq!(app.status_message, "Demo mode — no device to refresh.");
        assert!(device.is_none());
    }

    #[test]
    fn hid_failures_count_as_disconnects_and_refusals_do_not() {
        let gone = anyhow::Error::new(device::Disconnected("HID read failed".into()));
        assert!(is_disconnect(&gone));
        assert!(is_disconnect(&gone.context("Could not read device state")));
        assert!(!is_disconnect(&anyhow::anyhow!(
            "The MV7 rejected \"lock on\""
        )));
    }

    #[test]
    fn presence_poll_marks_loss_and_reconnects_after_one_more_poll() {
        use PollStep::*;
        for (connected, present, seen_back, expected) in [
            (true, true, false, Nothing),
            (true, false, false, MarkDisconnected),
            (false, false, false, Nothing),
            (false, false, true, Nothing),
            (false, true, false, WaitForBoot),
            (false, true, true, Reconnect),
        ] {
            assert_eq!(
                poll_step(connected, present, seen_back),
                expected,
                "connected={connected} present={present} seen_back={seen_back}"
            );
        }
    }

    #[test]
    fn presence_poll_does_nothing_in_demo_mode() {
        let mut app = App::default();
        let mut seen_back = false;
        poll_presence(&mut app, &mut None, &mut seen_back);
        assert!(app.device_connected);
        assert_eq!(app.status_message, App::default().status_message);
    }

    #[test]
    fn skipped_note_is_empty_or_names_the_first_query() {
        assert_eq!(skipped_note(&[]), "");
        assert_eq!(
            skipped_note(&["feature 0x01 0x99".into()]),
            " Couldn't read an optional setting: feature 0x01 0x99."
        );
        assert_eq!(
            skipped_note(&["feature 0x01 0x99".into(), "feature 0x02 0x77".into()]),
            " Couldn't read 2 optional settings, first: feature 0x01 0x99."
        );
    }

    fn app_with_preset(model: DeviceModel, saved: protocol::DeviceState) -> App {
        let mut presets: [Option<PresetSlot>; presets::PRESET_COUNT] = Default::default();
        presets[0] = Some(PresetSlot::from_device_state("saved", &saved, model));
        App {
            device_model: model,
            presets,
            ..App::default()
        }
    }

    fn gen2_eq_state() -> protocol::DeviceState {
        let mut state = protocol::DeviceState::default();
        state.eq_bands[0].gain_db = 15;
        state.eq_bands[1].gain_db = -25;
        state
    }

    #[test]
    fn gen2_eq_preset_is_snapped_to_the_gen1_grid_on_load() {
        let mut app = app_with_preset(DeviceModel::Mvx2u, gen2_eq_state());
        apply_action(&mut app, &mut None, DeviceAction::LoadPreset(0), None);
        assert_eq!(app.device_state.eq_bands[0].gain_db, 20);
        assert_eq!(app.device_state.eq_bands[1].gain_db, -20);

        let mut app = app_with_preset(DeviceModel::Mvx2uGen2, gen2_eq_state());
        apply_action(&mut app, &mut None, DeviceAction::LoadPreset(0), None);
        assert_eq!(app.device_state.eq_bands[0].gain_db, 15);
        assert_eq!(app.device_state.eq_bands[1].gain_db, -25);
    }

    /// Loading a preset that switches the mode keeps the cursor on its slot.
    #[test]
    fn loading_a_preset_that_changes_mode_keeps_presets_focus() {
        let saved = protocol::DeviceState {
            mode: InputMode::Manual,
            ..protocol::DeviceState::default()
        };
        let mut app = App {
            active_tab: app::Tab::Presets,
            focus: app::Focus::PresetActions(0),
            ..app_with_preset(DeviceModel::Mvx2u, saved)
        };
        assert_eq!(app.device_state.mode, InputMode::Auto);
        apply_action(&mut app, &mut None, DeviceAction::LoadPreset(0), None);
        assert_eq!(app.device_state.mode, InputMode::Manual);
        assert_eq!(app.active_tab, app::Tab::Presets);
        assert_eq!(app.focus, app::Focus::PresetActions(0));
    }

    // ── Preset loads, every source on every model ─────────────────────────────

    const PRESET_LOADS_GOLDEN: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/preset-loads.golden"
    );

    const ALL_MODELS: [DeviceModel; 6] = [
        DeviceModel::Mvx2u,
        DeviceModel::Mvx2uGen2,
        DeviceModel::Mv6,
        DeviceModel::Mv6Gen2,
        DeviceModel::Mv7,
        DeviceModel::Mv7Plus,
    ];

    /// Every setting away from both `DeviceState::default()` and the fake's
    /// seed, so each one a preset saves shows up as a write when it loads.
    fn distinct_state() -> DeviceState {
        use protocol::{
            AutoGain, AutoTone, CompressorPreset, HpfFrequency, LedBehavior, LedBrightness,
            LedLiveTheme, LedPulsingTheme, LedSolidTheme, MicPosition, ReverbType,
        };
        DeviceState {
            gain_tenths: 300,
            mode: InputMode::Manual,
            auto_position: MicPosition::Far,
            auto_tone: AutoTone::Bright,
            auto_gain: AutoGain::Loud,
            muted: true,
            phantom_power: true,
            monitor_mix: 40,
            playback_mix: 60,
            limiter_enabled: true,
            compressor: CompressorPreset::Medium,
            hpf: HpfFrequency::Hz75,
            eq_enabled: true,
            eq_bands: [-40, -20, 20, 40, 60].map(|gain_db| EqBand {
                enabled: true,
                gain_db,
            }),
            denoiser_enabled: true,
            popper_stopper_enabled: false,
            mute_btn_disabled: Some(true),
            tone: 3,
            mv6_gain_locked: true,
            adaptation_rate_fast: true,
            denoiser_level: 80,
            tone_advanced: true,
            reverb_on_output: true,
            reverb_monitoring: true,
            reverb_type: ReverbType::Hall,
            reverb_intensity: 70,
            led_behavior: LedBehavior::Solid,
            led_brightness: LedBrightness::Max,
            led_live_theme: LedLiveTheme::Space,
            led_solid_theme: LedSolidTheme::Custom,
            led_pulsing_theme: LedPulsingTheme::Custom,
            led_solid_rgb: [1, 2, 3],
            led_pulsing_rgb: [4, 5, 6],
            led_live_edge_rgb: [7, 8, 9],
            led_live_middle_rgb: [10, 11, 12],
            led_live_interior_rgb: [13, 14, 15],
            eq_preset: EqPreset::HighPass,
            led_live_meter: false,
            led_night_mode: true,
            ..DeviceState::default()
        }
    }

    /// A preset saved now on each model, through a TOML round trip as `s`
    /// writes and reads it, then the preset files older versions wrote. Each
    /// model's preset comes from its own readback of `distinct_state()`, so a
    /// setting it can't read holds the default, as on real hardware.
    fn preset_sources() -> Vec<(String, PresetSlot)> {
        let mut sources: Vec<(String, PresetSlot)> = ALL_MODELS
            .iter()
            .map(|&model| {
                let fake = FakeDevice::with_state(model, distinct_state());
                let state = fake.open().get_state().expect("readback").state;
                let slot = PresetSlot::from_device_state("Distinct", &state, model);
                let text = toml::to_string_pretty(&slot).expect("save");
                (format!("{model:?}"), toml::from_str(&text).expect("load"))
            })
            .collect();
        for dir in [
            "presets-mvx2u-gen2",
            "presets-mv6-gen2",
            "presets-mv7plus",
            "presets-v2.9",
        ] {
            let path = format!(
                "{}/tests/fixtures/{dir}/preset_1.toml",
                env!("CARGO_MANIFEST_DIR")
            );
            let text = std::fs::read_to_string(&path).expect("fixture");
            sources.push((dir.to_string(), toml::from_str(&text).expect("parse")));
        }
        sources
    }

    /// One write a preset load sent: a binary SET's feature and value, or an
    /// MV7 command line that carries a value.
    struct PresetWrite {
        /// What may be written only once: the feature, or the MV7 command
        /// (with its block number for `setBlock`).
        setting: String,
        text: String,
    }

    /// The one setting a model writes but can't read back: the MV7+ LED
    /// pulsing theme, whose GET address aliases Config Lock.
    const WRITE_ONLY_SETTINGS: [&str; 1] = ["LED_PULSING_THEME"];

    /// Load `source` on a fake `model` the way the Presets tab does, after
    /// reading the fake's state as the TUI would. Returns the writes and any
    /// problem: a refused or ignored packet, or a SET to a setting the model
    /// doesn't read back. The fake answers every model's addresses, so its
    /// own readback is what tells which settings a model has.
    fn load_preset_writes(
        source: &PresetSlot,
        model: DeviceModel,
    ) -> (Vec<PresetWrite>, Vec<String>) {
        let fake = FakeDevice::new(model);
        let device = Some(fake.open());
        let mut state = device
            .as_ref()
            .expect("device")
            .get_state()
            .expect("readback")
            .state;
        let start = fake.log_len();
        let read: std::collections::HashSet<String> = fake
            .log_since(0)
            .into_iter()
            .filter_map(|packet| match packet.kind {
                fake_hid::PacketKind::Get { feature } => Some(feature),
                _ => None,
            })
            .collect();
        let write_monitor_mix = apply_preset_to_state(source, &mut state, model);
        apply_preset_to_device(&device, source, &state, model, write_monitor_mix).expect("load");
        let mut writes = Vec::new();
        let mut problems = Vec::new();
        for packet in fake.log_since(start) {
            match packet.kind {
                fake_hid::PacketKind::Set {
                    feature,
                    value,
                    applied,
                } => {
                    let value = match value {
                        serde_json::Value::String(text) => text,
                        other => other.to_string(),
                    };
                    if !read.contains(&feature) && !WRITE_ONLY_SETTINGS.contains(&feature.as_str())
                    {
                        problems.push(format!("a SET to {feature}, which it doesn't read back"));
                    }
                    if !applied {
                        problems.push(format!("{feature} ignored by the device"));
                    }
                    writes.push(PresetWrite {
                        text: format!("{feature}={value}"),
                        setting: feature,
                    });
                }
                fake_hid::PacketKind::Text { line, .. } if line.contains(' ') => {
                    let mut words = line.split(' ');
                    let command = words.next().unwrap_or_default();
                    if command == "getBlock" {
                        continue;
                    }
                    let setting = match command {
                        "setBlock" => format!("{command} {}", words.next().unwrap_or_default()),
                        _ => command.to_string(),
                    };
                    writes.push(PresetWrite {
                        setting,
                        text: line,
                    });
                }
                fake_hid::PacketKind::Refused { reason } => {
                    problems.push(format!("a refused packet: {reason}"));
                }
                fake_hid::PacketKind::Get { .. }
                | fake_hid::PacketKind::Confirm
                | fake_hid::PacketKind::Text { .. } => {}
            }
        }
        (writes, problems)
    }

    /// Rules every preset load keeps, whatever the preset and model.
    fn preset_load_rule_breaks(
        source: &PresetSlot,
        model: DeviceModel,
        writes: &[PresetWrite],
    ) -> Vec<String> {
        let mut breaks = Vec::new();
        let binary = model != DeviceModel::Mv7;
        if binary && writes.first().is_some_and(|write| write.setting != "MODE") {
            breaks.push("MODE is not written first".to_string());
        }
        let mut counts = std::collections::HashMap::<&str, usize>::new();
        for write in writes {
            *counts.entry(write.setting.as_str()).or_default() += 1;
        }
        for (setting, count) in counts {
            // Gain Lock is unlocked for the gain write and locked again.
            let allowed = if setting == "GAIN_LOCK" { 2 } else { 1 };
            if count > allowed {
                breaks.push(format!("{setting} written {count} times"));
            }
        }
        let is_level = |write: &PresetWrite| {
            matches!(
                write.setting.as_str(),
                "MONITOR_MIX" | "MIC_LEVEL" | "setBlock 22"
            )
        };
        let other_kind = source
            .monitor_mix_is_mic_level
            .is_some_and(|mic_level| mic_level != model.has_playback_mix());
        if let Some(position) = writes.iter().position(is_level) {
            if other_kind {
                breaks.push("Monitor Mix written from a preset of the other kind".to_string());
            }
            if position + 1 != writes.len() {
                breaks.push("Monitor Mix is not written last".to_string());
            }
        }
        breaks
    }

    /// Loads every preset source on every model: the rules above must hold,
    /// and the writes must match `tests/fixtures/preset-loads.golden`. After an
    /// intended change, regenerate with `UPDATE_GOLDEN=1` and read the diff.
    #[test]
    fn preset_loads_match_golden() {
        let mut lines = Vec::new();
        let mut breaks = Vec::new();
        for (name, source) in preset_sources() {
            for model in ALL_MODELS {
                let (writes, problems) = load_preset_writes(&source, model);
                for problem in problems
                    .into_iter()
                    .chain(preset_load_rule_breaks(&source, model, &writes))
                {
                    breaks.push(format!("{name} → {model:?}: {problem}"));
                }
                let texts: Vec<&str> = writes.iter().map(|write| write.text.as_str()).collect();
                let sent = match texts.as_slice() {
                    [] => "(no writes)".to_string(),
                    _ => texts.join(", "),
                };
                lines.push(format!("{name} → {model:?}: {sent}"));
            }
        }
        assert!(breaks.is_empty(), "{}", breaks.join("\n"));
        let actual = lines.join("\n") + "\n";
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(PRESET_LOADS_GOLDEN, &actual).expect("write golden file");
            return;
        }
        // As in `renders_match_golden`: a load that changed shows as a -/+ pair
        // with the same "source → model" key, an added or removed one alone.
        let golden = std::fs::read_to_string(PRESET_LOADS_GOLDEN).unwrap_or_default();
        let golden_lines: std::collections::HashSet<&str> = golden.lines().collect();
        let actual_lines: std::collections::HashSet<&str> = actual.lines().collect();
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
            "preset loads changed; rerun with UPDATE_GOLDEN=1 if intended:\n{}",
            diff.join("\n")
        );
    }
}
