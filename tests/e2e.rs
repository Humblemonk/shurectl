//! End-to-end tests. Run with `cargo test --features qa --test e2e`.
//!
//! `scenarios` pipes every file in `tests/scenarios/` through the built binary's
//! `--qa` mode, once per model the file's header names and once per device mode
//! (a fake device, and demo mode with no device), so CI tests the same binary and
//! protocol an agent uses. `pty_smoke` starts the real TUI under a pseudo-terminal.
//!
//! A scenario file is JSON lines. The first is the header:
//! `{"scenario": "name", "models": "all" | ["mv6", ...], "modes": ["fake", "demo"], "size": [120, 40]}`
//! (`modes` and `size` are optional). Every other line is an input or an
//! `expect` line, as `.claude/skills/qa/SKILL.md` describes.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

const MODELS: [&str; 6] = ["mvx2u", "mvx2u-gen2", "mv6", "mv6-gen2", "mv7", "mv7plus"];
const MODES: [&str; 2] = ["fake", "demo"];

fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

struct Scenario {
    name: String,
    header: String,
    models: Vec<String>,
    modes: Vec<String>,
    /// `(line number in the file, line)`, header excluded.
    lines: Vec<(usize, String)>,
}

fn string_list(value: &Value, all: &[&str]) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        _ => all.iter().map(|s| s.to_string()).collect(),
    }
}

fn load_scenarios() -> Vec<Scenario> {
    let dir = tests_dir().join("scenarios");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).expect("read scenario");
            let mut lines = text
                .lines()
                .enumerate()
                .map(|(i, line)| (i + 1, line.trim().to_string()))
                .filter(|(_, line)| !line.is_empty());
            let (_, header) = lines.next().unwrap_or_default();
            let parsed: Value = serde_json::from_str(&header)
                .unwrap_or_else(|e| panic!("{}: bad header: {e}", path.display()));
            let name = parsed["scenario"]
                .as_str()
                .unwrap_or_else(|| panic!("{}: the header needs \"scenario\"", path.display()))
                .to_string();
            let models = string_list(&parsed["models"], &MODELS);
            for model in &models {
                assert!(
                    MODELS.contains(&model.as_str()),
                    "{}: unknown model {model}",
                    path.display()
                );
            }
            let lines: Vec<(usize, String)> = lines.collect();
            assert!(
                lines.iter().any(|(_, line)| line.contains("\"expect\"")),
                "{}: a scenario needs at least one expect line",
                path.display()
            );
            Scenario {
                name,
                header,
                models,
                modes: string_list(&parsed["modes"], &MODES),
                lines,
            }
        })
        .collect()
}

/// One long-lived `shurectl --qa` process for a model and device mode.
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Session {
    fn start(model: &str, mode: &str) -> Self {
        let device_flag = if mode == "fake" {
            "--fake-device"
        } else {
            "--demo"
        };
        let mut child = Command::new(env!("CARGO_BIN_EXE_shurectl"))
            .args(["--qa", device_flag, model])
            .current_dir(tests_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start shurectl --qa");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, line: &str) -> Value {
        writeln!(self.stdin, "{line}").expect("write to shurectl --qa");
        self.stdin.flush().expect("flush");
        let mut reply = String::new();
        self.stdout
            .read_line(&mut reply)
            .expect("read from shurectl --qa");
        serde_json::from_str(&reply).unwrap_or_else(|e| panic!("bad reply {reply:?}: {e}"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What a failed line found: the failures, the screen and the packets.
fn describe_failure(
    scenario: &Scenario,
    model: &str,
    mode: &str,
    number: usize,
    line: &str,
    reply: &Value,
) -> String {
    let screen: Vec<&str> = reply["screen"]
        .as_array()
        .map(|rows| rows.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    format!(
        "{} [{model}, {mode}] line {number}: {line}\n  failures: {}\n  errors: {}\n  app: {}\n  packets since the previous line: {}\n  screen:\n{}\n",
        scenario.name,
        reply["failures"],
        reply["errors"],
        reply["app"],
        reply["packets"],
        screen.join("\n")
    )
}

#[test]
fn scenarios() {
    let scenarios = load_scenarios();
    assert!(!scenarios.is_empty(), "no scenarios in tests/scenarios");
    let mut failures = Vec::new();
    let mut runs = 0;
    for model in MODELS {
        for mode in MODES {
            let mut session: Option<Session> = None;
            for scenario in scenarios.iter().filter(|s| {
                s.models.iter().any(|m| m == model) && s.modes.iter().any(|m| m == mode)
            }) {
                let session = session.get_or_insert_with(|| Session::start(model, mode));
                runs += 1;
                session.send(r#"{"reset": true}"#);
                session.send(&scenario.header);
                for (number, line) in &scenario.lines {
                    let reply = session.send(line);
                    if reply["ok"] != Value::Bool(true) {
                        failures.push(describe_failure(
                            scenario, model, mode, *number, line, &reply,
                        ));
                        break;
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {runs} scenario runs failed:\n\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The only test of the real terminal path: start the TUI under a
/// pseudo-terminal, wait for its header, quit with `q`, and check that it
/// exits cleanly and leaves the alternate screen.
#[test]
fn pty_smoke() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pty");
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_shurectl"));
    command.args(["--demo", "mv7plus"]);
    command.env("TERM", "xterm-256color");
    let mut child = pty.slave.spawn_command(command).expect("spawn shurectl");
    drop(pty.slave);

    // Reads block, so a thread forwards output until the pty closes.
    let mut reader = pty.master.try_clone_reader().expect("pty reader");
    let (sender, output) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || sender.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut writer = pty.master.take_writer().expect("pty writer");

    let mut seen = Vec::new();
    let wait_for = |seen: &mut Vec<u8>, needle: &str| {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !String::from_utf8_lossy(seen).contains(needle) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match output.recv_timeout(left) {
                Ok(chunk) => seen.extend(chunk),
                Err(_) => panic!(
                    "no {needle:?} within 10 s; output so far:\n{}",
                    String::from_utf8_lossy(seen)
                ),
            }
        }
    };
    wait_for(&mut seen, "DEMO");
    writer.write_all(b"q").expect("send q");
    writer.flush().expect("flush");
    // Leaving the alternate screen is the last thing the app draws.
    wait_for(&mut seen, "\x1b[?1049l");

    let status = child.wait().expect("wait for shurectl");
    assert!(status.success(), "exit status {status:?}");
}
