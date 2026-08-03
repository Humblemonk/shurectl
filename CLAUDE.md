# shurectl

Terminal UI configurator for Shure USB audio interfaces (MVX2U, MV6) on Linux and macOS.
Replaces the Windows/Mac-only ShurePlus MOTIV Desktop app by talking to the device directly
over USB HID. Single-crate Rust binary. Prefer the simple, obvious solution over clever
abstractions.

## Verification

Before considering any change complete:

```
cargo clippy --features probe -- -D warnings && cargo fmt --check && cargo test
```

`--features probe` is required — `shurectl-probe` is feature-gated and clippy skips it otherwise.

For non-trivial features, explore the relevant code and confirm a plan before implementing.

## Architecture

```
src/
  main.rs       # Entry point, CLI args (--demo, --list, --mute), event loop, apply_action()
  app.rs        # App state: Tab, Focus, DeviceState, DeviceAction events
  device.rs     # hidapi wrapper: open device, send/receive HID reports
  meter.rs      # cpal capture: dBFS metering, RollingWindow, PeakWindow
  presets.rs    # Host-side presets: TOML load/save/delete, PresetSlot
  protocol.rs   # Packet encoding, CRC-16/ANSI, command constructors, apply_response()
  ui.rs         # ratatui rendering: 5 tabs (Main | EQ | Dynamics | Presets | Info) + help overlay
  bin/probe.rs  # Maintainer-only HID address sweeper — builds only under `--features probe`
```

**Control flow:** key event → `handle_key()` → `DeviceAction` → `apply_action()` (main.rs) → `device.rs` → `protocol.rs` packet.

**Meter flow:** cpal callback → `meter_level` (AtomicI32) + `peak_window` (Mutex) → read by `ui.rs` each render tick.

**Layering rules (strict):**
- `apply_action()` in `main.rs` is the *only* place that writes to the device. Never call `device.rs` from `ui.rs` or `app.rs`.
- Raw protocol byte values live *only* in `protocol.rs` as named constants. Never hardcode bytes elsewhere.
- `src/bin/probe.rs` builds as `shurectl-probe` only under `--features probe`. It is a maintainer tool and must never ship to end users via `cargo install` or Homebrew.

## USB HID Protocol

Every packet is exactly 64 bytes (65 with hidapi's report-ID byte 0), sent via `hid_write()`
/ `hid_read()` on `/dev/hidrawN` — not the USB audio class interface, and not
`HIDIOCSFEATURE`/`HIDIOCGFEATURE`.

```
[0x01] [0x11] [0x22] [seq] [0x03] [0x08] [len] [0x70] [len] [cmd0..cmd2] [feat_addr..] [value..] [crc_hi] [crc_lo] [pad..]
  ^report ID    ^magic, never changes            CRC-16/ANSI covers 0x11 onward
```

- **USB IDs:** VID `0x14ED`, PID `0x1013`
- **CRC:** CRC-16/ANSI — poly `0x8005`, init `0x0000`, reflected in/out (NOT CCITT-FALSE)
- **SET + CONFIRM:** every SET must be immediately followed by a `CMD_CONFIRM` packet or the device won't apply the change
- **State readback:** no monolithic GET_STATE. `device.rs::get_state()` issues individual `cmd_get_*` packets; `apply_response()` dispatches on the 2-byte feature address and writes into `DeviceState`. Feature address → field mapping is documented inline in `protocol.rs`.

**When a command misbehaves on real hardware, capture packets before guessing:**

```
sudo modprobe usbmon
lsusb | grep -i shure            # find bus number
sudo wireshark -i usbmonN        # filter: usb.transfer_type == 0x01
```

Compare captures against `cmd_*` constructor output. Firmware-version differences almost
always show up as `FEAT_*` addresses or value encoding — fix in `protocol.rs` only. If a
byte offset or command value is uncertain, say so and propose verifying with usbmon rather
than assuming.

## Adding a New Command

Follow this sequence, no skipped steps:

1. `protocol.rs` — `FEAT_*` constant, `cmd_get_*`/`cmd_set_*` constructors, `apply_response()` branch decoding into `DeviceState`
2. `device.rs` — typed `get_*`/`set_*` methods on `Mvx2u`; add getter to the `getters` slice in `get_state()` if part of full readback
3. `app.rs` — `DeviceAction` variant if user-triggerable; wire into `adjust_focused()` or `toggle_focused()`
4. `main.rs` — handle the variant in `apply_action()`
5. `ui.rs` — UI element if needed
6. `protocol.rs` — roundtrip test for the new packet
7. `README.md` — update protocol table and keyboard shortcuts

## TUI / Focus Model

- `Tab` selects the visible panel; `Focus` selects the active control within it
- `adjust_focused()` handles ←/→ for sliders; `toggle_focused()` handles Enter/Space for booleans and enum cycling
- Both return `Option<DeviceAction>` — `None` means UI-only change, no HID write
- Preset name editing lives in `main.rs::handle_key()` (not `toggle_focused()`): when `editing_preset_name` is true, chars append, Enter commits (`PersistPresetName`), Esc cancels

## Meter

`meter.rs` runs a cpal capture stream on a background thread, publishing via `Arc`:
- `meter_level: Arc<AtomicI32>` — instantaneous peak dBFS × 10, lock-free
- `peak_window: Arc<Mutex<PeakWindow>>` — `short` (0.3 s) window drives bar height; `long` (3.0 s) drives peak-hold marker

`start_meter()` returns `MeterStatus`; the caller must keep the `Stream` in
`MeterStatus::Running` alive — dropping it stops capture. The meter does not start in demo mode.

## Presets

TOML files in `~/.config/shurectl/presets/`, 4 fixed slots (`preset_1.toml`–`preset_4.toml`).

- **Mirror types:** `presets.rs` defines `Ser*` enums with serde derives so `protocol.rs` types stay serde-free — on-disk format is decoupled from internal enum evolution
- `PresetSlot` captures all DSP settings from `DeviceState`; identity fields (`serial_number`, `firmware_version`) are excluded and preserved on apply
- `load_all_presets()` runs at startup; missing files → `None`
- `DeviceAction` variants: `SavePreset`, `LoadPreset` (applies then sends all SETs), `DeletePreset`, `PersistPresetName`

## Demo Mode

`--demo` runs with `device: None`; `send_if_connected()` silently succeeds. All app state
changes still apply — only HID writes are skipped. Demo mode must always remain fully navigable.

## Crates

See `Cargo.toml` for versions. Usage notes that matter:

- `ratatui` — use `Frame::render_widget()`, not direct buffer writes
- `crossterm` — handle `KeyEventKind::Press` only
- `hidapi` — `linux-native` feature, `/dev/hidrawN` access
- `cpal` — default input device only
- `libc` — stderr suppression (`dup`/`dup2`) during cpal ALSA/JACK probing
- `anyhow` — all fallible functions return `anyhow::Result<T>`
- `tempfile` (dev) — hermetic temp dirs in `presets.rs` tests

## Rust Rules

- No `unwrap()`/`expect()` in production paths; no `panic!()` outside tests; no `todo!()`/`unimplemented!()` in final code
- No `println!()` — `eprintln!()` only at startup; use the TUI status bar otherwise
- Prefer borrowing; justify every `.clone()`
- Exhaustive match arms — avoid wildcard `_` that silently swallows variants
- Meaningful names (`gain_db` not `g`); delete replaced code, no versioned function names
- Validate packet arguments before encoding (clamp, don't panic)
- Never write firmware-update packets — those byte sequences are intentionally omitted (see readme legal section)

## Testing

| Situation | Approach |
| --- | --- |
| New protocol command | Roundtrip test in `protocol.rs` first |
| Packet encoding changes | Test CRC correctness and 64-byte length invariant |
| State decode changes | Test `apply_response()` with hand-crafted response buffers |
| Focus/navigation changes | Manual test in `--demo` mode |
| `main()` / CLI args | No tests |

Performance is not a concern (~100 ms input-driven tick rate) — no benchmarks unless a
specific bottleneck is identified.
