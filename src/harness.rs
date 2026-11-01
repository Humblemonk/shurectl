//! Headless driver for the e2e scenarios (`tests/e2e.rs`) and agent QA passes
//! (`shurectl --qa`). Built only for tests and `--features qa`, so it never ships.
//!
//! The harness runs the real `App`, the real `step()` (so `handle_key()`,
//! `mouse::handle_mouse()` and `apply_action()`) and a real `ShureDevice` on a
//! `fake_hid::FakeDevice`, rendering through ratatui's `TestBackend`. Nothing
//! here writes to a device except through `apply_action()`, and nothing opens
//! real hardware.
//!
//! Its input is one JSON object per line: an input (`key`, `type`, `click`,
//! `click_on`, `wheel`, `resize`, `tick`, `device`, `meter`, `preset_dir`), an
//! `expect` line, a `query`, `record` or `reset`. Every line is answered with one
//! JSON line: the screen, the `App` fields, the packets since the previous line
//! and any failed expectations. `.claude/skills/qa/SKILL.md` documents the lines.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use serde_json::{Map, Value, json};

use crate::app::App;
use crate::device::ShureDevice;
use crate::fake_hid::{FakeDevice, LiveField, Packet, PacketKind, hex};
use crate::mouse::HitMap;
use crate::protocol::{DeviceModel, DeviceState, InputMode};
use crate::{LoopState, load_initial_state, presets, step, ui};

/// Every model, by the name `--demo` and scenario headers use.
pub const MODEL_NAMES: [(&str, DeviceModel); 6] = [
    ("mvx2u", DeviceModel::Mvx2u),
    ("mvx2u-gen2", DeviceModel::Mvx2uGen2),
    ("mv6", DeviceModel::Mv6),
    ("mv6-gen2", DeviceModel::Mv6Gen2),
    ("mv7", DeviceModel::Mv7),
    ("mv7plus", DeviceModel::Mv7Plus),
];

/// The size `renders_match_golden` checks, and every session's starting size.
const DEFAULT_SIZE: (u16, u16) = (120, 40);
const MAX_SIZE: (u16, u16) = (300, 120);
/// Longest fake time one `tick` line may advance.
const MAX_TICK: Duration = Duration::from_secs(60);
/// The real event loop's tick; `tick` runs one `step()` per frame of it.
const FRAME: Duration = Duration::from_millis(100);
/// Most presses one `key` line may repeat.
const MAX_REPEAT: u64 = 100;
/// Packets listed in one reply; the rest are summarised by count.
const MAX_PACKETS_PER_REPLY: usize = 200;

/// Expectation keys that read the fake's packet log. In demo mode there is no
/// device, so they are skipped rather than failed.
const PACKET_KEYS: [&str; 6] = [
    "set",
    "sets",
    "get",
    "packets_raw",
    "text_sent",
    "no_packets",
];

/// A directory under the system temp dir, removed on drop. The preset files of
/// one session live here, never in the user's config directory.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "shurectl-qa-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path)
            .with_context(|| format!("Failed to create {}", path.display()))?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One line's outcome, sent back as JSON.
#[derive(Default)]
struct Outcome {
    failures: Vec<String>,
    errors: Vec<String>,
    skipped: Vec<String>,
    result: Option<Value>,
}

pub struct Harness {
    model: DeviceModel,
    fake: Option<FakeDevice>,
    app: App,
    device: Option<ShureDevice>,
    terminal: Terminal<TestBackend>,
    loop_state: LoopState,
    /// The fake clock `step()` runs on.
    now: Instant,
    hits: HitMap,
    /// `ui::fits()` for the last frame drawn, which `step()` needs as the real
    /// loop passes it.
    fits: bool,
    previous_screen: Vec<String>,
    /// Packet log length at the previous reply and the previous `expect` line.
    reply_cursor: usize,
    expect_cursor: usize,
    recording: Option<Vec<String>>,
    preset_dir: TempDir,
}

impl Harness {
    /// A session on `model`, with a fake device or (`fake` false) in demo mode.
    /// The fake device starts from `FakeDevice::seed_state()` (Manual, 24 dB),
    /// read back as the real app reads a device. Demo mode starts from
    /// `DeviceState::default()` (36 dB), as `shurectl --demo` does.
    pub fn new(model: DeviceModel, fake: bool) -> Result<Self> {
        let fake = fake.then(|| FakeDevice::new(model));
        let device = fake.as_ref().map(FakeDevice::open);
        let now = Instant::now();
        let mut app = App {
            demo_mode: device.is_none(),
            device_model: model,
            meter_clock: Some(now),
            ..App::default()
        };
        load_initial_state(&mut app, device.as_ref());
        let preset_dir = TempDir::new()?;
        presets::set_dir_override(Some(preset_dir.0.clone()));
        app.presets = presets::load_all_presets();
        let mut harness = Self {
            model,
            reply_cursor: fake.as_ref().map_or(0, FakeDevice::log_len),
            expect_cursor: fake.as_ref().map_or(0, FakeDevice::log_len),
            fake,
            app,
            device,
            terminal: Terminal::new(TestBackend::new(DEFAULT_SIZE.0, DEFAULT_SIZE.1))?,
            loop_state: LoopState::new(now),
            now,
            hits: HitMap::default(),
            fits: true,
            previous_screen: Vec::new(),
            recording: None,
            preset_dir,
        };
        harness.draw()?;
        harness.previous_screen = harness.screen_lines();
        Ok(harness)
    }

    fn draw(&mut self) -> Result<()> {
        let app = &self.app;
        let mut hits = HitMap::default();
        let mut fits = true;
        self.terminal.draw(|f| {
            fits = ui::fits(f.area());
            hits = ui::draw(f, app);
        })?;
        self.hits = hits;
        self.fits = fits;
        Ok(())
    }

    fn buffer(&self) -> &Buffer {
        self.terminal.backend().buffer()
    }

    /// The screen as one string per row.
    pub fn screen_lines(&self) -> Vec<String> {
        let buffer = self.buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// The first cell where `text` starts, searching row by row.
    fn find_text(&self, text: &str) -> Option<(u16, u16)> {
        let buffer = self.buffer();
        for y in 0..buffer.area.height {
            // Track each symbol's column, so wide symbols don't shift the match.
            let mut row = String::new();
            let mut columns = Vec::new();
            for x in 0..buffer.area.width {
                let symbol = buffer[(x, y)].symbol();
                for _ in symbol.chars() {
                    columns.push(x);
                }
                row.push_str(symbol);
            }
            if let Some(byte_index) = row.find(text) {
                let char_index = row[..byte_index].chars().count();
                return columns.get(char_index).map(|&x| (x, y));
            }
        }
        None
    }

    fn packets_since(&self, start: usize) -> Vec<Packet> {
        self.fake
            .as_ref()
            .map_or_else(Vec::new, |fake| fake.log_since(start))
    }

    fn log_len(&self) -> usize {
        self.fake.as_ref().map_or(0, FakeDevice::log_len)
    }

    /// Run one event through `step()` and redraw, as the real loop does. A
    /// wheel adjustment is flushed by the next empty pass, as it is when no
    /// further event is queued.
    fn event(&mut self, event: Event) -> Result<()> {
        step(
            &mut self.app,
            &mut self.device,
            &self.hits,
            self.fits,
            Some(event),
            self.now,
            &mut self.loop_state,
        );
        if self.loop_state.pending_scroll.is_some() {
            step(
                &mut self.app,
                &mut self.device,
                &self.hits,
                self.fits,
                None,
                self.now,
                &mut self.loop_state,
            );
        }
        self.draw()
    }

    /// Advance the fake clock one frame at a time, running the presence poll
    /// whenever it falls due.
    fn tick(&mut self, total: Duration) -> Result<()> {
        let mut elapsed = Duration::ZERO;
        while elapsed < total {
            let frame = FRAME.min(total - elapsed);
            elapsed += frame;
            self.now += frame;
            self.app.meter_clock = Some(self.now);
            step(
                &mut self.app,
                &mut self.device,
                &self.hits,
                self.fits,
                None,
                self.now,
                &mut self.loop_state,
            );
            self.draw()?;
        }
        Ok(())
    }

    /// Handle one line and build its reply.
    pub fn handle_line(&mut self, line: &str, line_number: usize) -> Value {
        let mut outcome = Outcome::default();
        match serde_json::from_str::<Value>(line) {
            Ok(Value::Object(object)) => {
                if let Err(e) = self.dispatch(&object, line, &mut outcome) {
                    outcome.errors.push(format!("{e:#}"));
                }
            }
            Ok(_) => outcome
                .errors
                .push("each line must be a JSON object".into()),
            Err(e) => outcome.errors.push(format!("invalid JSON: {e}")),
        }
        self.reply(line_number, outcome)
    }

    fn dispatch(
        &mut self,
        line: &Map<String, Value>,
        raw: &str,
        outcome: &mut Outcome,
    ) -> Result<()> {
        if let Some(query) = line.get("query") {
            outcome.result = Some(self.query(query.as_str().unwrap_or_default())?);
            return Ok(());
        }
        if let Some(record) = line.get("record") {
            return self.record(record, line, outcome);
        }
        if let Some(expect) = line.get("expect") {
            let Value::Object(expect) = expect else {
                bail!("\"expect\" takes an object");
            };
            self.expect(expect, outcome);
            self.expect_cursor = self.log_len();
        } else if line.contains_key("scenario") {
            if let Some(size) = line.get("size") {
                self.resize(size)?;
            }
        } else {
            self.input(line, outcome)?;
        }
        if let Some(recording) = &mut self.recording
            && !line.contains_key("scenario")
        {
            recording.push(raw.trim().to_string());
        }
        Ok(())
    }

    fn input(&mut self, line: &Map<String, Value>, outcome: &mut Outcome) -> Result<()> {
        let (name, value) = line
            .iter()
            .find(|(name, _)| !["mods", "at", "repeat"].contains(&name.as_str()))
            .ok_or_else(|| anyhow!("empty line"))?;
        match name.as_str() {
            "key" => {
                let code = key_code(value.as_str().unwrap_or_default())?;
                let mods = modifiers(line.get("mods").and_then(Value::as_str).unwrap_or(""))?;
                let repeat = line.get("repeat").and_then(Value::as_u64).unwrap_or(1);
                if !(1..=MAX_REPEAT).contains(&repeat) {
                    bail!("\"repeat\" is 1 to {MAX_REPEAT}");
                }
                for _ in 0..repeat {
                    self.event(Event::Key(KeyEvent::new(code, mods)))?;
                }
                Ok(())
            }
            "type" => {
                for c in value.as_str().unwrap_or_default().chars() {
                    self.event(Event::Key(KeyEvent::new(
                        KeyCode::Char(c),
                        KeyModifiers::NONE,
                    )))?;
                }
                Ok(())
            }
            "click" => {
                let (column, row) = cell(value)?;
                self.event(mouse_event(
                    MouseEventKind::Down(MouseButton::Left),
                    column,
                    row,
                ))
            }
            "click_on" => {
                let text = value.as_str().unwrap_or_default();
                let (column, row) = self
                    .find_text(text)
                    .ok_or_else(|| anyhow!("\"{text}\" is not on the screen"))?;
                self.event(mouse_event(
                    MouseEventKind::Down(MouseButton::Left),
                    column,
                    row,
                ))
            }
            "wheel" => {
                let kind = match value.as_str() {
                    Some("up") => MouseEventKind::ScrollUp,
                    Some("down") => MouseEventKind::ScrollDown,
                    _ => bail!("\"wheel\" is \"up\" or \"down\""),
                };
                let (column, row) = cell(line.get("at").unwrap_or(&Value::Null))?;
                self.event(mouse_event(kind, column, row))
            }
            "resize" => self.resize(value),
            "tick" => {
                let ms = value
                    .as_u64()
                    .ok_or_else(|| anyhow!("\"tick\" takes milliseconds"))?;
                let total = Duration::from_millis(ms);
                if total > MAX_TICK {
                    bail!("one tick may advance at most {} ms", MAX_TICK.as_millis());
                }
                self.tick(total)
            }
            "device" => self.device_input(value, outcome),
            "meter" => {
                let dbfs = value
                    .as_f64()
                    .ok_or_else(|| anyhow!("\"meter\" takes dBFS"))?;
                if let Ok(mut window) = self.app.peak_window.lock() {
                    window.push(self.now, (dbfs * 10.0).round() as i32);
                }
                self.draw()
            }
            "preset_dir" => self.preset_dir(value.as_str().unwrap_or_default()),
            other => bail!("unknown line \"{other}\""),
        }
    }

    fn resize(&mut self, size: &Value) -> Result<()> {
        let (width, height) = cell(size)?;
        if width == 0 || height == 0 || width > MAX_SIZE.0 || height > MAX_SIZE.1 {
            bail!("size must be between 1x1 and {}x{}", MAX_SIZE.0, MAX_SIZE.1);
        }
        self.terminal.backend_mut().resize(width, height);
        self.terminal
            .resize(ratatui::layout::Rect::new(0, 0, width, height))?;
        self.draw()
    }

    fn device_input(&mut self, value: &Value, outcome: &mut Outcome) -> Result<()> {
        let Some(fake) = &self.fake else {
            outcome
                .skipped
                .push("device: demo mode has no device".into());
            return Ok(());
        };
        match value {
            Value::String(action) if action == "unplug" => fake.unplug(),
            Value::String(action) if action == "replug" => fake.replug(),
            Value::Object(faults) => {
                for (name, amount) in faults {
                    let count = amount.as_u64().unwrap_or(0) as usize;
                    match name.as_str() {
                        "drop_acks" => fake.faults_mut(|f| f.drop_acks = count),
                        "extra_acks" => fake.faults_mut(|f| f.extra_acks = count),
                        "refuse" => fake.faults_mut(|f| f.refuse = count),
                        "refused_get" => {
                            let feature = match amount {
                                Value::Null => None,
                                Value::String(name) if fake.feature_names().contains(name) => {
                                    Some(name.clone())
                                }
                                _ => bail!("\"refused_get\" takes a feature name or null"),
                            };
                            fake.faults_mut(|f| f.refused_get = feature);
                        }
                        "firmware" => {
                            let version = match amount {
                                Value::Null => None,
                                Value::String(version) => Some(version.clone()),
                                _ => bail!("\"firmware\" takes a version string or null"),
                            };
                            fake.faults_mut(|f| f.firmware_version = version);
                        }
                        "muted" => match amount {
                            Value::Bool(muted) => fake.set_muted_on_device(*muted),
                            _ => bail!("\"muted\" takes true or false"),
                        },
                        "mode" => {
                            let mode = match amount.as_str() {
                                Some("Auto") => InputMode::Auto,
                                Some("Manual") => InputMode::Manual,
                                _ => bail!("\"mode\" takes \"Auto\" or \"Manual\""),
                            };
                            let changed = DeviceState {
                                mode,
                                ..DeviceState::default()
                            };
                            fake.set_on_device(LiveField::Mode, &changed);
                        }
                        "gain_tenths" | "monitor_mix" | "playback_mix" => {
                            let Some(value) = amount.as_u64() else {
                                bail!("\"{name}\" takes a number");
                            };
                            let mut changed = DeviceState::default();
                            let field = match name.as_str() {
                                "gain_tenths" => {
                                    changed.gain_tenths = u16::try_from(value)?;
                                    LiveField::Gain
                                }
                                "monitor_mix" => {
                                    changed.monitor_mix = u8::try_from(value)?;
                                    LiveField::MonitorMix
                                }
                                _ => {
                                    changed.playback_mix = u8::try_from(value)?;
                                    LiveField::PlaybackMix
                                }
                            };
                            fake.set_on_device(field, &changed);
                        }
                        "fail_io" => {
                            fake.faults_mut(|f| f.fail_io = amount.as_bool().unwrap_or(false))
                        }
                        other => bail!("unknown device fault \"{other}\""),
                    }
                }
            }
            _ => bail!("\"device\" takes \"unplug\", \"replug\" or an object of faults"),
        }
        Ok(())
    }

    /// Copy the preset files in `dir` into this session's preset directory and
    /// reload them, for old-file compatibility scenarios.
    fn preset_dir(&mut self, dir: &str) -> Result<()> {
        for entry in std::fs::read_dir(dir).with_context(|| format!("Cannot read {dir}"))? {
            let path = entry?.path();
            if let Some(name) = path.file_name() {
                std::fs::copy(&path, self.preset_dir.0.join(name))?;
            }
        }
        self.app.presets = presets::load_all_presets();
        self.draw()
    }

    fn record(
        &mut self,
        record: &Value,
        line: &Map<String, Value>,
        outcome: &mut Outcome,
    ) -> Result<()> {
        match record.as_str() {
            Some("start") => {
                self.recording = Some(Vec::new());
                Ok(())
            }
            Some("save") => {
                let lines = self.recording.take().ok_or_else(|| {
                    anyhow!("not recording; send {{\"record\": \"start\"}} first")
                })?;
                let path = line
                    .get("as")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("\"record\": \"save\" needs \"as\": a .jsonl path"))?;
                let saved = self.save_scenario(Path::new(path), line.get("models"), &lines)?;
                outcome.result = Some(json!({ "saved": saved }));
                if !lines.iter().any(|l| l.contains("\"expect\"")) {
                    outcome.errors.push(
                        "saved, but the runner refuses a scenario with no expect line".into(),
                    );
                }
                Ok(())
            }
            _ => bail!("\"record\" is \"start\" or \"save\""),
        }
    }

    fn save_scenario(
        &self,
        path: &Path,
        models: Option<&Value>,
        lines: &[String],
    ) -> Result<String> {
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            bail!("scenario files end in .jsonl");
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("scenario");
        let models = models
            .cloned()
            .unwrap_or_else(|| json!([model_name(self.model)]));
        let area = self.buffer().area;
        let header =
            json!({ "scenario": name, "models": models, "size": [area.width, area.height] });
        let mut text = format!("{header}\n");
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        std::fs::write(path, text).with_context(|| format!("Cannot write {}", path.display()))?;
        Ok(path.display().to_string())
    }

    fn query(&mut self, query: &str) -> Result<Value> {
        Ok(match query {
            "screen" => json!(self.screen_lines()),
            "screen_styled" => self.styled_screen(),
            "state" => state_json(&self.app.device_state),
            "hitmap" => Value::Array(
                self.hits
                    .regions()
                    .iter()
                    .map(|(area, target)| {
                        json!({ "x": area.x, "y": area.y, "width": area.width,
                                "height": area.height, "target": format!("{target:?}") })
                    })
                    .collect(),
            ),
            "packets" => Value::Array(self.packets_since(0).iter().map(Packet::to_json).collect()),
            "models" => json!(MODEL_NAMES.map(|(name, _)| name)),
            "focus_ring" => json!(self.focus_ring()),
            "features" => json!(self.fake.as_ref().map(FakeDevice::feature_names)),
            other => bail!(
                "unknown query \"{other}\"; try screen, screen_styled, state, hitmap, \
                 packets, models, focus_ring or features"
            ),
        })
    }

    /// Every focus Down visits from the current one, in order.
    fn focus_ring(&mut self) -> Vec<String> {
        let (focus, band) = (self.app.focus, self.app.eq_selected_band);
        let mut ring = Vec::new();
        while !ring.contains(&self.app.focus) {
            ring.push(self.app.focus);
            self.app.focus_next();
        }
        (self.app.focus, self.app.eq_selected_band) = (focus, band);
        ring.iter().map(|f| format!("{f:?}")).collect()
    }

    /// Each row as runs of text sharing a style, colors by palette name.
    fn styled_screen(&self) -> Value {
        let buffer = self.buffer();
        let rows: Vec<Value> = (0..buffer.area.height)
            .map(|y| {
                let mut runs: Vec<(String, String, String)> = Vec::new();
                for x in 0..buffer.area.width {
                    let cell = &buffer[(x, y)];
                    let (fg, bg) = (color_name(cell.fg), color_name(cell.bg));
                    match runs.last_mut() {
                        Some(run) if run.1 == fg && run.2 == bg => run.0.push_str(cell.symbol()),
                        Some(_) | None => runs.push((cell.symbol().to_string(), fg, bg)),
                    }
                }
                Value::Array(
                    runs.into_iter()
                        .filter(|run| !run.0.trim().is_empty())
                        .map(|(text, fg, bg)| json!({ "text": text, "fg": fg, "bg": bg }))
                        .collect(),
                )
            })
            .collect();
        Value::Array(rows)
    }

    fn app_json(&self) -> Value {
        let app = &self.app;
        json!({
            "model": model_name(self.model),
            "demo": app.demo_mode,
            "tab": format!("{:?}", app.active_tab),
            "focus": format!("{:?}", app.focus),
            "status": app.status_message,
            "status_is_error": app.status_is_error,
            "help_visible": app.help_visible,
            "editing": app.editing_name(),
            "name_draft": app.name_draft,
            "confirming_factory_reset": app.confirming_factory_reset,
            "confirming_preset_load": app.confirming_preset_load,
            "connected": app.device_connected,
            "quit": app.should_quit,
            "fits": self.fits,
        })
    }

    fn reply(&mut self, line_number: usize, outcome: Outcome) -> Value {
        let screen = self.screen_lines();
        let changed: Vec<usize> = (0..screen.len())
            .filter(|&row| self.previous_screen.get(row) != screen.get(row))
            .collect();
        let packets = self.packets_since(self.reply_cursor);
        self.reply_cursor = self.log_len();
        let mut errors = outcome.errors;
        errors.extend(packets.iter().filter_map(|packet| match &packet.kind {
            PacketKind::Refused { reason } => Some(format!(
                "the fake device refused a write ({reason}): {}",
                hex(&packet.raw)
            )),
            PacketKind::Get { .. }
            | PacketKind::Set { .. }
            | PacketKind::Confirm
            | PacketKind::Text { .. } => None,
        }));
        let listed: Vec<Value> = packets
            .iter()
            .take(MAX_PACKETS_PER_REPLY)
            .map(Packet::to_json)
            .collect();
        let mut reply = json!({
            "line": line_number,
            "ok": outcome.failures.is_empty() && errors.is_empty(),
            "app": self.app_json(),
            "screen": screen,
            "changed": changed,
            "packets": listed,
            "failures": outcome.failures,
            "errors": errors,
        });
        if packets.len() > MAX_PACKETS_PER_REPLY {
            reply["packets_omitted"] = json!(packets.len() - MAX_PACKETS_PER_REPLY);
        }
        if !outcome.skipped.is_empty() {
            reply["skipped"] = json!(outcome.skipped);
        }
        if let Some(result) = outcome.result {
            reply["result"] = result;
        }
        self.previous_screen = screen;
        reply
    }

    // ── Expectations ──────────────────────────────────────────────────────────

    fn expect(&mut self, expect: &Map<String, Value>, outcome: &mut Outcome) {
        let screen = self.screen_lines();
        let app = self.app_json();
        let packets = self.packets_since(self.expect_cursor);
        for (key, expected) in expect {
            if key == "click_targets" {
                if expected.as_bool() == Some(true) {
                    outcome.failures.extend(self.check_click_targets());
                }
                continue;
            }
            if self.fake.is_none() && PACKET_KEYS.contains(&key.as_str()) {
                outcome
                    .skipped
                    .push(format!("{key}: demo mode has no device"));
                continue;
            }
            if let Err(failure) = self.check(key, expected, &screen, &app, &packets) {
                outcome.failures.push(format!("{key}: {failure}"));
            }
        }
    }

    /// One expectation. `Err` describes what was found instead.
    fn check(
        &self,
        key: &str,
        expected: &Value,
        screen: &[String],
        app: &Value,
        packets: &[Packet],
    ) -> std::result::Result<(), String> {
        let text = || expected.as_str().unwrap_or_default();
        match key {
            "screen_contains" | "screen_not_contains" => {
                let wanted: Vec<&str> = match expected {
                    Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
                    _ => vec![text()],
                };
                let present = key == "screen_contains";
                for item in wanted {
                    if screen.iter().any(|row| row.contains(item)) != present {
                        return Err(format!(
                            "\"{item}\" {} the screen",
                            if present { "is not on" } else { "is on" }
                        ));
                    }
                }
                Ok(())
            }
            "screen_row" => {
                let (row, wanted) = match expected.as_array().map(Vec::as_slice) {
                    Some([row, wanted]) => (
                        row.as_u64().unwrap_or(u64::MAX) as usize,
                        wanted.as_str().unwrap_or_default(),
                    ),
                    _ => return Err("takes [row, \"text\"]".into()),
                };
                match screen.get(row) {
                    Some(line) if line.contains(wanted) => Ok(()),
                    Some(line) => Err(format!("row {row} is \"{line}\"")),
                    None => Err(format!("there is no row {row}")),
                }
            }
            "screen_hash" => {
                let actual = format!("{:016x}", ui::buffer_hash(self.buffer()));
                compare(&json!(actual), expected)
            }
            "style" => self.check_style(expected),
            "tab"
            | "focus"
            | "status_is_error"
            | "help_visible"
            | "editing"
            | "quit"
            | "fits"
            | "confirming_factory_reset"
            | "confirming_preset_load" => compare(&app[key], expected),
            "status" => {
                let status = app["status"].as_str().unwrap_or_default();
                if status.contains(text()) {
                    Ok(())
                } else {
                    Err(format!("status is \"{status}\""))
                }
            }
            "connected" => {
                let shows_disconnected = screen.iter().any(|row| row.contains("[DISCONNECTED]"));
                compare(&app["connected"], expected)?;
                // Below the minimum size only the notice is drawn, no header.
                if self.fits && shows_disconnected == expected.as_bool().unwrap_or(true) {
                    return Err("the header disagrees with App::device_connected".into());
                }
                Ok(())
            }
            "state" => {
                let state = state_json(&self.app.device_state);
                let Value::Object(fields) = expected else {
                    return Err("takes an object of DeviceState fields".into());
                };
                for (field, value) in fields {
                    match state.get(field) {
                        Some(actual) => {
                            compare(actual, value).map_err(|e| format!("{field}: {e}"))?
                        }
                        None => return Err(format!("DeviceState has no field \"{field}\"")),
                    }
                }
                Ok(())
            }
            "set" => {
                let sets = decoded_sets(packets);
                if sets.iter().any(|set| set_matches(set, expected)) {
                    Ok(())
                } else {
                    Err(format!("SETs sent: {}", json!(sets)))
                }
            }
            "sets" => {
                let sets = decoded_sets(packets);
                let mut remaining = sets.iter();
                for wanted in expected.as_array().map(Vec::as_slice).unwrap_or_default() {
                    if !remaining.any(|set| set_matches(set, wanted)) {
                        return Err(format!(
                            "{wanted} not found in order; SETs sent: {}",
                            json!(sets)
                        ));
                    }
                }
                Ok(())
            }
            "get" => {
                let found = packets
                    .iter()
                    .any(|p| matches!(&p.kind, PacketKind::Get { feature } if feature == text()));
                if found {
                    Ok(())
                } else {
                    Err(format!("no GET of {}", text()))
                }
            }
            "no_packets" => {
                let none = packets.is_empty();
                if none == expected.as_bool().unwrap_or(true) {
                    Ok(())
                } else {
                    Err(format!(
                        "{} packets sent: {}",
                        packets.len(),
                        json!(packets.iter().map(Packet::to_json).collect::<Vec<_>>())
                    ))
                }
            }
            "packets_raw" => {
                for (i, wanted) in expected
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                {
                    let actual = packets.get(i).map(|p| hex(&p.raw)).unwrap_or_default();
                    if !actual.starts_with(wanted.as_str().unwrap_or_default()) {
                        return Err(format!("packet {i} is \"{actual}\""));
                    }
                }
                Ok(())
            }
            "text_sent" => {
                let lines: Vec<&str> = packets
                    .iter()
                    .filter_map(|p| match &p.kind {
                        PacketKind::Text { line, .. } => Some(line.as_str()),
                        PacketKind::Get { .. }
                        | PacketKind::Set { .. }
                        | PacketKind::Confirm
                        | PacketKind::Refused { .. } => None,
                    })
                    .collect();
                if lines.contains(&text()) {
                    Ok(())
                } else {
                    Err(format!("lines sent: {lines:?}"))
                }
            }
            other => Err(format!("unknown expectation \"{other}\"")),
        }
    }

    /// Click every control region on the screen that isn't focused, in drawing
    /// order, at its first cell that resolves to it (a later, more specific
    /// region wins where they overlap). Each first click must focus exactly that
    /// control and send nothing (bc3bb3f), and no region may be fully covered.
    fn check_click_targets(&mut self) -> Vec<String> {
        use crate::mouse::Target;
        let mut failures = Vec::new();
        let controls: Vec<_> = self
            .hits
            .regions()
            .iter()
            .filter_map(|&(_, target)| match target {
                Target::Control(focus) => Some(focus),
                Target::Tab(_)
                | Target::PresetAction(..)
                | Target::LoadButton
                | Target::CancelButton
                | Target::LoadBox => None,
            })
            .collect();
        for focus in controls {
            if self.app.focus == focus {
                continue;
            }
            // Each click moves focus and redraws, which can move the regions, so
            // look the control up in the hit map of the frame on screen now.
            let target = Target::Control(focus);
            let Some(area) = self
                .hits
                .regions()
                .iter()
                .find_map(|&(area, found)| (found == target).then_some(area))
            else {
                failures.push(format!(
                    "click_targets: {focus:?} has no region once focus moved to {:?}",
                    self.app.focus
                ));
                continue;
            };
            let Some((x, y)) = area
                .positions()
                .map(|p| (p.x, p.y))
                .find(|&(x, y)| self.hits.target_at(x, y) == Some(target))
            else {
                failures.push(format!(
                    "click_targets: {focus:?}'s region is covered by others"
                ));
                continue;
            };
            let before = self.log_len();
            let click = mouse_event(MouseEventKind::Down(MouseButton::Left), x, y);
            if let Err(e) = self.event(click) {
                failures.push(format!("click_targets: {e:#}"));
            }
            if self.app.focus != focus {
                failures.push(format!(
                    "click_targets: clicking ({x}, {y}) in {focus:?}'s region focused {:?}",
                    self.app.focus
                ));
            }
            if self.log_len() != before {
                failures.push(format!(
                    "click_targets: a first click on {focus:?} sent packets"
                ));
            }
        }
        failures
    }

    fn check_style(&self, expected: &Value) -> std::result::Result<(), String> {
        let text = expected["text"].as_str().unwrap_or_default();
        let (x, y) = self
            .find_text(text)
            .ok_or_else(|| format!("\"{text}\" is not on the screen"))?;
        let cell = &self.buffer()[(x, y)];
        for (key, color) in [("fg", cell.fg), ("bg", cell.bg)] {
            if let Some(wanted) = expected.get(key) {
                compare(&json!(color_name(color)), wanted)
                    .map_err(|e| format!("{key} of \"{text}\": {e}"))?;
            }
        }
        Ok(())
    }
}

/// Equal JSON, with numbers compared by value (so 250 matches 250.0).
fn compare(actual: &Value, expected: &Value) -> std::result::Result<(), String> {
    let equal = match (actual.as_f64(), expected.as_f64()) {
        (Some(a), Some(b)) => a == b,
        _ => actual == expected,
    };
    if equal {
        Ok(())
    } else {
        Err(format!("expected {expected}, got {actual}"))
    }
}

fn decoded_sets(packets: &[Packet]) -> Vec<Value> {
    packets
        .iter()
        .filter_map(|packet| match &packet.kind {
            PacketKind::Set {
                feature,
                value,
                applied,
            } => Some(json!({ "feature": feature, "value": value, "applied": applied })),
            PacketKind::Get { .. }
            | PacketKind::Confirm
            | PacketKind::Text { .. }
            | PacketKind::Refused { .. } => None,
        })
        .collect()
}

/// `{"feature": "GAIN"}` matches any GAIN SET; a `value` (or `applied`) must match too.
fn set_matches(set: &Value, wanted: &Value) -> bool {
    let wanted = match wanted {
        Value::String(feature) => &json!({ "feature": feature }),
        other => other,
    };
    ["feature", "value", "applied"].iter().all(|key| {
        wanted
            .get(key)
            .is_none_or(|w| compare(&set[key], w).is_ok())
    })
}

fn color_name(color: ratatui::style::Color) -> String {
    match ui::palette_name(color) {
        Some(name) => name.to_string(),
        None => format!("{color:?}"),
    }
}

fn model_name(model: DeviceModel) -> &'static str {
    MODEL_NAMES
        .iter()
        .find(|(_, m)| *m == model)
        .map_or("unknown", |(name, _)| name)
}

fn cell(value: &Value) -> Result<(u16, u16)> {
    match value.as_array().map(Vec::as_slice) {
        Some([a, b]) => match (a.as_u64(), b.as_u64()) {
            (Some(a), Some(b)) => Ok((u16::try_from(a)?, u16::try_from(b)?)),
            _ => bail!("expected [column, row]"),
        },
        _ => bail!("expected [column, row]"),
    }
}

fn mouse_event(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

/// A key named as crossterm names it: `Tab`, `BackTab`, `Enter`, `Esc`, `Up`,
/// `Left`, `Backspace`, `Delete`, `F1`, or a single character.
fn key_code(name: &str) -> Result<KeyCode> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Ok(KeyCode::Char(c));
    }
    Ok(match name {
        "Tab" => KeyCode::Tab,
        "BackTab" => KeyCode::BackTab,
        "Enter" => KeyCode::Enter,
        "Esc" => KeyCode::Esc,
        "Space" => KeyCode::Char(' '),
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "Backspace" => KeyCode::Backspace,
        "Delete" => KeyCode::Delete,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        other => match other.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
            Some(n) => KeyCode::F(n),
            None => bail!("unknown key \"{other}\""),
        },
    })
}

/// `"shift"`, `"ctrl"`, `"alt"`, or several joined by `+`.
fn modifiers(names: &str) -> Result<KeyModifiers> {
    let mut mods = KeyModifiers::NONE;
    for name in names.split('+').map(str::trim).filter(|n| !n.is_empty()) {
        mods |= match name.to_ascii_lowercase().as_str() {
            "shift" => KeyModifiers::SHIFT,
            "ctrl" | "control" => KeyModifiers::CONTROL,
            "alt" => KeyModifiers::ALT,
            other => bail!("unknown modifier \"{other}\""),
        };
    }
    Ok(mods)
}

/// Every `DeviceState` field by name. The destructuring is exhaustive, so a new
/// field is a compile error here until scenarios can name it.
pub fn state_json(state: &DeviceState) -> Value {
    let DeviceState {
        gain_tenths,
        mode,
        auto_position,
        auto_tone,
        auto_gain,
        muted,
        phantom_power,
        monitor_mix,
        playback_mix,
        limiter_enabled,
        compressor,
        hpf,
        eq_enabled,
        eq_bands,
        locked,
        denoiser_enabled,
        popper_stopper_enabled,
        mute_btn_disabled,
        tone,
        mv6_gain_locked,
        adaptation_rate_fast,
        denoiser_level,
        tone_advanced,
        reverb_on_output,
        reverb_monitoring,
        reverb_type,
        reverb_intensity,
        led_behavior,
        led_brightness,
        led_live_theme,
        led_solid_theme,
        led_pulsing_theme,
        led_solid_rgb,
        led_pulsing_rgb,
        led_live_edge_rgb,
        led_live_middle_rgb,
        led_live_interior_rgb,
        eq_preset,
        led_live_meter,
        led_night_mode,
        serial_number,
        device_name,
        firmware_version,
        factory_serial,
    } = state;
    let debug = |value: &dyn std::fmt::Debug| json!(format!("{value:?}"));
    let fields = [
        ("gain_tenths", json!(gain_tenths)),
        ("mode", debug(mode)),
        ("auto_position", debug(auto_position)),
        ("auto_tone", debug(auto_tone)),
        ("auto_gain", debug(auto_gain)),
        ("muted", json!(muted)),
        ("phantom_power", json!(phantom_power)),
        ("monitor_mix", json!(monitor_mix)),
        ("playback_mix", json!(playback_mix)),
        ("limiter_enabled", json!(limiter_enabled)),
        ("compressor", debug(compressor)),
        ("hpf", debug(hpf)),
        ("eq_enabled", json!(eq_enabled)),
        (
            "eq_band_gains",
            json!(eq_bands.iter().map(|band| band.gain_db).collect::<Vec<_>>()),
        ),
        (
            "eq_band_enables",
            json!(eq_bands.iter().map(|band| band.enabled).collect::<Vec<_>>()),
        ),
        ("locked", json!(locked)),
        ("denoiser_enabled", json!(denoiser_enabled)),
        ("popper_stopper_enabled", json!(popper_stopper_enabled)),
        ("mute_btn_disabled", json!(mute_btn_disabled)),
        ("tone", json!(tone)),
        ("mv6_gain_locked", json!(mv6_gain_locked)),
        ("adaptation_rate_fast", json!(adaptation_rate_fast)),
        ("denoiser_level", json!(denoiser_level)),
        ("tone_advanced", json!(tone_advanced)),
        ("reverb_on_output", json!(reverb_on_output)),
        ("reverb_monitoring", json!(reverb_monitoring)),
        ("reverb_type", debug(reverb_type)),
        ("reverb_intensity", json!(reverb_intensity)),
        ("led_behavior", debug(led_behavior)),
        ("led_brightness", debug(led_brightness)),
        ("led_live_theme", debug(led_live_theme)),
        ("led_solid_theme", debug(led_solid_theme)),
        ("led_pulsing_theme", debug(led_pulsing_theme)),
        ("led_solid_rgb", json!(led_solid_rgb)),
        ("led_pulsing_rgb", json!(led_pulsing_rgb)),
        ("led_live_edge_rgb", json!(led_live_edge_rgb)),
        ("led_live_middle_rgb", json!(led_live_middle_rgb)),
        ("led_live_interior_rgb", json!(led_live_interior_rgb)),
        ("eq_preset", debug(eq_preset)),
        ("led_live_meter", json!(led_live_meter)),
        ("led_night_mode", json!(led_night_mode)),
        ("serial_number", json!(serial_number)),
        ("device_name", json!(device_name)),
        ("firmware_version", json!(firmware_version)),
        ("factory_serial", json!(factory_serial)),
    ];
    Value::Object(
        fields
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect(),
    )
}

/// `shurectl --qa`: answer JSON lines on stdin until it closes. `{"reset": true}`
/// starts a fresh session on the same model.
#[cfg(feature = "qa")]
pub fn run_qa(model: DeviceModel, fake: bool) -> Result<()> {
    use std::io::{BufRead, Write};

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut harness = Harness::new(model, fake)?;
    let mut line_number = 0;
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        line_number += 1;
        let is_reset =
            serde_json::from_str::<Value>(&line).is_ok_and(|value| value.get("reset").is_some());
        let reply = if is_reset {
            harness = Harness::new(model, fake)?;
            line_number = 0;
            json!({ "reset": true, "ok": true, "app": harness.app_json() })
        } else {
            harness.handle_line(&line, line_number)
        };
        writeln!(stdout, "{reply}")?;
        stdout.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(harness: &mut Harness, lines: &[&str]) -> Vec<Value> {
        lines
            .iter()
            .enumerate()
            .map(|(i, line)| harness.handle_line(line, i + 1))
            .collect()
    }

    fn assert_ok(replies: &[Value]) {
        for reply in replies {
            assert_eq!(reply["ok"], true, "{reply:#}");
        }
    }

    #[test]
    fn failed_expectations_report_the_actual_value() {
        let mut harness = Harness::new(DeviceModel::Mv7Plus, true).expect("harness");
        let reply =
            harness.handle_line(r#"{"expect": {"tab": "Eq", "state": {"muted": true}}}"#, 1);
        assert_eq!(reply["ok"], false);
        let failures = reply["failures"].to_string();
        assert!(failures.contains("got \\\"Main\\\""), "{failures}");
        assert!(failures.contains("muted"), "{failures}");
    }

    #[test]
    fn demo_mode_skips_packet_expectations_and_sends_nothing() {
        let mut harness = Harness::new(DeviceModel::Mvx2u, false).expect("harness");
        let replies = run(
            &mut harness,
            &[
                r#"{"key": "Down"}"#,
                r#"{"key": "Enter"}"#,
                r#"{"expect": {"set": {"feature": "MUTE", "value": true}, "state": {"muted": true}, "no_packets": true}}"#,
            ],
        );
        assert_ok(&replies);
        assert!(replies[2]["skipped"].to_string().contains("set"));
    }

    #[test]
    fn click_on_text_focuses_the_control_under_it() {
        let mut harness = Harness::new(DeviceModel::Mv7Plus, true).expect("harness");
        let replies = run(
            &mut harness,
            &[
                r#"{"click_on": "Mic Level"}"#,
                r#"{"expect": {"focus": "MonitorMix"}}"#,
            ],
        );
        assert_ok(&replies);
    }

    #[test]
    fn unknown_lines_and_keys_are_errors() {
        let mut harness = Harness::new(DeviceModel::Mv6, true).expect("harness");
        for line in [
            r#"{"bogus": 1}"#,
            r#"{"key": "Nope"}"#,
            "not json",
            r#"{"expect": {"tabb": "Main"}}"#,
        ] {
            assert_eq!(harness.handle_line(line, 1)["ok"], false, "{line}");
        }
    }
}
