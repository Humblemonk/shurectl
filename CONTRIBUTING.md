# Contributing to shurectl

## Development Setup

### Prerequisites

- Rust (edition 2024)
- Linux: `libasound2-dev` and `libudev-dev`
- A supported Shure device for hardware testing, or use `--demo` for UI-only work

Optionally, [mise](https://mise.jdx.dev) installs the toolchain (Rust stable with clippy and
rustfmt, plus Node and jscpd for the duplication check) from `mise.toml`: run `mise install`.

### Building

```bash
git clone https://github.com/Humblemonk/shurectl.git
cd shurectl
cargo build
```

### Quality Gate

All of the following must pass clean before any commit:

```bash
cargo clippy --features probe,qa -- -D warnings && cargo build --all-targets --features probe,qa \
  && cargo fmt --check && cargo test && cargo test --features qa --test e2e
```

With mise, `mise run check` runs the same gate and `mise run jscpd` the duplication check.

The `--features probe` flag is required: `shurectl-probe` is feature-gated and would
otherwise be skipped by clippy entirely. The `qa` feature builds the headless `--qa`
harness that `tests/e2e.rs` drives; it is for maintainers and CI and never ships.

There are no warnings — only requirements.

---

## Using AI Assistants

> **If you use an AI coding assistant (Claude, Copilot, Cursor, etc.), make sure it loads
> `AGENTS.md` before starting any work.**

`AGENTS.md` at the repository root is the authoritative source for this project's
architecture, layering rules, cross-device UI rules, and coding standards. It covers the
data flow between modules, forbidden patterns, and the required workflow for adding new
commands. File-specific detail (USB HID packet format, meter, presets) lives in
`.claude/rules/`. An AI working without these will produce code that conflicts with
established patterns and is likely to introduce protocol bugs.

How the common tools pick it up:

- **Claude Code** — automatic, via `CLAUDE.md` (which imports `AGENTS.md`) and `.claude/rules/`
- **Codex, Cursor, Windsurf, GitHub Copilot** — read `AGENTS.md` natively in current versions
- **Claude Projects or any chat-based tool** — add `AGENTS.md` (and the relevant
  `.claude/rules/` file) as project knowledge, or paste it at the start of your session

The key rules AI assistants must follow are called out explicitly in `AGENTS.md`: confirm a
plan before implementing non-trivial features, follow the new-command checklist, keep UI
consistent across models, and run the quality gate before considering any work done.

---

## Project Structure

```
src/
├── main.rs       — Entry point, event loop, CLI args
├── app.rs        — Application state, focus/tab navigation, DeviceAction events
├── device.rs     — hidapi wrapper; open/send/receive for Shure devices
├── meter.rs      — cpal audio capture; real-time dBFS metering, RollingWindow, PeakWindow
├── presets.rs    — Host-side preset storage: TOML serialisation, load/save/delete, PresetSlot
├── protocol.rs   — USB HID packet encoding, CRC-16/ANSI, command constructors, apply_response()
├── ui.rs         — ratatui TUI rendering (all 5 tabs + help overlay)
├── fake_hid.rs   — fake device for tests and --qa (never shipped)
└── harness.rs    — headless --qa driver (never shipped)
tests/
├── e2e.rs        — runs tests/scenarios/*.jsonl against every model; PTY smoke test
└── scenarios/    — end-to-end scenarios, one JSON-lines file each
```

### End-to-end scenarios

`cargo test --features qa --test e2e` runs each file in `tests/scenarios/` through
`shurectl --qa` on every model its header lists, with a fake device and in demo mode, and
starts the real TUI once under a pseudo-terminal. To write one, drive the app by hand:

```bash
cargo build --features qa
printf '{"key":"Down"}\n{"key":"Enter"}\n{"query":"screen"}\n' \
  | ./target/debug/shurectl --qa --fake-device mv6
```

`.claude/skills/qa/SKILL.md` lists every line the harness understands.

All USB HID command byte values, feature addresses, and packet structure details are documented inline in `src/protocol.rs`.

---

## HID Feature Address Probe

`src/bin/probe.rs` is a developer tool used for protocol reverse-engineering. It systematically sweeps HID feature addresses across all pages and logs every valid device response, which is how undocumented features are discovered on new or updated hardware.

The probe is **read-only** — it only sends GET packets, never SET or CONFIRM. It is safe to run against a live device; no settings will be changed.

The probe binary is named `shurectl-probe` and is gated behind the `probe` cargo feature, so it is **not built by default**. This keeps it out of `cargo install` and Homebrew installs — end users have no reason to have an HID address sweeper in their `PATH`.

```bash
cargo run --bin shurectl-probe --features probe                          # scan the connected device
cargo run --bin shurectl-probe --features probe -- --pid 0x1013          # MVX2U Gen 1
cargo run --bin shurectl-probe --features probe -- --pid 0x1026          # MV6
cargo run --bin shurectl-probe --features probe -- --pid 0x1035          # MV6 Gen 2
cargo run --bin shurectl-probe --features probe -- --also-mix-class      # also sweep mix-class prefix
cargo run --bin shurectl-probe --features probe -- --also-lock-class     # also sweep lock-class
cargo run --bin shurectl-probe --features probe -- --page 0x03           # sweep a specific page only
cargo run --bin shurectl-probe --features probe -- --output results.txt  # write results to file
cargo run --bin shurectl-probe --features probe -- --delay-ms 50         # increase if device misses responses
```

See the module-level doc comment in `src/bin/probe.rs` for full details on packet classes and known limitations.

---

## Adding Support for a New Device

1. Capture USB traffic with usbmon/Wireshark against the official ShurePlus MOTIV Desktop app
2. Run the probe tool against the device to enumerate responding feature addresses
3. Cross-reference captures with probe output to confirm address→feature mappings
4. Add `FEAT_*` constants, command constructors, and `apply_response()` branches in `protocol.rs`
5. Add typed `get_*`/`set_*` methods in `device.rs` and wire up `DeviceAction` variants in `app.rs`
6. Update `KNOWN_FEATURES` in `src/bin/probe.rs` with any newly confirmed addresses

When protocol behaviour is uncertain, capture first — don't guess.

---

## Commit Style

This project follows [Conventional Commits](https://www.conventionalcommits.org/). Commit messages should explain the *why*, not just the *what*.

```
feat: add gain lock support for MVX2U Gen 2
fix: correct feature address for MV6 monitor mix SET packet
docs: document HDR_CONSTANT quirk for MV6 lock commands
```
