# Contributing to shurectl

## Setup

- Rust via [rustup](https://rustup.rs): `rust-toolchain.toml` selects stable with clippy and rustfmt
- Linux: `libasound2-dev` and `libudev-dev`
- A supported Shure device for hardware testing, or `--demo` for UI-only work

```bash
git clone https://github.com/Humblemonk/shurectl.git
cd shurectl
cargo run -- --demo mv7plus
```

## Quality gate

Run this before every commit. CI runs the same script on Linux, macOS and Windows:

```bash
scripts/check.sh
```

It runs clippy, the build, `cargo fmt --check`, the unit tests and the end-to-end scenarios,
and fails on any warning. It builds with `--features probe,qa`, so clippy also covers
`shurectl-probe` and the headless `--qa` harness; both are for maintainers and CI and never
ship.

## Project rules

`AGENTS.md` is the authoritative guide to the architecture, layering rules, cross-device UI
rules, coding standards and the checklist for adding a command. File-specific detail (HID
packet format, meter, presets, UI palette) lives in `.claude/rules/`. Read them before a
non-trivial change.

If you use an AI coding assistant, make sure it loads them too:

- **Claude Code**: automatic, via `CLAUDE.md` (which imports `AGENTS.md`) and `.claude/rules/`
- **Codex, Cursor, Windsurf, GitHub Copilot**: read `AGENTS.md` natively
- **Chat-based tools**: add `AGENTS.md` and the relevant `.claude/rules/` file as project
  knowledge, or paste them at the start of the session

## End-to-end scenarios

`tests/scenarios/` holds one JSON-lines file per scenario; its `README.md` lists them and what
they check. To write one, drive the app by hand:

```bash
cargo build --features qa
printf '{"key":"Down"}\n{"key":"Enter"}\n{"query":"screen"}\n' \
  | ./target/debug/shurectl --qa --fake-device mv6
```

`.claude/skills/qa/SKILL.md` lists every line the harness understands.

## HID feature address probe

`shurectl-probe` (`src/bin/probe.rs`) sweeps HID feature addresses and logs every reply, which
is how undocumented features are found on new hardware. It is **read-only**: it sends GET
packets only, never SET or CONFIRM, so it is safe against a live device. It is gated behind the
`probe` feature to keep it out of `cargo install` and Homebrew.

```bash
cargo run --bin shurectl-probe --features probe                  # the one connected device
cargo run --bin shurectl-probe --features probe -- --pid 0x1035  # pick a model by PID
cargo run --bin shurectl-probe --features probe -- --help        # pages, prefix classes, delay
```

The module doc comment in `src/bin/probe.rs` explains the packet classes and known limits.

## Adding support for a new device

1. Capture USB traffic with usbmon/Wireshark against ShurePlus MOTIV
2. Run the probe against the device to enumerate responding feature addresses
3. Cross-reference captures with probe output to confirm address → feature mappings
4. Implement each command as in **Adding a New Command** in `AGENTS.md`
5. Add newly confirmed addresses to `KNOWN_FEATURES` in `src/bin/probe.rs`

When protocol behaviour is uncertain, capture first. Don't guess.

## Commits

Write the subject as a plain imperative sentence saying what changes for the user, with no
type prefix. Use the body for the *why*, naming the models affected and any commit it fixes.

```
Send EQ band switches only to the Gen 1, and keep unsaved Playback Level
Undo only writes that clearly failed, and load only saved preset settings
Release 2.12.0: safer preset loads, 48V kept, review severity
```
