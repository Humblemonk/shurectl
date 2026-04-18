# shurectl — Project Instructions

The goal of this project is to build and maintain **shurectl**, an open-source terminal UI
configurator for Shure USB audio interfaces (currently the **MVX2U** and **MV6**) on Linux
and macOS. It replaces the Windows/Mac-only ShurePlus MOTIV Desktop app by communicating
with devices directly over USB HID.

This is a Rust project. Operate as a senior Rust developer: write clean, readable, maintainable
code. Avoid clever abstractions. The simple, obvious solution is almost always correct.

---

## 🚨 AUTOMATED CHECKS ARE MANDATORY

ALL check failures are BLOCKING — everything must be ✅ GREEN before moving on.
No errors. No formatting issues. No linting problems. Zero tolerance.

**Reality checkpoint command:**
```
cargo clippy -- -D warnings && cargo fmt --check && cargo test
```

Run this after every complete feature, before starting a new component, and whenever
something feels wrong.

---

## CRITICAL WORKFLOW — ALWAYS FOLLOW THIS

**Research → Plan → Implement**

Never jump straight to coding. Always follow this sequence:

1. **Research**: Explore the codebase, understand existing patterns
2. **Plan**: Create a detailed implementation plan and confirm it with me
3. **Implement**: Execute with validation checkpoints

When asked to implement any feature, first say:
> "Let me research the codebase and create a plan before implementing."

For complex or architectural decisions, say:
> "Let me ultrathink about this before proposing a solution."

---

## Project Structure

This is a single-crate Rust binary. All source lives under `src/`:

```
src/
  main.rs       # Entry point, CLI args (--demo, --list), event loop, key handling
  app.rs        # Application state: Tab, Focus, DeviceState, DeviceAction events
  device.rs     # hidapi wrapper: open MVX2U, send/receive HID reports
  meter.rs      # cpal audio capture: real-time dBFS metering, RollingWindow, PeakWindow
  presets.rs    # Host-side preset storage: TOML serialisation, load/save/delete, PresetSlot
  protocol.rs   # Packet encoding, CRC-16/ANSI, all command constructors, apply_response()
  ui.rs         # ratatui rendering: all 5 tabs + help overlay
```

**Data flow:** key event → `handle_key()` → `DeviceAction` → `apply_action()` → `device.rs` → HID packet → `protocol.rs`

**Meter data flow:** cpal audio callback → `meter_level` (AtomicI32) + `peak_window` (Mutex<PeakWindow>) → `ui.rs` reads on each render tick

**Tab structure:** Main | EQ | Dynamics | Presets | Info

---

## shurectl Domain Rules

These are project-specific patterns learned from working in this codebase.

### USB HID Protocol

The MVX2U uses plain USB HID Output/Input Reports for all configuration. Every packet is
exactly 64 bytes, sent via `hid_write()` and received via `hid_read()`:

```
[0x01] [0x11] [0x22] [seq] [0x03] [0x08] [data_len] [0x70] [data_len] [cmd0][cmd1][cmd2] [feat_addr...] [value...] [crc_hi] [crc_lo] [0x00 padding...]
  ↑─── Report ID ───↑                                                                      ↑──────────── CRC-16/ANSI covers from 0x11 onward ───────────────↑
```

- **USB IDs**: VID `0x14ED`, PID `0x1013`
- **Header magic**: `0x11 0x22` — never changes
- **Report ID**: `0x01` — required as byte 0 by hidapi's `hid_write()`; our buffers are 65 bytes total (1 report ID + 64 payload)
- **CRC**: CRC-16/ANSI — poly `0x8005`, init `0x0000`, reflected input and output (NOT CCITT-FALSE)
- **Transport**: plain HID Output Reports (`hid_write`) for commands; Input Reports (`hid_read`) for responses — `HIDIOCSFEATURE`/`HIDIOCGFEATURE` are NOT used
- **Interface**: accessed via `/dev/hidrawN`, not the USB audio class interface
- **SET + CONFIRM**: every SET command must be immediately followed by a CONFIRM packet (`CMD_CONFIRM`); the device will not apply the change without it

All command byte values and feature address constants live in `protocol.rs`.
Do not hardcode raw byte values outside of `protocol.rs`.

### Adding a New Command

New device commands always follow this sequence — do not skip steps:

1. `protocol.rs` — add `FEAT_*` address constant, `cmd_get_*` and `cmd_set_*` constructor functions, and a matching branch in `apply_response()` to decode the GET response into `DeviceState`
2. `device.rs` — add typed `get_*` / `set_*` methods on `Mvx2u` that call the constructors; add `cmd_get_*` to the `getters` slice in `get_state()` if it's part of full state readback
3. `app.rs` — add a `DeviceAction` variant if user-triggerable; update `adjust_focused()` or `toggle_focused()` as appropriate
4. `main.rs` — handle the new variant in `apply_action()`
5. `ui.rs` — add UI element if needed
6. `protocol.rs` tests — add a roundtrip test for the new packet

### Protocol Verification

If a command doesn't behave as expected on the real device, **capture packets first**:

```bash
sudo modprobe usbmon
lsusb | grep -i shure          # find bus number
sudo wireshark -i usbmonN      # replace N with bus number
# Filter: usb.transfer_type == 0x01
```

Compare the captured bytes to what the `cmd_*` constructors in `protocol.rs` produce.
Feature address constants (`FEAT_*`) and value encoding are the most likely source of
firmware-version differences. Fix these in `protocol.rs` only — never in `device.rs` or
higher layers.

### State Readback

State is fetched by issuing individual `cmd_get_*` packets for each feature, not by a single
monolithic GET_STATE command. `device.rs::get_state()` calls each getter in sequence and
feeds every response into `apply_response()`, which dispatches on the 2-byte feature address
returned by the device and writes the decoded value into the appropriate field of `DeviceState`.

Feature address → `DeviceState` field mapping is documented inline in `protocol.rs`.
If live device state looks wrong after `get_state()`, check the feature address constants
(`FEAT_*`) and the value-decoding branches in `apply_response()`.

### TUI / Focus Model

- `Tab` enum controls which panel is visible
- `Focus` enum controls which control within a tab is active
- `App::adjust_focused()` handles `←→` for sliders/numeric controls
- `App::toggle_focused()` handles `Enter/Space` for booleans and cycling enums
- Both return `Option<DeviceAction>` — `None` means UI-only change, no HID write needed
- `apply_action()` in `main.rs` is the only place that writes to the device

Never call `device.rs` methods from `ui.rs` or `app.rs`. All device writes go through
`apply_action()`.

### Meter Architecture

`meter.rs` runs a cpal audio capture stream on a background thread. It publishes two values
into `App`, shared via `Arc`:

- `meter_level: Arc<AtomicI32>` — instantaneous peak dBFS × 10, lock-free
- `peak_window: Arc<Mutex<PeakWindow>>` — two `RollingWindow`s: `short` (0.3 s) drives the
  bar height; `long` (3.0 s) drives the peak-hold marker

`start_meter()` returns a `MeterStatus` enum. The caller must keep the `Stream` inside
`MeterStatus::Running` alive — dropping it stops capture. The meter is not started in demo mode.

### Preset Storage

`presets.rs` implements host-side preset management. Presets are stored as TOML files in
`~/.config/shurectl/presets/`, with 4 fixed slots named `preset_1.toml`–`preset_4.toml`.

Key design decisions:
- **Mirror types**: `presets.rs` defines separate `Ser*` enums (`SerInputMode`, `SerMicPosition`,
  etc.) with `#[derive(Serialize, Deserialize)]`. Protocol types in `protocol.rs` stay free of
  serde concerns. Stable on-disk format is decoupled from internal enum evolution.
- **`PresetSlot`**: captures all configurable DSP settings from `DeviceState` — every field
  that can be sent to the MVX2U over HID. Hardware-identity fields (`serial_number`,
  `firmware_version`) are intentionally excluded.
- **`PresetSlot::from_device_state()`** — snapshot live state into a slot.
- **`PresetSlot::apply_to_device_state()`** — restore a slot onto `DeviceState`, preserving identity fields.
- **`load_all_presets()`** — loads all 4 slots at startup; missing files produce `None` entries.
- **`dirs_next::config_dir()`** — resolves `~/.config/` on Linux; falls back to `$HOME/.config/`
  if `dirs-next` returns `None`.

`DeviceAction` preset variants (handled in `main.rs::apply_action()`):
- `SavePreset(usize)` — snapshot current state, write TOML, update `app.presets[i]`
- `LoadPreset(usize)` — call `apply_to_device_state()`, send all SET commands to device
- `DeletePreset(usize)` — remove the TOML file, set `app.presets[i] = None`
- `PersistPresetName(usize)` — write the already-in-memory-updated name back to disk

Preset name editing is handled in `main.rs::handle_key()`, not in `toggle_focused()`.
When `app.editing_preset_name` is `true`, character keys append to the name and `Enter`
commits (fires `PersistPresetName`), while `Esc` cancels without saving.

### Demo Mode

`--demo` runs with `device: None`. `send_if_connected()` silently succeeds when
`device` is `None`. All app state changes still apply — only HID writes are skipped.
This is intentional: demo mode should always be fully navigable.

### Key Crates in Use

- `ratatui 0.30` — TUI rendering; use `Frame::render_widget()`, not direct buffer writes
- `crossterm 0.29` — terminal backend and key events; `KeyEventKind::Press` only
- `hidapi 2.4` (linux-native feature) — HID device open/read/write via `/dev/hidrawN`
- `cpal 0.17` — audio capture for the input level meter; default input device only
- `libc 0.2` — stderr suppression during cpal ALSA/JACK probing (`dup`/`dup2`)
- `anyhow` — all fallible functions return `anyhow::Result`
- `clap 4.5` — CLI argument parsing; `derive` feature only
- `serde 1` (with `derive` feature) — serialisation traits for preset TOML files
- `toml 0.8` — TOML serialisation/deserialisation for preset files
- `dirs-next 2.0` — platform config directory resolution (`~/.config/` on Linux)
- `tempfile 3` (dev-dependency) — hermetic temp directories in `presets.rs` tests

---

## Rust-Specific Rules

### FORBIDDEN — never do these:
- No `unwrap()` or `expect()` in production paths — use `?` and `anyhow::Result`
- No `panic!()` outside of tests
- No `println!()` — errors go to `eprintln!()` only at startup; TUI status bar otherwise
- No unnecessary `clone()` — prefer borrowing; justify every `.clone()`
- No `todo!()` or `unimplemented!()` in final code
- No versioned function names (`send_v2`, `build_packet_new`)
- No keeping old and new code side-by-side — delete replaced code
- No raw byte literals outside `protocol.rs` — use the named constants

### Required standards:
- Return `anyhow::Result<T>` from all fallible functions
- Use `?` for early error propagation — avoid deep nesting
- Match arms must be exhaustive; avoid wildcard `_` that silently swallows variants
- Meaningful names: `gain_db` not `g`, `band_index` not `i`
- Table-driven tests using `vec![(input, expected)]`
- All protocol constants documented with inline comments explaining their purpose

---

## Testing Strategy

| Situation | Approach |
|---|---|
| New protocol command | Write roundtrip test in `protocol.rs` `#[cfg(test)]` first |
| Packet encoding changes | Test CRC correctness and packet length invariants |
| State decode changes | Test `apply_response()` with hand-crafted response buffers |
| Focus/navigation changes | Manual test in `--demo` mode |
| `main()` and CLI args | Skip tests |

Performance is not a concern for this application — it is input-event-driven at ~100ms
tick rate. Do not add benchmarks unless a specific bottleneck is identified.

---

## Implementation Checklist

A feature is complete when:
- [ ] `cargo clippy -- -D warnings` passes clean
- [ ] `cargo fmt --check` passes
- [ ] `cargo test` passes (all suites)
- [ ] New commands are documented in `README.md` (protocol table and keyboard shortcuts)
- [ ] No dead code or unused imports remain
- [ ] Old code that was replaced has been deleted
- [ ] Tested in `--demo` mode for UI-only changes
- [ ] Tested against real device if HID protocol was touched

---

## 🚨 Hook Failures Are BLOCKING

When any check fails, you MUST:
1. **Stop immediately** — do not continue with other work
2. **Fix all issues** — every ❌ until everything is ✅ GREEN
3. **Verify the fix** — re-run the failed command to confirm
4. **Resume original task** — return to where you were

There are no warnings, only requirements.

---

## Problem-Solving Together

When stuck or in doubt:

- **Stop** — don't spiral into complex solutions
- **Step back** — re-read the requirements
- **Simplify** — the simple solution is usually correct
- **Ask** — "I see two approaches: [A] vs [B]. Which do you prefer?"

My guidance on better approaches is always welcome — please ask for it.

When protocol behaviour is uncertain, always say:
> "I'm not sure about this byte offset / command value. We should verify with usbmon before assuming."

---

## Communication Protocol

Progress updates:
```
✓ Added FEAT_AUTO_GAIN constant and apply_response() branch, roundtrip test passes
✓ EQ tab focus navigation complete, demo mode verified
✗ GET feature address for monitor_mix looks wrong on firmware 1.2 — investigating
```

Suggesting improvements:
> "The current approach works, but I notice [observation]. Would you like me to [specific improvement]?"

---

## Security

- The MVX2U HID interface is local hardware — no network exposure
- Validate all packet arguments before encoding (clamp ranges, not panic)
- No secrets in source — if a config file is added, use environment variables or `~/.config/`
- Never write firmware update packets — the firmware update byte sequences are intentionally
  omitted from this codebase (see readme legal section)

---

## Working Memory

When the conversation gets long:
- Re-read this file
- Summarize progress in `PROGRESS.md`
- Keep `TODO.md` current:

```markdown
## Current Task
- [ ] What we're doing RIGHT NOW

## Completed
- [x] What's done and tested

## Next Steps
- [ ] What comes next
```

---

> **Reminder**: If this file hasn't been referenced in 30+ minutes, re-read it.
> This is always a feature branch — no backwards compatibility needed.
> Clarity over cleverness, always.
> When in doubt about protocol bytes, capture first — don't guess.
