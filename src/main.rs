//! shurectl — Interactive TUI configurator for Shure USB audio interfaces and microphones
//!
//! Supports:
//!   - Shure MVX2U Gen 1 (XLR-to-USB audio interface)
//!   - Shure MVX2U Gen 2 (XLR-to-USB interface with updated DSP)
//!   - Shure MV6          (USB gaming microphone)
//!   - Shure MV7          (USB/XLR dynamic microphone, original)
//!   - Shure MV7+         (USB/XLR dynamic microphone)
//!
//! Usage:
//!   shurectl                   # Connect to device, launch TUI
//!   shurectl --demo            # Demo MVX2U (default model) without a device
//!   shurectl --demo mv6        # Demo MV6 without a device
//!   shurectl --demo mvx2u-gen2 # Demo MVX2U Gen 2 without a device
//!   shurectl --demo mv7        # Demo MV7 without a device
//!   shurectl --list            # List detected devices and exit
//!   shurectl --device PATH     # Open a specific device by HID path
//!   shurectl --mute            # Toggle mute (no TUI)
//!   shurectl --mute on         # Mute (no TUI)
//!   shurectl --mute off        # Unmute (no TUI)

mod app;
mod device;
mod meter;
mod mouse;
mod presets;
mod protocol;
mod ui;

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    cursor::Show,
    event::{
        self, DisableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use app::{App, DeviceAction};
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
    /// Optionally specify which device model to simulate: mvx2u (default), mvx2u-gen2, mv6, mv7, mv7plus.
    #[arg(long, short, num_args = 0..=1, default_missing_value = "mvx2u", value_name = "MODEL")]
    demo: Option<String>,

    /// List connected Shure devices and exit
    #[arg(long, short)]
    list: bool,

    /// Open a specific device by its HID path.
    /// Without this flag, the first detected device is opened (error if multiple found).
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

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

    if let Some(ref dev) = device {
        match reload_state(&mut app, dev) {
            Ok(unrecognised) => {
                app.set_ok(format!(
                    "Connected to {} — state loaded.{}",
                    dev.model.display_name(),
                    unrecognised_note(&unrecognised)
                ));
            }
            Err(e) => {
                app.device_connected = !is_disconnect(&e);
                app.set_err(format!("Connected but failed to read state: {e}"));
            }
        }
    } else {
        app.set_ok(format!(
            "Demo mode ({}) — changes will not be sent to a device.",
            device_model.display_name()
        ));
    }

    app.presets = presets::load_all_presets();

    let _meter_stream = if !demo_mode {
        match start_meter(Arc::clone(&app.meter_level), Arc::clone(&app.peak_window)) {
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

    let result = run_event_loop(&mut terminal, &mut app, &mut device);

    drop(terminal);
    drop(cleanup);

    if let Err(e) = result {
        eprintln!("Error: {e}");
    }

    Ok(())
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
    apply_preset_to_state(&slot, &mut state, model);
    apply_preset_to_device(&Some(dev), &state, model).context("Could not load preset")?;
    println!("Loaded \"{}\".", slot.name);
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
        "mv7" => Ok(DeviceModel::Mv7),
        "mv7plus" | "mv7+" => Ok(DeviceModel::Mv7Plus),
        other => {
            anyhow::bail!(
                "unknown demo model \"{other}\". Valid options: mvx2u, mvx2u-gen2, mv6, mv7, mv7plus"
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
) -> Result<()> {
    let tick_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();
    let mut last_presence_poll = Instant::now();
    let mut seen_back = false;
    // The newest wheel adjustment, its control, and when the burst began. Wheel
    // adjustments carry absolute values, so the newest one supersedes the rest.
    let mut pending_scroll: Option<(app::Focus, DeviceAction, Instant)> = None;

    loop {
        let mut hits = mouse::HitMap::default();
        terminal.draw(|f| hits = ui::draw(f, app))?;

        let timeout = if pending_scroll.is_some() {
            Duration::ZERO
        } else {
            tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_default()
        };

        let queued = event::poll(timeout)?;
        if queued {
            let event = event::read()?;
            let is_scroll = matches!(
                event,
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollUp | MouseEventKind::ScrollDown,
                    ..
                })
            );
            let action = match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    app.armed_preset_action = None;
                    handle_key(app, key.code, key.modifiers)
                }
                Event::Mouse(event) => mouse::handle_mouse(app, event, &hits)
                    .and_then(|key| handle_key(app, key, KeyModifiers::NONE)),
                Event::Key(_)
                | Event::Resize(_, _)
                | Event::FocusGained
                | Event::FocusLost
                | Event::Paste(_) => None,
            };
            match action {
                Some(action) if is_scroll => {
                    let started = match pending_scroll.take() {
                        Some((focus, _, started)) if focus == app.focus => started,
                        Some((_, earlier, _)) => {
                            apply_action(app, device, earlier);
                            Instant::now()
                        }
                        None => Instant::now(),
                    };
                    pending_scroll = Some((app.focus, action, started));
                }
                Some(action) => {
                    // Keep device writes in input order.
                    if let Some((_, earlier, _)) = pending_scroll.take() {
                        apply_action(app, device, earlier);
                    }
                    apply_action(app, device, action);
                }
                None => {}
            }
        }

        if let Some((_, action, _)) = pending_scroll.take_if(|(_, _, started)| {
            !queued || app.should_quit || started.elapsed() >= SCROLL_FLUSH_INTERVAL
        }) {
            apply_action(app, device, action);
        }

        if last_presence_poll.elapsed() >= PRESENCE_POLL_INTERVAL {
            last_presence_poll = Instant::now();
            poll_presence(app, device, &mut seen_back);
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) -> Option<DeviceAction> {
    // ── Preset or device name editing mode ────────────────────────────────────
    // Handled before the quit keys so `q` can be typed in a name. Ctrl+C is
    // ignored while editing, as before.
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

    if matches!(code, KeyCode::Char('q') | KeyCode::Char('Q'))
        || (code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL))
    {
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

fn apply_action(app: &mut App, device: &mut Option<ShureDevice>, action: DeviceAction) {
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
            app.set_ok(format!("Auto Gain → {}", gain));
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
        DeviceAction::SetMonitorMix(m) => {
            app.set_ok(format!("Monitor mix → {}%", m));
            send_if_connected(device, |d| d.set_monitor_mix(*m))
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
        // ── MV6 actions ───────────────────────────────────────────────────────
        DeviceAction::SetMv6Denoiser(en) => {
            app.set_ok(format!("Denoiser → {}", if *en { "ON" } else { "OFF" }));
            send_if_connected(device, |d| d.set_mv6_denoiser(*en))
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
                "Mute Button → {}",
                if *disabled { "Disabled" } else { "Enabled" }
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
        DeviceAction::SetMv6MonitorMix(m) => {
            app.set_ok(format!("Monitor mix → {}%", m));
            send_if_connected(device, |d| d.set_mv6_monitor_mix(*m))
        }
        // ── MV7+ exclusive actions ────────────────────────────────────────────
        DeviceAction::SetMv7PlaybackMix(m) => {
            app.set_ok(format!("Playback mix → {}%", m));
            send_if_connected(device, |d| d.set_mv7_playback_mix(*m))
        }
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
            let slot = PresetSlot::from_device_state(name, &app.device_state);
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
                apply_preset_to_state(slot, &mut app.device_state, app.device_model);
                app.set_ok(format!("Loaded \"{}\".", slot.name));
                apply_preset_to_device(device, &app.device_state, app.device_model)
                    .and_then(|()| resync_mv7_state(app, device))
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
        DeviceAction::FactoryReset => {
            app.set_ok("Factory reset sent — device is restarting. Restart shurectl to reconnect.");
            send_if_connected(device, |d| d.factory_reset())
        }
    };

    if let Err(e) = result {
        if is_disconnect(&e) {
            app.device_connected = false;
            app.set_err(format!("Device error: {e} — {RECONNECT_HINT}"));
        } else {
            app.set_err(format!("Device error: {e}"));
        }
    }

    // A readback or preset may have changed the mode under the current tab.
    app.settle_focus(mode_before);
}

/// Whether `e` means the device is gone (a HID read or write failed), as
/// opposed to a device that is present but refused a command.
fn is_disconnect(e: &anyhow::Error) -> bool {
    e.downcast_ref::<device::Disconnected>().is_some()
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
        Ok(unrecognised) => {
            app.device_connected = true;
            app.set_ok(format!(
                "State refreshed from device.{}",
                unrecognised_note(&unrecognised)
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
        Ok(unrecognised) => {
            app.device_connected = true;
            app.set_ok(format!(
                "Reconnected to {name} — state loaded.{}",
                unrecognised_note(&unrecognised)
            ));
            Ok(())
        }
        Err(e) => {
            app.device_connected = !is_disconnect(&e);
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
        PollStep::MarkDisconnected => {
            app.device_connected = false;
            app.set_err(format!(
                "{} disconnected — {RECONNECT_HINT}",
                dev.model.display_name()
            ));
        }
        PollStep::Reconnect => apply_action(app, device, DeviceAction::Reconnect),
    }
    *seen_back = present && !app.device_connected;
}

/// Status-bar suffix for replies a readback could not match to any setting,
/// so they can be seen and reported without writing to the terminal.
fn unrecognised_note(unrecognised: &[String]) -> String {
    match unrecognised {
        [] => String::new(),
        [only] => format!(" Ignored an unrecognised reply: {only}."),
        [first, ..] => format!(
            " Ignored {} unrecognised replies, first: {first}.",
            unrecognised.len()
        ),
    }
}

/// Replace the app's state with a full readback from the device. Returns the
/// replies it could not match, for the caller's status message.
fn reload_state(app: &mut App, dev: &ShureDevice) -> Result<Vec<String>> {
    let readback = dev.get_state()?;
    app.device_state = readback.state;
    app.device_state.serial_number = dev.serial_number.clone();
    Ok(readback.unrecognised)
}

/// The MV7 stores mode, mic position and tone as one setting, and changing it
/// changes other state too: entering Auto Level resets EQ and compressor, and
/// leaving it reports a new gain. Re-read the whole state afterwards so the UI
/// shows what the mic actually has. No-op for other models and in demo mode.
fn resync_mv7_state(app: &mut App, device: &Option<ShureDevice>) -> Result<()> {
    match device {
        Some(dev) if dev.model == DeviceModel::Mv7 => {
            let unrecognised = reload_state(app, dev)?;
            // Keep the caller's message ("Mode → …") and add the note to it.
            app.status_message
                .push_str(&unrecognised_note(&unrecognised));
            Ok(())
        }
        Some(_) | None => Ok(()),
    }
}

fn send_if_connected<F>(device: &Option<ShureDevice>, f: F) -> Result<()>
where
    F: FnOnce(&ShureDevice) -> Result<()>,
{
    match device {
        Some(dev) => f(dev),
        None => Ok(()),
    }
}

/// Apply `slot` to `state`, fitted to `model`.
fn apply_preset_to_state(slot: &PresetSlot, state: &mut protocol::DeviceState, model: DeviceModel) {
    slot.apply_to_device_state(state);
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
}

/// Send every configurable field of `state` to the device.
/// Called after loading a preset to bring the hardware into sync.
fn apply_preset_to_device(
    device: &Option<ShureDevice>,
    state: &protocol::DeviceState,
    model: DeviceModel,
) -> Result<()> {
    send_if_connected(device, |d| {
        d.set_mode(
            state.mode == InputMode::Auto,
            state.auto_position,
            state.auto_tone,
        )?;
        // The MV7 manages gain itself in Auto Level, so its gain is sent below,
        // in Manual only. Every other model keeps the original order:
        // mode → gain → mute → HPF.
        if model != DeviceModel::Mv7 {
            d.set_gain(state.gain_tenths)?;
        }
        d.set_mute(state.muted)?;
        // The MV7 has no HPF.
        if model != DeviceModel::Mv7 {
            d.set_hpf(&state.hpf)?;
        }
        match model {
            DeviceModel::Mvx2u => {
                d.set_auto_position(state.auto_position, state.auto_tone)?;
                d.set_auto_tone(state.auto_position, state.auto_tone)?;
                d.set_auto_gain(&state.auto_gain)?;
                d.set_phantom(state.phantom_power)?;
                d.set_monitor_mix(state.monitor_mix)?;
                d.set_limiter(state.limiter_enabled)?;
                d.set_compressor(&state.compressor)?;
                d.set_eq_enable(state.eq_enabled)?;
                for (band, eq) in state.eq_bands.iter().enumerate() {
                    d.set_eq_band_enable(band, eq.enabled)?;
                    d.set_eq_band_gain(band, eq.gain_db)?;
                }
            }
            DeviceModel::Mvx2uGen2 => {
                d.set_phantom(state.phantom_power)?;
                d.set_mv6_monitor_mix(state.monitor_mix)?;
                d.set_limiter(state.limiter_enabled)?;
                d.set_compressor(&state.compressor)?;
                d.set_mv6_denoiser(state.denoiser_enabled)?;
                d.set_mv6_popper_stopper(state.popper_stopper_enabled)?;
                d.set_mv6_tone(state.tone)?;
                d.set_mv6_gain_lock(state.mv6_gain_locked)?;
                for (band, eq) in state.eq_bands.iter().enumerate() {
                    d.set_eq_band_gain(band, eq.gain_db)?;
                }
            }
            DeviceModel::Mv6 => {
                d.set_mv6_denoiser(state.denoiser_enabled)?;
                d.set_mv6_popper_stopper(state.popper_stopper_enabled)?;
                d.set_mv6_mute_btn_disable(state.mute_btn_disabled)?;
                d.set_mv6_tone(state.tone)?;
                d.set_mv6_gain_lock(state.mv6_gain_locked)?;
                d.set_mv6_monitor_mix(state.monitor_mix)?;
            }
            DeviceModel::Mv7 => {
                if state.mode == InputMode::Manual {
                    d.set_gain(state.gain_tenths)?;
                    d.set_compressor(&state.compressor)?;
                    d.set_eq_preset(state.eq_preset)?;
                }
                d.set_mv6_monitor_mix(state.monitor_mix)?;
                d.set_led_live_meter(state.led_live_meter)?;
                d.set_led_night_mode(state.led_night_mode)?;
            }
            DeviceModel::Mv7Plus => {
                d.set_mv6_denoiser(state.denoiser_enabled)?;
                d.set_mv6_popper_stopper(state.popper_stopper_enabled)?;
                d.set_mv6_mute_btn_disable(state.mute_btn_disabled)?;
                d.set_mv6_tone(state.tone)?;
                d.set_mv6_monitor_mix(state.monitor_mix)?;
                d.set_limiter(state.limiter_enabled)?;
                d.set_compressor(&state.compressor)?;
                d.set_mv7_playback_mix(state.playback_mix)?;
                d.set_mv7_reverb_output(state.reverb_on_output)?;
                d.set_mv7_reverb_monitor(state.reverb_monitoring)?;
                d.set_mv7_reverb_type(&state.reverb_type)?;
                d.set_mv7_reverb_intensity(state.reverb_intensity)?;
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
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn device_name_edit_drops_unsupported_chars_and_renames_on_enter() {
        let mut app = device_name_focused(DeviceModel::Mvx2uGen2, "Desk");
        assert!(handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).is_none());
        assert!(app.editing_device_name);
        assert_eq!(app.name_draft, "Desk");
        for c in " é2q".chars() {
            handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(!app.should_quit);
        assert_eq!(app.name_draft, "Desk 2q");
        assert_eq!(
            app.device_state.device_name, "Desk",
            "unchanged until Enter"
        );
        let action = handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(&action, Some(DeviceAction::RenameDevice(n)) if n == "Desk 2q"));
        assert!(!app.editing_device_name);
        apply_action(&mut app, &mut None, action.expect("rename action"));
        assert_eq!(app.device_state.device_name, "Desk 2q");
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
    fn tab_key_moves_to_next_tab() {
        let mut app = App::default();
        // Manual mode, so the MVX2U Gen 1's EQ tab isn't locked and skipped.
        app.device_state.mode = InputMode::Manual;
        app.active_tab = app::Tab::Main;
        handle_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.active_tab, app::Tab::Eq);
    }

    #[test]
    fn shift_tab_moves_to_previous_tab() {
        // Terminals report Shift+Tab as BackTab (usually with SHIFT set), never
        // as Tab with SHIFT, so this is the key the handler has to react to.
        let mut app = App::default();
        app.active_tab = app::Tab::Main;
        handle_key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(app.active_tab, app::Tab::Info);
    }

    fn editing_preset_name(name: &str) -> App {
        let mut presets: [Option<PresetSlot>; presets::PRESET_COUNT] = Default::default();
        presets[0] = Some(PresetSlot::from_device_state(
            name,
            &protocol::DeviceState::default(),
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
    fn q_can_be_typed_in_a_preset_name() {
        let mut app = editing_preset_name("");
        for c in "Quiet q".chars() {
            handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(!app.should_quit);
        assert_eq!(app.name_draft, "Quiet q");
        assert!(matches!(
            handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            Some(DeviceAction::PersistPresetName(0))
        ));
        assert_eq!(
            app.presets[0].as_ref().map(|s| s.name.as_str()),
            Some("Quiet q")
        );
    }

    #[test]
    fn ctrl_c_is_ignored_while_editing_a_preset_name() {
        let mut app = editing_preset_name("Voice");
        handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(!app.should_quit);
        assert!(app.editing_preset_name);
        assert_eq!(app.name_draft, "Voice");
        assert_eq!(
            app.presets[0].as_ref().map(|s| s.name.as_str()),
            Some("Voice")
        );
    }

    #[test]
    fn q_quits_outside_name_editing() {
        let mut app = App::default();
        handle_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(app.should_quit);
    }

    fn gen2_on_eq_tab(mode: InputMode) -> App {
        App {
            device_model: DeviceModel::Mvx2uGen2,
            active_tab: app::Tab::Eq,
            device_state: protocol::DeviceState {
                mode,
                ..protocol::DeviceState::default()
            },
            ..App::default()
        }
    }

    /// Gen 2 in Auto shows only the tone slider, so `f` must not zero the
    /// hidden Manual-mode bands.
    #[test]
    fn flatten_eq_ignored_on_gen2_in_auto_mode() {
        let mut app = gen2_on_eq_tab(InputMode::Auto);
        let action = handle_key(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(action.is_none(), "got {action:?}");
    }

    #[test]
    fn flatten_eq_works_on_gen2_in_manual_mode() {
        let mut app = gen2_on_eq_tab(InputMode::Manual);
        let action = handle_key(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(matches!(action, Some(DeviceAction::FlattenEq)));
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
        presets[0] = Some(PresetSlot::from_device_state("MV6", &mv6_state));
        let mut app = App {
            device_model: DeviceModel::Mv7Plus,
            presets,
            ..App::default()
        };
        apply_action(&mut app, &mut None, DeviceAction::LoadPreset(0));
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
        apply_action(&mut app, &mut device, DeviceAction::Refresh);
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

    /// Models with Gain Lock keep the preset's lock, since they can show and undo it.
    #[test]
    fn gain_lock_preset_keeps_lock_on_models_with_gain_lock() {
        let locked_state = protocol::DeviceState {
            mv6_gain_locked: true,
            ..protocol::DeviceState::default()
        };
        let mut presets: [Option<PresetSlot>; presets::PRESET_COUNT] = Default::default();
        presets[0] = Some(PresetSlot::from_device_state("MV6", &locked_state));
        let mut app = App {
            device_model: DeviceModel::Mvx2uGen2,
            presets,
            ..App::default()
        };
        apply_action(&mut app, &mut None, DeviceAction::LoadPreset(0));
        assert!(app.device_state.mv6_gain_locked);
        assert!(app.gain_locked());
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
    fn unrecognised_note_is_empty_or_names_the_first_reply() {
        assert_eq!(unrecognised_note(&[]), "");
        assert_eq!(
            unrecognised_note(&["feature 0x01 0x99".into()]),
            " Ignored an unrecognised reply: feature 0x01 0x99."
        );
        assert_eq!(
            unrecognised_note(&["feature 0x01 0x99".into(), "feature 0x02 0x77".into()]),
            " Ignored 2 unrecognised replies, first: feature 0x01 0x99."
        );
    }

    fn app_with_preset(model: DeviceModel, saved: protocol::DeviceState) -> App {
        let mut presets: [Option<PresetSlot>; presets::PRESET_COUNT] = Default::default();
        presets[0] = Some(PresetSlot::from_device_state("saved", &saved));
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
        apply_action(&mut app, &mut None, DeviceAction::LoadPreset(0));
        assert_eq!(app.device_state.eq_bands[0].gain_db, 20);
        assert_eq!(app.device_state.eq_bands[1].gain_db, -20);

        let mut app = app_with_preset(DeviceModel::Mvx2uGen2, gen2_eq_state());
        apply_action(&mut app, &mut None, DeviceAction::LoadPreset(0));
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
        apply_action(&mut app, &mut None, DeviceAction::LoadPreset(0));
        assert_eq!(app.device_state.mode, InputMode::Manual);
        assert_eq!(app.active_tab, app::Tab::Presets);
        assert_eq!(app.focus, app::Focus::PresetActions(0));
    }
}
