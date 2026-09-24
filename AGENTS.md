# shurectl

Terminal UI configurator for Shure USB audio interfaces (MVX2U Gen 1/Gen 2, MV6, MV7+) on
Linux and macOS. Talks to the device directly over USB HID, replacing the Windows/Mac-only
ShurePlus MOTIV Desktop app. Single-crate Rust binary. Prefer the simple, obvious solution
over clever abstractions.

File-specific detail lives in `.claude/rules/`. Read the matching file before editing:
`protocol.md` for `protocol.rs`/`device.rs`/`probe.rs` (HID packet format, usbmon
debugging), `meter.md` for `meter.rs`/`main.rs`, `presets.md` for `presets.rs`. Claude Code
loads these automatically.

## Commands

```
cargo run -- --demo mv7plus     # no hardware; also mvx2u, mvx2u-gen2, mv6
cargo run -- --list             # list connected devices
```

**Verification gate** — run before calling any change complete:

```
cargo clippy --features probe -- -D warnings && cargo build --all-targets --features probe \
  && cargo fmt --check && cargo test
```

- `--features probe` is required; otherwise clippy skips `shurectl-probe`.
- `cargo build --all-targets` catches dead imports in `#[cfg(test)]` code that clippy never
  compiles. Read the `cargo test` output for warnings; don't just grep for `test result`.
- Clippy isn't run with `--all-targets` yet because of pre-existing
  `field_reassign_with_default` warnings in `app.rs` tests. Fold them into struct literals before tightening.
- CI runs jscpd (budget 7% in `.github/linters/.jscpd.json`, ~5.5% actual). Check with
  `npx jscpd -c .github/linters/.jscpd.json .`. Fix duplication by extending a shared helper;
  don't raise the threshold.

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
  bin/probe.rs  # Maintainer-only HID address sweeper, built only under `--features probe`
```

**Control flow:** key event → `handle_key()` → `DeviceAction` → `apply_action()` (main.rs) →
`device.rs` → `protocol.rs` packet.

**Layering rules (strict):**
- `apply_action()` in `main.rs` is the *only* place that writes to the device. Never call
  `device.rs` from `ui.rs` or `app.rs`.
- Raw protocol byte values live *only* in `protocol.rs` as named constants.
- `shurectl-probe` must never ship to end users via `cargo install` or Homebrew.
- Never write firmware-update packets. Those byte sequences are intentionally omitted (see
  the README legal section).

**Demo mode:** `--demo` runs with `device: None`, and `send_if_connected()` silently succeeds.
State changes still apply; only HID writes are skipped. Demo mode must stay fully navigable.

## Adding a New Command

Follow this sequence without skipping steps:

1. `protocol.rs`: `FEAT_*` constant, `cmd_get_*`/`cmd_set_*` constructors, and an
   `apply_response()` branch decoding into `DeviceState`
2. `device.rs`: typed `get_*`/`set_*` methods on `ShureDevice`. If it's part of full
   readback, add the getter to the `getters` slice in the `get_state_*()` function of every
   model that supports it (`get_state()` dispatches per model)
3. `app.rs`: `DeviceAction` variant if user-triggerable; wire it into `adjust_focused()` or
   `toggle_focused()`
4. `main.rs`: handle the variant in `apply_action()`
5. `ui.rs`: UI element if needed. Follow **Cross-Device UI Consistency** below
6. `presets.rs`: if it's a DSP setting, add it to `PresetSlot`, `from_device_state()`, and
   `apply_to_device_state()` so presets capture it
7. `protocol.rs`: roundtrip test for the new packet
8. `README.md`: update the protocol table and keyboard shortcuts, noting which models support
   the command if it isn't universal

## TUI / Focus Model

- `Tab` selects the visible panel; `Focus` selects the active control within it
- `adjust_focused()` handles ←/→ for sliders; `toggle_focused()` handles Enter/Space for
  booleans and enum cycling
- Both return `Option<DeviceAction>`. `None` means a UI-only change with no HID write
- Preset name editing lives in `main.rs::handle_key()`, not `toggle_focused()`. While
  `editing_preset_name` is true, chars append, Enter commits (`PersistPresetName`), and Esc
  cancels

## Cross-Device UI Consistency

A user owns one device but reads one README, one help overlay, and one set of screenshots.
If "Gain Lock" on the Gen 2 is "Lock" on the MV6, the docs stop matching reality. Divergence
is also the largest maintenance cost here: `ui.rs` has eight `draw_main_left_*` variants and
four-way `DeviceModel` matches in `draw_main_right()` and `draw_info_tab()`, and `app.rs`
matches on `DeviceModel` in `reset_focus_for_tab()`, `focus_next()`, and `focus_prev()`.

**The rule:** any control that exists on more than one model has identical label, units,
value formatting, keybinding, and position relative to its neighbours on all of them. Only
genuinely device-exclusive hardware features may differ.

Shared across all models unless the hardware makes it impossible:

- **Tab set and order**: `Tab::ALL` is the source of truth. Models filter it, never reorder it
- **Keybindings**: Tab/Shift-Tab cycles tabs, ↑/↓ moves focus, ←/→ adjusts, Enter/Space
  toggles, `?` help. No model-specific keys
- **Focus traversal**: mode → mute → gain → monitor mix → device extras. Models skip entries
  they lack; they don't reshuffle
- **Labels, units, formatting**: "Gain", "Monitor Mix", "Denoiser" are spelled the same
  everywhere. dB values, percentages, and enum names render through the same code path
- **Drawing helpers**: `draw_mode_block()`, `draw_mute_block()`, `draw_monitor_mix_gauge()`,
  `draw_gain_lock_block()`, `draw_phantom_block()`, `segmented_span()`, `draw_main_shared()`.
  Extend a helper with a parameter rather than forking a near-copy
- **Status and error wording**: same phrasing for the same condition on every model

**Hiding vs. locking** (the convention from `draw_tabs()`, applied to controls too):

- Permanently unsupported on this hardware → hide it (Reverb/LED on non-MV7+)
- Supported but currently unavailable → show it with 🔒 and a notice (EQ/Dynamics on Gen 1
  in Auto mode, via `draw_tab_locked_notice()`)
- Never leave a control visible and focusable but inert. It reads as a bug

**Before adding anything model-specific, in order:**

1. Do other models have this capability under a different vendor name? Use the existing
   shurectl name and control. Vendor naming stops at `protocol.rs`.
2. Can an existing shared helper render it? Extend the helper.
3. If a per-model `draw_*` fork is genuinely required, keep block order, borders, and spacing
   identical to its siblings.
4. Update *every* `DeviceModel` match: focus fns, both `draw_main_*`, `draw_info_tab()`, and
   preset serialization. Wherever `_` was used, a missing arm is a silent UX divergence
   rather than a compile error.
5. Verify with `--demo mvx2u`, `--demo mvx2u-gen2`, `--demo mv6`, and `--demo mv7plus`.

**Antipatterns:**

- Copying `draw_main_left_gen2_manual()` to bootstrap a new model and tweaking strings
- A keybinding only one model responds to
- Reordering tabs or focus for one model because it "reads better" there
- Hardcoding a model name in a shared helper instead of passing behaviour in
- Documenting a shortcut in `README.md` that only works on some devices without saying so

## Rust Rules

- No `unwrap()`/`expect()` in production paths; no `panic!()` outside tests; no
  `todo!()`/`unimplemented!()` in final code
- No `println!()`. Use `eprintln!()` only at startup, and the TUI status bar after that
- `anyhow::Result<T>` for all fallible functions
- Prefer borrowing; justify every `.clone()`
- Exhaustive match arms; no wildcard `_` that silently swallows variants
- Meaningful names (`gain_db` not `g`); delete replaced code, no versioned function names
- Validate packet arguments before encoding (clamp, don't panic)
- `ratatui`: use `Frame::render_widget()`, not direct buffer writes. `crossterm`: handle
  `KeyEventKind::Press` only

## Testing

| Situation | Approach |
| --- | --- |
| New protocol command | Roundtrip test in `protocol.rs` first |
| Packet encoding changes | Test CRC correctness and 64-byte length invariant |
| State decode changes | Test `apply_response()` with hand-crafted response buffers |
| Focus/navigation changes | Manual test in `--demo` for all four models |
| `main()` / CLI args | No tests |

Performance is not a concern (~100 ms input-driven tick). No benchmarks unless a specific
bottleneck has been identified.

## Workflow

- For non-trivial features, explore the relevant code and confirm a plan before implementing.
- If a byte offset or command value is uncertain, say so and propose a usbmon capture rather
  than guessing.
- When a cross-model UI divergence is unavoidable, name the affected models in the commit/PR
  body.
