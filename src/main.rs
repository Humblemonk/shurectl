//! shurectl — Interactive TUI configurator for Shure USB microphones
//!
//! Supports:
//!   - Shure MVX2U Gen 1 (XLR-to-USB audio interface)
//!   - Shure MVX2U Gen 2 (XLR-to-USB interface with updated DSP)
//!   - Shure MV6          (USB gaming microphone)
//!
//! Usage:
//!   shurectl                   # Connect to device, launch TUI
//!   shurectl --demo            # Demo MVX2U (default model) without a device
//!   shurectl --demo mv6        # Demo MV6 without a device
//!   shurectl --demo mvx2u-gen2 # Demo MVX2U Gen 2 without a device
//!   shurectl --list            # List detected devices and exit
//!   shurectl --device PATH     # Open a specific device by HID path

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
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use app::{App, DeviceAction};
use device::ShureDevice;
use meter::{MeterStatus, start_meter};
use presets::PresetSlot;
use protocol::{DeviceModel, InputMode};

#[derive(Parser)]
#[command(
    name = "shurectl",
    version,
    about = "shurectl — TUI configurator for Shure MVX2U, MVX2U Gen 2, and MV6 USB microphones"
)]
struct Cli {
    /// Run in demo mode without a real device.
    /// Optionally specify which device model to simulate: mvx2u (default), mvx2u-gen2, mv6.
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list {
        let devs = device::list_devices();
        if devs.is_empty() {
            println!("No Shure MVX2U, MVX2U Gen 2, or MV6 devices found.");
            #[cfg(target_os = "linux")]
            println!("Check that the device is plugged in and the udev rule is installed.");
            #[cfg(not(target_os = "linux"))]
            println!("Check that the device is plugged in and accessible.");
        } else {
            println!("Found {} Shure device(s):", devs.len());
            for d in devs {
                println!(
                    "  {} | {} | S/N: {}",
                    d.path,
                    d.model.display_name(),
                    d.serial
                );
            }
        }
        return Ok(());
    }

    let (device, demo_mode, demo_model) = if let Some(ref model_str) = cli.demo {
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
        match dev.get_state() {
            Ok(mut state) => {
                state.serial_number = dev.serial_number.clone();
                app.device_state = state;
                app.set_ok(format!(
                    "Connected to {} — state loaded.",
                    dev.model.display_name()
                ));
            }
            Err(e) => {
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

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_event_loop(&mut terminal, &mut app, &device);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {e}");
    }

    Ok(())
}

/// Parse a `--demo` model string into a `DeviceModel`.
/// Accepts case-insensitive variants of the supported model names.
fn parse_demo_model(s: &str) -> Result<DeviceModel> {
    match s.to_ascii_lowercase().replace('_', "-").as_str() {
        "mvx2u" => Ok(DeviceModel::Mvx2u),
        "mvx2u-gen2" | "mvx2ugen2" => Ok(DeviceModel::Mvx2uGen2),
        "mv6" => Ok(DeviceModel::Mv6),
        other => {
            anyhow::bail!("unknown demo model \"{other}\". Valid options: mvx2u, mvx2u-gen2, mv6")
        }
    }
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    device: &Option<ShureDevice>,
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
                if let Some(slot) = &mut app.presets[i]
                    && slot.name.len() < 40
                {
                    slot.name.push(c);
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
            if let app::Focus::PresetName(i) = app.focus
                && app.presets[i].is_some()
            {
                app.editing_preset_name = true;
                app.editing_preset_index = i;
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
        KeyCode::Char('f') if app.active_tab == app::Tab::Eq => Some(DeviceAction::FlattenEq),
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

fn apply_action(app: &mut App, device: &Option<ShureDevice>, action: DeviceAction) {
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
                slot.apply_to_device_state(&mut app.device_state);
                app.set_ok(format!("Loaded \"{}\".", slot.name));
                apply_preset_to_device(device, &app.device_state, app.device_model)
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

fn send_if_connected<F>(device: &Option<ShureDevice>, f: F) -> Result<()>
where
    F: FnOnce(&ShureDevice) -> Result<()>,
{
    match device {
        Some(dev) => f(dev),
        None => Ok(()),
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
        d.set_mode(state.mode == InputMode::Auto)?;
        d.set_gain(state.gain_db)?;
        d.set_mute(state.muted)?;
        d.set_hpf(&state.hpf)?;
        match model {
            DeviceModel::Mvx2u => {
                d.set_auto_position(&state.auto_position)?;
                d.set_auto_tone(&state.auto_tone)?;
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
        }
        Ok(())
    })
}
