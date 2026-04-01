//! shurectl — Interactive TUI configurator for the Shure MVX2U on Linux
//!
//! Usage:
//!   shure               # Connect to device, launch TUI
//!   shure --demo        # Run without a device (for testing)
//!   shure --list        # List detected devices and exit
//!
//! See README.md for udev setup and permissions.

mod app;
mod device;
mod meter;
mod presets;
mod protocol;
mod ui;

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use app::{App, DeviceAction};
use device::Mvx2u;
use meter::{MeterStatus, start_meter};
use presets::PresetSlot;
use protocol::InputMode;

#[derive(Parser)]
#[command(
    name = "shurectl",
    version,
    about = "shurectl — TUI configurator for the Shure MVX2U audio interface"
)]
struct Cli {
    /// Run in demo mode without a real device
    #[arg(long, short)]
    demo: bool,

    /// List connected MVX2U devices and exit
    #[arg(long, short)]
    list: bool,

    /// Open a specific device by hidraw path (e.g. /dev/hidraw3).
    /// Without this flag, the first detected MVX2U is opened.
    /// Use --list to see available paths.
    #[arg(long, short = 'D')]
    device: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list {
        let devs = device::list_devices();
        if devs.is_empty() {
            println!("No Shure MVX2U devices found.");
            println!("Check that the device is plugged in and the udev rule is installed.");
        } else {
            println!("Found {} MVX2U device(s):", devs.len());
            for d in devs {
                println!("  {} | S/N: {}", d.path, d.serial);
            }
        }
        return Ok(());
    }

    let (device, demo_mode) = if cli.demo {
        (None, true)
    } else {
        let open_result = if let Some(ref path) = cli.device {
            Mvx2u::open_path(path)
        } else {
            Mvx2u::open()
        };
        match open_result {
            Ok(d) => (Some(d), false),
            Err(e) => {
                eprintln!("Warning: {e}");
                eprintln!("Launching in demo mode. Use --demo to suppress this warning.");
                (None, true)
            }
        }
    };

    let mut app = App {
        demo_mode,
        ..App::default()
    };

    if let Some(ref dev) = device {
        match dev.get_state() {
            Ok(mut state) => {
                state.serial_number = dev.serial_number.clone();
                app.device_state = state;
                app.set_ok("Connected — state loaded from device.");
            }
            Err(e) => {
                app.set_err(format!("Connected but failed to read state: {e}"));
            }
        }
    } else {
        app.set_ok("Demo mode — changes will not be sent to a device.");
    }

    app.presets = presets::load_all_presets();

    // Start the input level meter. _meter_stream must stay alive —
    // dropping it stops the capture thread.
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

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_event_loop(&mut terminal, &mut app, &device);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {e}");
    }

    Ok(())
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    device: &Option<Mvx2u>,
) -> Result<()> {
    let tick_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_default();

        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && let Some(action) = handle_key(app, key.code, key.modifiers)
        {
            apply_action(app, device, action);
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
    if matches!(code, KeyCode::Char('q') | KeyCode::Char('Q'))
        || (code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL))
    {
        // Quit is blocked while editing a preset name — Esc must be used first.
        if !app.editing_preset_name {
            app.should_quit = true;
        }
        return None;
    }

    // ── Preset name editing mode ──────────────────────────────────────────────
    if app.editing_preset_name {
        match code {
            KeyCode::Enter | KeyCode::Esc => {
                app.editing_preset_name = false;
                let i = app.editing_preset_index;
                // Persist the updated name if the slot is filled.
                if app.presets[i].is_some() {
                    return Some(DeviceAction::PersistPresetName(i));
                }
            }
            KeyCode::Backspace => {
                let i = app.editing_preset_index;
                if let Some(slot) = &mut app.presets[i] {
                    slot.name.pop();
                }
            }
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
                let i = app.editing_preset_index;
                if let Some(slot) = &mut app.presets[i] {
                    // Cap name length at 40 characters.
                    if slot.name.len() < 40 {
                        slot.name.push(c);
                    }
                }
            }
            _ => {}
        }
        return None;
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
            if mods.contains(KeyModifiers::SHIFT) {
                app.prev_tab();
            } else {
                app.next_tab();
            }
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
            // On PresetName: enter edit mode.
            if let app::Focus::PresetName(i) = app.focus
                && app.presets[i].is_some()
            {
                app.editing_preset_name = true;
                app.editing_preset_index = i;
                return None;
            }
            app.toggle_focused()
        }
        // Preset-specific keys — only active on the Presets tab.
        KeyCode::Char('s') if app.active_tab == app::Tab::Presets => {
            if let app::Focus::PresetName(i) | app::Focus::PresetActions(i) = app.focus {
                Some(DeviceAction::SavePreset(i))
            } else {
                None
            }
        }
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

fn apply_action(app: &mut App, device: &Option<Mvx2u>, action: DeviceAction) {
    let result = match &action {
        DeviceAction::Refresh => {
            if let Some(dev) = device {
                match dev.get_state() {
                    Ok(mut state) => {
                        state.serial_number = dev.serial_number.clone();
                        app.device_state = state;
                        app.set_ok("State refreshed from device.");
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            } else {
                app.set_ok("Demo mode — no device to refresh.");
                Ok(())
            }
        }
        DeviceAction::SetGain(g) => {
            app.set_ok(format!("Gain → {} dB", g));
            send_if_connected(device, |d| d.set_gain(*g))
        }
        DeviceAction::SetMode(mode) => {
            app.set_ok(format!("Mode → {}", mode));
            send_if_connected(device, |d| d.set_mode(*mode == InputMode::Auto))
        }
        DeviceAction::SetAutoPosition(pos) => {
            app.set_ok(format!("Mic Position → {}", pos));
            send_if_connected(device, |d| d.set_auto_position(pos))
        }
        DeviceAction::SetAutoTone(tone) => {
            app.set_ok(format!("Tone → {}", tone));
            send_if_connected(device, |d| d.set_auto_tone(tone))
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
                app.set_ok("Device locked — SET commands will be ignored until unlocked.");
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
        DeviceAction::SetEqBandGain(band, gain_db) => {
            app.set_ok(format!("EQ Band {} → {:+} dB", band + 1, gain_db));
            send_if_connected(device, |d| d.set_eq_band_gain(*band, *gain_db))
        }
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
                slot.apply_to_device_state(&mut app.device_state);
                app.set_ok(format!("Loaded \"{}\".", slot.name));
                // Apply every setting to the device.
                apply_preset_to_device(device, &app.device_state)
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
    };

    if let Err(e) = result {
        app.set_err(format!("Device error: {e}"));
    }
}

fn send_if_connected<F>(device: &Option<Mvx2u>, f: F) -> Result<()>
where
    F: FnOnce(&Mvx2u) -> Result<()>,
{
    match device {
        Some(dev) => f(dev),
        None => Ok(()),
    }
}

/// Send every configurable field of `state` to the device.
/// Called after loading a preset to bring the hardware into sync.
fn apply_preset_to_device(device: &Option<Mvx2u>, state: &protocol::DeviceState) -> Result<()> {
    send_if_connected(device, |d| {
        d.set_mode(state.mode == InputMode::Auto)?;
        d.set_gain(state.gain_db)?;
        d.set_auto_position(&state.auto_position)?;
        d.set_auto_tone(&state.auto_tone)?;
        d.set_auto_gain(&state.auto_gain)?;
        d.set_mute(state.muted)?;
        d.set_phantom(state.phantom_power)?;
        d.set_monitor_mix(state.monitor_mix)?;
        d.set_limiter(state.limiter_enabled)?;
        d.set_compressor(&state.compressor)?;
        d.set_hpf(&state.hpf)?;
        d.set_eq_enable(state.eq_enabled)?;
        for (band, eq) in state.eq_bands.iter().enumerate() {
            d.set_eq_band_enable(band, eq.enabled)?;
            d.set_eq_band_gain(band, eq.gain_db)?;
        }
        Ok(())
    })
}
