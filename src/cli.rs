//! One-shot commands: `--json`, `--mute`, `--preset`, `--mode`, `--gain`,
//! `--monitor-mix` and `--playback-mix`. Each run opens the device, does what
//! it was asked and exits, for scripts, hotkeys and Stream Deck buttons.
//!
//! The command line stays small (see AGENTS.md): the JSON is the preset
//! format plus identity and mute, and only these settings are written. Every
//! write is the `DeviceAction` the TUI would send, through `apply_action()`,
//! and is refused first unless the TUI would send it right now
//! (`App::offers()`), so gain in Auto Level mode or under Gain Lock never
//! reaches the device.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::app::{App, DeviceAction, Focus};
use crate::device::ShureDevice;
use crate::presets::{self, PresetSlot};
use crate::protocol::{DeviceState, InputMode};

/// Mute action for the `--mute` flag.
#[derive(Debug, Clone, Copy, PartialEq, clap::ValueEnum)]
pub enum MuteAction {
    /// Flip the current mute state.
    Toggle,
    /// Mute the microphone.
    On,
    /// Unmute the microphone.
    Off,
    /// Print the current state (`on` or `off`) without changing it.
    Status,
}

/// Input mode for the `--mode` flag.
#[derive(Debug, Clone, Copy, PartialEq, clap::ValueEnum)]
pub enum ModeChoice {
    Auto,
    Manual,
    /// Switch to the other mode.
    Toggle,
}

/// A value for `--gain` and the level flags: a value to set, or a number of
/// presses of → (`+N`) or ← (`-N`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Amount {
    To(f64),
    By(i32),
}

/// clap's parser for an `Amount`: `24`, `24.5`, `+2` or `-1`.
pub fn parse_amount(text: &str) -> Result<Amount> {
    let text = text.trim();
    let wrong = || anyhow!("expected a value, or +N/-N steps, got {text:?}");
    let steps = |digits: &str| digits.parse::<i32>().map_err(|_| wrong());
    if let Some(digits) = text.strip_prefix('+') {
        // `+-3` would otherwise parse as 3 steps down.
        if digits.starts_with(['+', '-']) {
            return Err(wrong());
        }
        return steps(digits).map(Amount::By);
    }
    if text.starts_with('-') {
        return steps(text).map(Amount::By);
    }
    text.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map(Amount::To)
        .ok_or_else(wrong)
}

/// What a one-shot run was asked to do.
#[derive(Debug, Default)]
pub struct Request<'a> {
    /// `--device`: a path from `--list`, or a serial.
    pub device: Option<&'a str>,
    pub json: bool,
    pub mute: Option<MuteAction>,
    /// A 1-based preset slot.
    pub preset: Option<u8>,
    pub mode: Option<ModeChoice>,
    pub gain: Option<Amount>,
    pub monitor_mix: Option<Amount>,
    pub playback_mix: Option<Amount>,
}

impl Request<'_> {
    /// Whether anything here asks for a one-shot run instead of the TUI.
    pub fn is_one_shot(&self) -> bool {
        self.json || self.mute.is_some() || self.preset.is_some() || self.writes_settings()
    }

    /// Whether a full readback is needed: anything beyond mute alone.
    fn combined(&self) -> bool {
        self.json || self.preset.is_some() || self.writes_settings()
    }

    fn writes_settings(&self) -> bool {
        self.mode.is_some()
            || self.gain.is_some()
            || self.monitor_mix.is_some()
            || self.playback_mix.is_some()
    }
}

/// Run a one-shot request. With `--json`, stdout carries only JSON: the
/// state after any writes, or `{"error": …}` and exit status 1. Without it,
/// each write prints the TUI's status line, and errors go to stderr.
pub fn run(request: &Request) -> Result<()> {
    let opened = || -> Result<(Option<(usize, PresetSlot)>, ShureDevice)> {
        let preset = requested_preset(request)?;
        Ok((preset, open(request.device)?))
    };
    if !request.json {
        let (preset, dev) = opened()?;
        return execute(request, preset, dev, &mut |line| println!("{line}")).map(|_| ());
    }
    let printed = opened()
        .and_then(|(preset, dev)| execute(request, preset, dev, &mut |_| {}))
        .and_then(|app| state_json(&app));
    match printed {
        Ok(value) => println!("{value}"),
        Err(e) => {
            println!("{}", json!({ "error": format!("{e:#}") }));
            std::process::exit(1);
        }
    }
    Ok(())
}

/// The slot `--preset` names and its preset, read before the device is opened:
/// an empty slot or a file that won't parse is an error of its own.
fn requested_preset(request: &Request) -> Result<Option<(usize, PresetSlot)>> {
    let Some(number) = request.preset else {
        return Ok(None);
    };
    let index = usize::from(number) - 1;
    let slot =
        presets::load_preset(index)?.with_context(|| format!("Preset slot {number} is empty."))?;
    Ok(Some((index, slot)))
}

/// Refuse a combination that asks two things at once. `main()` reports it as
/// an argument error, as clap does the other conflicts.
pub fn check(request: &Request) -> Result<()> {
    if request.mute == Some(MuteAction::Status) && request.combined() {
        bail!("--mute status reads the mute state alone; it can't be combined with other options");
    }
    Ok(())
}

/// Do what `request` asks, reporting each write's status line. Returns the
/// app, whose state is what the device has after the writes.
fn execute(
    request: &Request,
    preset: Option<(usize, PresetSlot)>,
    dev: ShureDevice,
    report: &mut dyn FnMut(&str),
) -> Result<App> {
    let mute = request.mute;
    let mut app = App {
        device_model: dev.model,
        // Nothing has been read yet: only writes that need no readback can go.
        state_unread: true,
        ..App::default()
    };

    // Mute alone reads at most the mute state, so a cough button stays fast.
    if !request.combined()
        && let Some(action) = mute
    {
        let muted = match action {
            MuteAction::On => true,
            MuteAction::Off => false,
            MuteAction::Toggle => !read_muted(&mut app, &dev)?,
            MuteAction::Status => {
                report(if read_muted(&mut app, &dev)? {
                    "on"
                } else {
                    "off"
                });
                return Ok(app);
            }
        };
        let mut device = Some(dev);
        write(
            &mut app,
            &mut device,
            "--mute",
            DeviceAction::SetMute(muted),
            |state| {
                state.muted = muted;
            },
            report,
        )?;
        return Ok(app);
    }

    check_values(request, &app)?;
    crate::reload_state(&mut app, &dev).context("Could not read device state")?;
    check_writes(request, &app)?;
    let mut device = Some(dev);
    if let Some((index, slot)) = preset {
        app.presets[index] = Some(slot);
        let action = DeviceAction::LoadPreset(index);
        write(&mut app, &mut device, "--preset", action, |_| {}, report)?;
    }
    // The TUI's focus order: mode, mute, gain, then the monitor levels.
    if let Some(choice) = request.mode {
        let switched_from = app.device_state.mode;
        let mode = mode_after(choice, switched_from);
        let action = DeviceAction::SetMode(mode);
        write(
            &mut app,
            &mut device,
            "--mode",
            action,
            |state| state.mode = mode,
            report,
        )?;
        // Whether every model keeps its gain across the switch is unconfirmed,
        // so read the device again before a gain step or the JSON uses it. The
        // MV7 is read twice, as apply_action() re-reads it too; it's one
        // readback, not worth a model check.
        if mode != switched_from
            && let Some(dev) = device.as_ref()
        {
            crate::reload_state(&mut app, dev).context("Could not read device state")?;
        }
    }
    let mute_to = mute.and_then(|action| match action {
        MuteAction::On => Some(true),
        MuteAction::Off => Some(false),
        MuteAction::Toggle => Some(!app.device_state.muted),
        // Refused above when combined; alone it returned already.
        MuteAction::Status => None,
    });
    if let Some(muted) = mute_to {
        let action = DeviceAction::SetMute(muted);
        write(
            &mut app,
            &mut device,
            "--mute",
            action,
            |state| state.muted = muted,
            report,
        )?;
    }
    if let Some(amount) = request.gain {
        let tenths = match amount {
            Amount::To(db) => absolute_gain(&app, db)?,
            Amount::By(presses) => pressed(&app, Focus::Gain, presses).gain_tenths,
        };
        let action = DeviceAction::SetGain(tenths);
        write(
            &mut app,
            &mut device,
            "--gain",
            action,
            |state| state.gain_tenths = tenths,
            report,
        )?;
    }
    if let Some(amount) = request.monitor_mix {
        let mix = match amount {
            Amount::To(value) => absolute_level("--monitor-mix", value)?,
            Amount::By(presses) => pressed(&app, Focus::MonitorMix, presses).monitor_mix,
        };
        let action = DeviceAction::SetMonitorMix(mix);
        write(
            &mut app,
            &mut device,
            "--monitor-mix",
            action,
            |state| state.monitor_mix = mix,
            report,
        )?;
    }
    if let Some(amount) = request.playback_mix {
        let mix = match amount {
            Amount::To(value) => absolute_level("--playback-mix", value)?,
            Amount::By(presses) => pressed(&app, Focus::PlaybackMix, presses).playback_mix,
        };
        let action = DeviceAction::SetMv7PlaybackMix(mix);
        write(
            &mut app,
            &mut device,
            "--playback-mix",
            action,
            |state| state.playback_mix = mix,
            report,
        )?;
    }
    Ok(app)
}

/// Open the device `--device` names, or the only one plugged in.
fn open(selector: Option<&str>) -> Result<ShureDevice> {
    match selector {
        Some(selector) => ShureDevice::open_selected(selector),
        None => ShureDevice::open(),
    }
    .context("Could not open device")
}

/// The mute state, from one read of it alone. When that gets no usable answer
/// (an MV7 slower than the read's 200 ms, unconfirmed), the full readback
/// answers instead, so a toggle never starts from a guess.
fn read_muted(app: &mut App, dev: &ShureDevice) -> Result<bool> {
    match dev.read_mute().context("Could not read the mute state")? {
        Some(muted) => {
            app.device_state.muted = muted;
            Ok(muted)
        }
        None => {
            crate::reload_state(app, dev).context("Could not read device state")?;
            Ok(app.device_state.muted)
        }
    }
}

/// Send `action` the way the TUI's control does, if the TUI offers it right
/// now: `change` is what the control changes on screen first, so a write
/// that clearly didn't land puts the old value back (`apply_action()`).
fn write(
    app: &mut App,
    device: &mut Option<ShureDevice>,
    flag: &str,
    action: DeviceAction,
    change: impl FnOnce(&mut DeviceState),
    report: &mut dyn FnMut(&str),
) -> Result<()> {
    if !app.offers(&action) {
        // check_writes() passed the run on the first readback (mute alone,
        // which skips it, is always offered), so only a re-read after --mode
        // gets here, with any earlier flags written.
        bail!(
            "{} The changes before it, in the order mode, mute, gain, monitor \
             levels, were applied.",
            refusal(app, flag, &action)
        );
    }
    let is_mute = matches!(action, DeviceAction::SetMute(_));
    let before = app.device_state.clone();
    change(&mut app.device_state);
    // The device was just read and already has this value. Don't send it:
    // the MV7 answers [Failed] to a write that changes nothing. With no
    // device, apply_action() still sets the TUI's status line, and sends nothing.
    let unchanged = app.device_state == before
        && !app.state_unread
        && !matches!(action, DeviceAction::LoadPreset(_));
    let mut no_device = None;
    let target = if unchanged { &mut no_device } else { device };
    crate::apply_action(app, target, action, Some(before));
    if app.status_is_error {
        let mut message = without_tui_hints(&app.status_message);
        // The screen would show the mute read back; say it instead.
        if is_mute && app.device_connected && !app.state_unread {
            message.push_str(if app.device_state.muted {
                " Mute is ON."
            } else {
                " Mute is OFF."
            });
        }
        bail!("{message}");
    }
    report(&app.status_message);
    Ok(())
}

/// A failure's status line, without the TUI's advice: a one-shot run has no
/// `r` to press, no screen showing OFF, and doesn't wait for a replug.
fn without_tui_hints(status: &str) -> String {
    status
        .replace(
            crate::STATE_UNREAD_HINT,
            "The device's settings couldn't be read back.",
        )
        .replace(crate::MUTE_UNKNOWN_NOTE, "The mute state is unknown.")
        .replace(crate::RECONNECT_HINT, "the device disconnected.")
        .replace(crate::READ_BACK_NOTE, "The device was read back.")
}

/// Refuse what is wrong whatever the device's settings, before even reading
/// them: an absolute value out of range for the model, or a Playback Level it
/// lacks. Steps can't fail; they stop at the ends.
fn check_values(request: &Request, app: &App) -> Result<()> {
    if let Some(Amount::To(db)) = request.gain {
        absolute_gain(app, db)?;
    }
    for (flag, amount) in [
        ("--monitor-mix", request.monitor_mix),
        ("--playback-mix", request.playback_mix),
    ] {
        if let Some(Amount::To(value)) = amount {
            absolute_level(flag, value)?;
        }
    }
    if request.playback_mix.is_some() && !app.device_model.has_playback_mix() {
        bail!(
            "{}",
            refusal(app, "--playback-mix", &DeviceAction::SetMv7PlaybackMix(0))
        );
    }
    Ok(())
}

/// Refuse the whole run before its first write if the TUI would refuse any
/// part of it: replay the writes in order on a copy of the state just read,
/// asking `offers()` before each, as `write()` will. So `--mode toggle --gain
/// 30` from Manual, or anything with `--gain` under Gain Lock, changes nothing.
/// The exception is the re-read after a `--mode` switch: if it changes what
/// `offers()` allows (the MV6's Playback Level going unread), `write()` refuses
/// a later flag after the earlier ones were written, and says so.
fn check_writes(request: &Request, app: &App) -> Result<()> {
    // A copy for the replay to change.
    let mut planned = app.scratch();
    if let Some(choice) = request.mode {
        let mode = mode_after(choice, planned.device_state.mode);
        let action = DeviceAction::SetMode(mode);
        if !planned.offers(&action) {
            bail!("{}", refusal(&planned, "--mode", &action));
        }
        planned.device_state.mode = mode;
    }
    // The values don't matter to offers(), only which control sends them.
    let writes = [
        (
            request.mute.is_some(),
            "--mute",
            DeviceAction::SetMute(true),
        ),
        (request.gain.is_some(), "--gain", DeviceAction::SetGain(0)),
        (
            request.monitor_mix.is_some(),
            "--monitor-mix",
            DeviceAction::SetMonitorMix(0),
        ),
        (
            request.playback_mix.is_some(),
            "--playback-mix",
            DeviceAction::SetMv7PlaybackMix(0),
        ),
    ];
    for (asked, flag, action) in writes {
        if asked && !planned.offers(&action) {
            bail!("{}", refusal(&planned, flag, &action));
        }
    }
    Ok(())
}

fn mode_after(choice: ModeChoice, current: InputMode) -> InputMode {
    match (choice, current) {
        (ModeChoice::Auto, _) | (ModeChoice::Toggle, InputMode::Manual) => InputMode::Auto,
        (ModeChoice::Manual, _) | (ModeChoice::Toggle, InputMode::Auto) => InputMode::Manual,
    }
}

/// Why the TUI wouldn't send `action` now, for the flag that asked for it.
fn refusal(app: &App, flag: &str, action: &DeviceAction) -> String {
    let model = app.device_model.display_name();
    let gain = matches!(action, DeviceAction::SetGain(_));
    let playback = matches!(action, DeviceAction::SetMv7PlaybackMix(_));
    if gain && app.device_state.mode == InputMode::Auto {
        format!(
            "{flag}: the {model} is in Auto Level mode, which sets its own gain. Use --mode manual first."
        )
    } else if gain && app.gain_locked() {
        format!("{flag}: Gain Lock is on. Turn it off in the TUI first.")
    } else if playback && !app.device_model.has_playback_mix() {
        format!("{flag}: the {model} has no Playback Level.")
    } else if playback && app.playback_unread() {
        format!(
            "{flag}: the {model} didn't answer the Playback Level read, so it isn't \
             changed (that read is unconfirmed on this model)."
        )
    } else {
        format!("{flag}: not available on the {model} with its current settings.")
    }
}

/// A gain in dB, in tenths on the model's step, refused outside its range.
fn absolute_gain(app: &App, db: f64) -> Result<u16> {
    let model = app.device_model;
    let max = model.max_gain_tenths();
    let tenths = (db * 10.0).round();
    if !(0.0..=f64::from(max)).contains(&tenths) {
        bail!(
            "--gain: {db} dB is outside 0 to {} dB on the {}",
            f64::from(max) / 10.0,
            model.display_name()
        );
    }
    Ok(model.snap_gain_tenths(tenths as u16))
}

/// A 0–100% level, refused unless a whole number in range.
fn absolute_level(flag: &str, value: f64) -> Result<u8> {
    if value.fract() != 0.0 || !(0.0..=100.0).contains(&value) {
        bail!("{flag}: expected 0 to 100, got {value}");
    }
    Ok(value as u8)
}

/// The state after `presses` of → on `focus` (← when negative), made with the
/// TUI's own `adjust_focused()`, so a step is the arrow key's by construction.
fn pressed(app: &App, focus: Focus, presses: i32) -> DeviceState {
    // A copy for the presses to change.
    let mut control = App {
        focus,
        ..app.scratch()
    };
    // Every control's range is crossed well within this many presses.
    for _ in 0..presses.unsigned_abs().min(1000) {
        control.adjust_focused(presses.signum());
    }
    control.device_state
}

/// The JSON `--json` prints: identity and mute, and the settings in the
/// preset format (`PresetSlot`), which leaves out what the model lacks.
fn state_json(app: &App) -> Result<Value> {
    let state = &app.device_state;
    let model = app.device_model;
    let slot = PresetSlot::from_device_state("", state, model);
    let mut settings = serde_json::to_value(&slot)?;
    if let Some(fields) = settings.as_object_mut() {
        // A preset's own label, and how a load reads Monitor Mix: not settings.
        fields.remove("name");
        fields.remove("monitor_mix_is_mic_level");
        // Saved on every model, but only some have one, and the MV6 may
        // not answer its read (#99): then it holds a default, not a value.
        if !model.has_playback_mix() || app.playback_unread() {
            fields.remove("playback_mix");
        }
        // The preset file keeps EQ gains in tenths of a dB under `gain_db`;
        // show dB, as the top-level `gain_db` is, so -40 can't read as -40 dB.
        if let (Some(bands), Some(Value::Array(shown))) =
            (slot.eq_bands, fields.get_mut("eq_bands"))
        {
            for (band, shown) in bands.iter().zip(shown) {
                shown["gain_db"] = json!(f64::from(band.gain_db) / 10.0);
            }
        }
    }
    let mut value = json!({
        "model": model.display_name(),
        "muted": state.muted,
        "firmware_version": state.firmware_version,
        "serial": state.display_serial(),
        "settings": settings,
    });
    if model.has_device_rename() {
        value["device_name"] = json!(state.device_name);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_hid::{FakeDevice, Packet, PacketKind};
    use crate::protocol::DeviceModel;

    const MODELS: [DeviceModel; 6] = [
        DeviceModel::Mvx2u,
        DeviceModel::Mvx2uGen2,
        DeviceModel::Mv6,
        DeviceModel::Mv6Gen2,
        DeviceModel::Mv7,
        DeviceModel::Mv7Plus,
    ];

    /// Run `request` on `fake`, returning the app, the lines it reported and
    /// the packets it sent.
    fn run_on(fake: &FakeDevice, request: &Request) -> (Result<App>, Vec<String>, Vec<Packet>) {
        let start = fake.log_len();
        let mut lines = Vec::new();
        let result = check(request).and_then(|()| {
            let preset = requested_preset(request)?;
            execute(request, preset, fake.open(), &mut |line| {
                lines.push(line.to_string())
            })
        });
        (result, lines, fake.log_since(start))
    }

    /// Whether an MV7 line reads: a name alone (`micMute`) or `getBlock N`.
    fn reads_text(line: &str) -> bool {
        !line.contains(' ') || line.starts_with("getBlock ")
    }

    /// The reads among `packets`: binary GETs, and MV7 reading lines.
    fn reads(packets: &[Packet]) -> usize {
        packets
            .iter()
            .filter(|packet| match &packet.kind {
                PacketKind::Get { .. } => true,
                PacketKind::Text { line, .. } => reads_text(line),
                PacketKind::Set { .. } | PacketKind::Confirm | PacketKind::Refused { .. } => false,
            })
            .count()
    }

    /// The writes among `packets`: binary SETs, and every other MV7 line.
    fn writes(packets: &[Packet]) -> usize {
        packets
            .iter()
            .filter(|packet| match &packet.kind {
                PacketKind::Set { .. } => true,
                PacketKind::Text { line, .. } => !reads_text(line),
                PacketKind::Get { .. } | PacketKind::Confirm | PacketKind::Refused { .. } => false,
            })
            .count()
    }

    fn mute(action: MuteAction) -> Request<'static> {
        Request {
            mute: Some(action),
            ..Request::default()
        }
    }

    fn error_of(result: Result<App>) -> String {
        match result {
            Ok(_) => panic!("expected a refusal"),
            Err(e) => format!("{e:#}"),
        }
    }

    #[test]
    fn json_settings_are_the_preset_format_on_every_model() {
        for model in MODELS {
            let fake = FakeDevice::new(model);
            let request = Request {
                json: true,
                ..Request::default()
            };
            let (result, lines, _) = run_on(&fake, &request);
            let app = result.unwrap_or_else(|e| panic!("{model:?}: {e:#}"));
            assert!(lines.is_empty(), "{model:?}: --json reports no lines");
            let json = state_json(&app).expect("serializes");
            let mut preset =
                serde_json::to_value(PresetSlot::from_device_state("", &app.device_state, model))
                    .expect("serializes");
            // Everything but the EQ gains, which the JSON shows in dB (below).
            let mut settings = json["settings"].clone();
            for value in [&mut preset, &mut settings] {
                value.as_object_mut().map(|fields| {
                    for left_out in ["name", "monitor_mix_is_mic_level", "playback_mix"] {
                        fields.remove(left_out);
                    }
                    fields.remove("eq_bands")
                });
            }
            assert_eq!(settings, preset, "{model:?}");
            let shown = &json["settings"];
            assert!(shown.get("monitor_mix_is_mic_level").is_none(), "{model:?}");
            let has_playback = shown.get("playback_mix").is_some();
            assert_eq!(has_playback, model.has_playback_mix(), "{model:?}");
            assert_eq!(json["model"], json!(model.display_name()));
            assert_eq!(json["muted"], json!(false));
            assert_eq!(json["serial"], json!("FAKE0001"));
            assert_eq!(json.get("device_name").is_some(), model.has_device_rename());
            let has_phantom = json["settings"].get("phantom_power").is_some();
            assert_eq!(has_phantom, model.has_phantom_power(), "{model:?}");
        }
    }

    #[test]
    fn json_shows_eq_gains_in_db() {
        let mut state = FakeDevice::seed_state(DeviceModel::Mvx2uGen2);
        state.eq_bands[0].gain_db = -40;
        state.eq_bands[4].gain_db = 15;
        let fake = FakeDevice::with_state(DeviceModel::Mvx2uGen2, state);
        let request = Request {
            json: true,
            ..Request::default()
        };
        let (result, _, _) = run_on(&fake, &request);
        let json = state_json(&result.expect("reads")).expect("serializes");
        let bands = &json["settings"]["eq_bands"];
        assert_eq!(bands[0]["gain_db"], json!(-4.0));
        assert_eq!(bands[4]["gain_db"], json!(1.5));
        assert!(json["settings"].get("eq_bands").is_some());
        let mv7plus = FakeDevice::new(DeviceModel::Mv7Plus);
        let (result, _, _) = run_on(&mv7plus, &request);
        let json = state_json(&result.expect("reads")).expect("serializes");
        assert!(
            json["settings"].get("eq_bands").is_none(),
            "no EQ bands on the MV7+"
        );
    }

    #[test]
    fn mute_on_and_off_send_one_write_and_read_nothing() {
        for model in MODELS {
            for (action, muted, line) in [
                (MuteAction::On, true, "Mute → ON"),
                (MuteAction::Off, false, "Mute → OFF"),
            ] {
                let fake = FakeDevice::new(model);
                let (result, lines, packets) = run_on(&fake, &mute(action));
                let app = result.unwrap_or_else(|e| panic!("{model:?}: {e:#}"));
                assert_eq!(app.device_state.muted, muted, "{model:?}");
                assert_eq!(lines, [line], "{model:?}: the output --mute always printed");
                assert_eq!(reads(&packets), 0, "{model:?}: {packets:?}");
                assert_eq!(writes(&packets), 1, "{model:?}: {packets:?}");
            }
        }
    }

    #[test]
    fn toggle_and_status_read_the_mute_state_alone() {
        for model in MODELS {
            let fake = FakeDevice::new(model);
            fake.set_muted_on_device(true);
            let (result, lines, packets) = run_on(&fake, &mute(MuteAction::Status));
            result.unwrap_or_else(|e| panic!("{model:?}: {e:#}"));
            assert_eq!(lines, ["on"], "{model:?}");
            assert_eq!(reads(&packets), 1, "{model:?}: {packets:?}");
            assert_eq!(writes(&packets), 0, "{model:?}");

            let (result, lines, packets) = run_on(&fake, &mute(MuteAction::Toggle));
            let app = result.unwrap_or_else(|e| panic!("{model:?}: {e:#}"));
            assert!(!app.device_state.muted, "{model:?}");
            assert_eq!(lines, ["Mute → OFF"], "{model:?}");
            assert_eq!(reads(&packets), 1, "{model:?}: {packets:?}");
            assert_eq!(writes(&packets), 1, "{model:?}");
        }
    }

    #[test]
    fn an_unusable_mute_read_falls_back_to_the_full_readback() {
        for model in MODELS {
            let fake = FakeDevice::new(model);
            fake.set_muted_on_device(true);
            fake.faults_mut(|faults| faults.refuse = 1);
            let (result, lines, packets) = run_on(&fake, &mute(MuteAction::Status));
            result.unwrap_or_else(|e| panic!("{model:?}: {e:#}"));
            assert_eq!(lines, ["on"], "{model:?}");
            assert!(reads(&packets) > 1, "{model:?}: the full readback answers");
        }
    }

    #[test]
    fn writes_go_where_the_tui_would_send_them() {
        for model in MODELS {
            let fake = FakeDevice::new(model);
            let request = Request {
                mode: Some(ModeChoice::Manual),
                gain: Some(Amount::To(30.0)),
                monitor_mix: Some(Amount::To(40.0)),
                ..Request::default()
            };
            let (result, lines, _) = run_on(&fake, &request);
            let app = result.unwrap_or_else(|e| panic!("{model:?}: {e:#}"));
            assert_eq!(app.device_state.gain_tenths, model.snap_gain_tenths(300));
            assert_eq!(app.device_state.monitor_mix, 40);
            assert_eq!(lines.len(), 3, "{model:?}: one line per write: {lines:?}");

            let fake = FakeDevice::new(model);
            let request = Request {
                playback_mix: Some(Amount::To(40.0)),
                ..Request::default()
            };
            let (result, _, packets) = run_on(&fake, &request);
            if model.has_playback_mix() {
                result.unwrap_or_else(|e| panic!("{model:?}: {e:#}"));
            } else {
                let err = error_of(result);
                assert!(err.contains("has no Playback Level"), "{model:?}: {err}");
                assert_eq!(writes(&packets), 0, "{model:?}");
            }
        }
    }

    #[test]
    fn gain_is_refused_where_the_tui_hides_it() {
        let gain = Request {
            gain: Some(Amount::By(1)),
            ..Request::default()
        };
        let auto = DeviceState {
            mode: InputMode::Auto,
            ..FakeDevice::seed_state(DeviceModel::Mv7Plus)
        };
        let fake = FakeDevice::with_state(DeviceModel::Mv7Plus, auto);
        let (result, _, packets) = run_on(&fake, &gain);
        assert!(error_of(result).contains("Auto Level mode"));
        assert_eq!(writes(&packets), 0);

        let locked = DeviceState {
            mv6_gain_locked: true,
            ..FakeDevice::seed_state(DeviceModel::Mv6Gen2)
        };
        let fake = FakeDevice::with_state(DeviceModel::Mv6Gen2, locked);
        let (result, _, packets) = run_on(&fake, &gain);
        assert!(error_of(result).contains("Gain Lock is on"));
        assert_eq!(writes(&packets), 0);

        // --mode manual first, as the error says, in one run.
        let auto = DeviceState {
            mode: InputMode::Auto,
            ..FakeDevice::seed_state(DeviceModel::Mv7Plus)
        };
        let fake = FakeDevice::with_state(DeviceModel::Mv7Plus, auto);
        let both = Request {
            mode: Some(ModeChoice::Manual),
            ..gain
        };
        let (result, _, _) = run_on(&fake, &both);
        let app = result.expect("mode goes first");
        assert_eq!(app.device_state.gain_tenths, 250);
    }

    #[test]
    fn steps_follow_the_arrow_keys_and_stop_at_the_ends() {
        let app = |model, gain_tenths| App {
            device_model: model,
            device_state: DeviceState {
                gain_tenths,
                ..DeviceState::default()
            },
            ..App::default()
        };
        let gain_tenths = |app: &App, amount| match amount {
            Amount::To(db) => absolute_gain(app, db),
            Amount::By(presses) => Ok(pressed(app, Focus::Gain, presses).gain_tenths),
        };
        let level = |_: &str, current, amount| match amount {
            Amount::To(value) => absolute_level("--monitor-mix", value),
            Amount::By(presses) => {
                let app = App {
                    device_state: DeviceState {
                        monitor_mix: current,
                        ..DeviceState::default()
                    },
                    ..App::default()
                };
                Ok(pressed(&app, Focus::MonitorMix, presses).monitor_mix)
            }
        };
        assert_eq!(
            gain_tenths(&app(DeviceModel::Mv7, 240), Amount::By(1)).unwrap(),
            255
        );
        assert_eq!(
            gain_tenths(&app(DeviceModel::Mv7Plus, 240), Amount::By(-2)).unwrap(),
            220
        );
        assert_eq!(
            gain_tenths(&app(DeviceModel::Mvx2u, 590), Amount::By(5)).unwrap(),
            600
        );
        assert_eq!(
            gain_tenths(&app(DeviceModel::Mv6, 10), Amount::By(-5)).unwrap(),
            0
        );
        assert!(gain_tenths(&app(DeviceModel::Mv6, 0), Amount::To(37.0)).is_err());
        assert!(gain_tenths(&app(DeviceModel::Mv6, 0), Amount::To(-0.5)).is_err());

        assert_eq!(level("--monitor-mix", 98, Amount::By(5)).unwrap(), 100);
        assert_eq!(level("--monitor-mix", 3, Amount::By(-5)).unwrap(), 0);
        assert!(level("--monitor-mix", 0, Amount::To(40.5)).is_err());
        assert!(level("--monitor-mix", 0, Amount::To(101.0)).is_err());
    }

    #[test]
    fn amounts_parse_as_values_or_steps() {
        let parsed = |text| parse_amount(text).ok();
        assert_eq!(parsed("24"), Some(Amount::To(24.0)));
        assert_eq!(parsed("19.5"), Some(Amount::To(19.5)));
        assert_eq!(parsed("+2"), Some(Amount::By(2)));
        assert_eq!(parsed("-1"), Some(Amount::By(-1)));
        for bad in [
            "", "+", "loud", "+1.5", "+-3", "++3", "NaN", "inf", "--json",
        ] {
            assert!(parse_amount(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn mode_toggle_switches_to_the_other_mode() {
        for model in MODELS {
            let fake = FakeDevice::new(model);
            let toggle = Request {
                mode: Some(ModeChoice::Toggle),
                ..Request::default()
            };
            let (result, _, _) = run_on(&fake, &toggle);
            let app = result.unwrap_or_else(|e| panic!("{model:?}: {e:#}"));
            assert_eq!(
                app.device_state.mode,
                InputMode::Auto,
                "{model:?}: from Manual"
            );
        }
    }

    #[test]
    fn mute_status_asks_one_thing() {
        let request = Request {
            mute: Some(MuteAction::Status),
            json: true,
            ..Request::default()
        };
        let err = check(&request).unwrap_err().to_string();
        assert!(err.contains("can't be combined"), "{err}");
    }

    #[test]
    fn a_refusal_known_from_the_readback_sends_nothing() {
        // Gain Lock is read before any write: the mute doesn't go out either.
        let locked = DeviceState {
            mv6_gain_locked: true,
            ..FakeDevice::seed_state(DeviceModel::Mv6Gen2)
        };
        let fake = FakeDevice::with_state(DeviceModel::Mv6Gen2, locked);
        let request = Request {
            mute: Some(MuteAction::On),
            gain: Some(Amount::To(30.0)),
            ..Request::default()
        };
        let (result, lines, packets) = run_on(&fake, &request);
        assert!(error_of(result).contains("Gain Lock"));
        assert!(lines.is_empty(), "{lines:?}");
        assert_eq!(writes(&packets), 0);

        // From Manual, toggling to Auto would leave no gain to set.
        for mode in [ModeChoice::Toggle, ModeChoice::Auto] {
            let fake = FakeDevice::new(DeviceModel::Mv6);
            let request = Request {
                mode: Some(mode),
                gain: Some(Amount::By(1)),
                ..Request::default()
            };
            let (result, lines, packets) = run_on(&fake, &request);
            assert!(error_of(result).contains("Auto Level mode"), "{mode:?}");
            assert!(lines.is_empty(), "{mode:?}: {lines:?}");
            assert_eq!(writes(&packets), 0, "{mode:?}");
        }
    }

    #[test]
    fn a_value_known_wrong_is_refused_before_anything_is_sent() {
        for (model, request) in [
            (
                DeviceModel::Mv7Plus,
                Request {
                    mode: Some(ModeChoice::Toggle),
                    gain: Some(Amount::To(50.0)),
                    ..Request::default()
                },
            ),
            (
                DeviceModel::Mv6Gen2,
                Request {
                    mode: Some(ModeChoice::Toggle),
                    monitor_mix: Some(Amount::To(40.5)),
                    ..Request::default()
                },
            ),
            (
                DeviceModel::Mvx2u,
                Request {
                    gain: Some(Amount::To(30.0)),
                    playback_mix: Some(Amount::To(40.0)),
                    ..Request::default()
                },
            ),
        ] {
            let fake = FakeDevice::new(model);
            let (result, lines, packets) = run_on(&fake, &request);
            assert!(result.is_err(), "{model:?}");
            assert!(lines.is_empty(), "{model:?}: {lines:?}");
            assert_eq!(packets.len(), 0, "{model:?}: not even a readback");
        }
    }

    #[test]
    fn failures_leave_out_the_tuis_advice() {
        let unread = format!("Device error: timeout. {}", crate::STATE_UNREAD_HINT);
        let mute = format!(
            "Device error: timeout. {} {}",
            crate::MUTE_UNKNOWN_NOTE,
            crate::STATE_UNREAD_HINT
        );
        let gone = format!("Device error: gone — {}", crate::RECONNECT_HINT);
        let read_back = format!("Device error: timeout. {}", crate::READ_BACK_NOTE);
        assert_eq!(
            without_tui_hints(&read_back),
            "Device error: timeout. The device was read back."
        );
        assert_eq!(
            without_tui_hints(&unread),
            "Device error: timeout. The device's settings couldn't be read back."
        );
        assert_eq!(
            without_tui_hints(&mute),
            "Device error: timeout. The mute state is unknown. \
             The device's settings couldn't be read back."
        );
        assert_eq!(
            without_tui_hints(&gone),
            "Device error: gone — the device disconnected."
        );
        for message in [unread, mute, gone] {
            let plain = without_tui_hints(&message);
            assert!(
                !plain.contains("press r") && !plain.contains("shown as OFF"),
                "{plain}"
            );
        }
    }

    #[test]
    fn a_gain_step_after_a_mode_switch_starts_from_a_fresh_read() {
        let auto = DeviceState {
            mode: InputMode::Auto,
            ..FakeDevice::seed_state(DeviceModel::Mv7Plus)
        };
        let fake = FakeDevice::with_state(DeviceModel::Mv7Plus, auto);
        let request = Request {
            mode: Some(ModeChoice::Manual),
            gain: Some(Amount::By(1)),
            ..Request::default()
        };
        let (result, _, packets) = run_on(&fake, &request);
        result.expect("mode, then gain");
        let is_set = |packet: &Packet| matches!(packet.kind, PacketKind::Set { .. });
        let mode_set = packets.iter().position(is_set).expect("the mode SET");
        let gain_set = packets.iter().rposition(is_set).expect("the gain SET");
        assert!(mode_set < gain_set);
        assert!(
            reads(&packets[mode_set..gain_set]) > 1,
            "a readback between the two: {packets:?}"
        );
    }

    #[test]
    fn a_value_the_device_already_has_is_not_sent() {
        // The MV7 answers [Failed] to a write that changes nothing.
        for model in MODELS {
            let fake = FakeDevice::new(model);
            let request = Request {
                mode: Some(ModeChoice::Manual),
                gain: Some(Amount::To(24.0)),
                monitor_mix: Some(Amount::By(-5)),
                ..Request::default()
            };
            let (result, lines, packets) = run_on(&fake, &request);
            result.unwrap_or_else(|e| panic!("{model:?}: {e:#}"));
            assert_eq!(writes(&packets), 0, "{model:?}: {packets:?}");
            assert_eq!(
                lines.len(),
                3,
                "{model:?}: each still says where it is: {lines:?}"
            );
        }
    }

    #[test]
    fn a_failed_mute_that_was_read_back_says_where_it_is() {
        // The MV7 answers [Failed] to the SET and to its check, then the
        // re-read works: the mute didn't land, and the error says so.
        let fake = FakeDevice::new(DeviceModel::Mv7);
        fake.faults_mut(|faults| faults.refuse = 2);
        let (result, lines, _) = run_on(&fake, &mute(MuteAction::On));
        let err = error_of(result);
        assert!(
            err.ends_with("The device was read back. Mute is OFF."),
            "{err}"
        );
        assert!(lines.is_empty());
    }

    #[test]
    fn an_mv6_that_doesnt_answer_its_playback_level_isnt_sent_one() {
        let fake = FakeDevice::new(DeviceModel::Mv6);
        fake.faults_mut(|faults| faults.refused_get = Some("PLAYBACK_LEVEL".into()));
        let json = Request {
            json: true,
            ..Request::default()
        };
        let (result, _, _) = run_on(&fake, &json);
        let shown = state_json(&result.expect("the rest reads")).expect("serializes");
        assert!(shown["settings"].get("playback_mix").is_none(), "{shown}");
        assert!(shown["settings"].get("monitor_mix").is_some());

        for amount in [Amount::To(40.0), Amount::By(5)] {
            let request = Request {
                playback_mix: Some(amount),
                ..Request::default()
            };
            let (result, _, packets) = run_on(&fake, &request);
            let err = error_of(result);
            assert!(
                err.contains("didn't answer the Playback Level read"),
                "{err}"
            );
            assert_eq!(writes(&packets), 0, "{amount:?}");
        }

        // Answered, it works as on the other models.
        fake.faults_mut(|faults| faults.refused_get = None);
        let request = Request {
            playback_mix: Some(Amount::To(40.0)),
            ..Request::default()
        };
        let (result, _, _) = run_on(&fake, &request);
        assert_eq!(result.expect("written").device_state.playback_mix, 40);
    }

    #[test]
    fn a_playback_level_unread_after_a_re_read_is_refused_with_its_reason() {
        // As when the re-read after --mode goes without it: check_writes()
        // passed, so write() is the one to refuse.
        let mut app = App {
            device_model: DeviceModel::Mv6,
            unread_gets: vec![crate::protocol::cmd_get_mv7_playback_mix(0)],
            ..App::default()
        };
        let action = DeviceAction::SetMv7PlaybackMix(40);
        let err = write(
            &mut app,
            &mut None,
            "--playback-mix",
            action,
            |_| {},
            &mut |_| {},
        )
        .expect_err("refused")
        .to_string();
        assert!(
            err.contains("didn't answer the Playback Level read"),
            "{err}"
        );
        assert!(err.ends_with("were applied."), "{err}");
    }
}
