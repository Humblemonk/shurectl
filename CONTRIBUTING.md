# Contributing to shurectl

## Development Setup

### Prerequisites

- Rust (edition 2024)
- Linux: `libasound2-dev` and `libudev-dev`
- A supported Shure device for hardware testing, or use `--demo` for UI-only work

### Building

```bash
git clone https://github.com/Humblemonk/shurectl.git
cd shurectl
cargo build
```

### Quality Gate

All of the following must pass clean before any commit:

```bash
cargo clippy -- -D warnings && cargo fmt --check && cargo test
```

There are no warnings — only requirements.

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
└── ui.rs         — ratatui TUI rendering (all 5 tabs + help overlay)
```

All USB HID command byte values, feature addresses, and packet structure details are documented inline in `src/protocol.rs`.

---

## HID Feature Address Probe

`src/bin/probe.rs` is a developer tool used for protocol reverse-engineering. It systematically sweeps HID feature addresses across all pages and logs every valid device response, which is how undocumented features are discovered on new or updated hardware.

The probe is **read-only** — it only sends GET packets, never SET or CONFIRM. It is safe to run against a live device; no settings will be changed.

```bash
cargo run --bin probe                                   # scan MVX2U Gen 2 (default)
cargo run --bin probe -- --pid 0x1013                  # MVX2U Gen 1
cargo run --bin probe -- --pid 0x1019                  # MV7+
cargo run --bin probe -- --pid 0x1026                  # MV6
cargo run --bin probe -- --also-mix-class              # also sweep mix-class prefix
cargo run --bin probe -- --also-lock-class             # also sweep lock-class
cargo run --bin probe -- --output results.txt          # write results to file
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
