# shurectl

Terminal UI configurator for Shure USB audio interfaces (MVX2U Gen 1/Gen 2, MV6, MV6 Gen 2,
MV7, MV7+) on Linux and macOS. Talks to the device directly over USB HID, replacing the
Windows/Mac-only ShurePlus MOTIV Desktop app. Single-crate Rust binary. Prefer the simple,
obvious solution over clever abstractions.

File-specific detail lives in `.claude/rules/`. Read the matching file before editing:
`protocol.md` for `protocol.rs`/`device.rs`/`probe.rs` (HID packet format, usbmon
debugging), `meter.md` for `meter.rs`/`main.rs`, `presets.md` for `presets.rs`, `ui.md` for
`ui.rs` (MOTIV Mix palette and controls). Claude Code loads these only when a matching file
is read, so anything a reviewer must know from a diff alone belongs in **Review** below.

## Commands

```
cargo run -- --demo mv7plus     # no hardware; also mvx2u, mvx2u-gen2, mv6, mv6-gen2, mv7
cargo run -- --list             # list connected devices
```

**Verification gate** — run before calling any change complete. CI runs the same script:

```
scripts/check.sh
```

- It runs clippy, `cargo build --all-targets`, `cargo fmt --check`, `cargo test` and the
  end-to-end scenarios, and any warning fails it. Change the gate in the script, not in docs or CI.
- `--features probe,qa` is required; otherwise clippy skips `shurectl-probe` and the `--qa`
  harness. `qa` never ships (see **Testing**).
- `cargo build --all-targets` catches dead imports in `#[cfg(test)]` code that clippy never
  compiles.
- Clippy isn't run with `--all-targets` yet because of pre-existing
  `field_reassign_with_default` warnings in `app.rs` tests. Fold them into struct literals before tightening.
- CI runs jscpd (budget 5% in `.github/linters/.jscpd.json`, ~2.5% actual). Check with
  `npx jscpd -c .github/linters/.jscpd.json .`. Fix duplication by extending a shared helper;
  don't raise the threshold.

## Architecture

```
src/
  main.rs       # Entry point, CLI args (--demo, --list, --mute), event loop, apply_action()
  app.rs        # App state: Tab, Focus, DeviceState, DeviceAction events
  device.rs     # hidapi wrapper: open device, send/receive HID reports
  meter.rs      # cpal capture: dBFS metering, RollingWindow, PeakWindow
  mouse.rs      # HitMap from the last rendered frame; clicks/wheel → the same key actions
  presets.rs    # Host-side presets: TOML load/save/delete, PresetSlot
  protocol.rs   # Packet encoding, CRC-16/ANSI, command constructors, apply_response()
  ui.rs         # ratatui rendering: the tabs in Tab::ALL + help overlay
  ui_renders.golden  # Render hashes checked by ui.rs `renders_match_golden`
  bin/probe.rs  # Maintainer-only HID address sweeper, built only under `--features probe`
  fake_hid.rs   # Fake device behind device.rs's HidTransport (tests and `--features qa` only)
  harness.rs    # `--qa` JSON-lines driver over the real event loop step (tests and qa only)
tests/
  e2e.rs        # Runs tests/scenarios/*.jsonl through `shurectl --qa` per model; PTY smoke
  scenarios/    # One JSON-lines file per scenario
```

**Control flow:** key event → `step()` → `handle_key()` → `DeviceAction` → `apply_action()`
(main.rs) → `device.rs` → `HidTransport` → `protocol.rs` packet. `run_event_loop()` and the
`--qa` harness both call `step()`; only the harness swaps in a fake transport and clock.

**Layering rules (strict):**
- `apply_action()` in `main.rs` is the *only* place that writes to the device. Never call
  `device.rs` from `ui.rs`, `app.rs` or `mouse.rs`. Mouse input only maps to key actions.
- Raw protocol byte values live *only* in `protocol.rs` as named constants.
- `shurectl-probe`, `--qa` and `fake_hid.rs` must never ship to end users via `cargo install`
  or Homebrew. Keep the `qa` feature and `cfg(any(test, feature = "qa"))` gates intact.
- Never write firmware-update packets. Those byte sequences are intentionally omitted (see
  the readme Credits and legal section).

**Demo mode:** `--demo` runs with `device: None`, and `send_if_connected()` silently succeeds.
State changes still apply; only HID writes are skipped. Demo mode must stay fully navigable.

**Connection state:** `run_event_loop()` calls `poll_presence()` every 2 s. It checks the OS
device list only (`ShureDevice::is_present()`), with no HID traffic. A missing device shows
`[DISCONNECTED]` in the header; once it is back (after one more poll, to let it boot) it is
reconnected through `DeviceAction::Reconnect`, so `apply_action()` stays the only writer.
`r` refreshes and reconnects on demand. Only a `device::Disconnected` error (HID read/write
failure, or the device not found again) marks the device disconnected; a device that is
present but refuses or ignores a command stays connected.

## Adding a New Command

Follow this sequence without skipping steps:

1. `protocol.rs`: `FEAT_*` constant, `cmd_get_*`/`cmd_set_*` constructors, and an
   `apply_response()` branch decoding into `DeviceState`
2. `device.rs`: typed `get_*`/`set_*` methods on `ShureDevice`. If it's part of full
   readback, add the getter to the `getters` slice in the `get_state_*()` function of every
   model that supports it (`get_state()` dispatches per model)
   - 2b. `fake_hid.rs`: add the setting to `features()` so the fake stores and answers it.
     `get_state_reads_back_the_seeded_state_on_every_model` fails until you do
3. `app.rs`: `DeviceAction` variant if user-triggerable; wire it into `adjust_focused()` or
   `toggle_focused()`
4. `main.rs`: handle the variant in `apply_action()`
5. `ui.rs`: UI element if needed. Follow **Cross-Device UI Consistency** below
6. `presets.rs`: if it's a DSP setting, add it to `PresetSlot`, `from_device_state()`, and
   `apply_to_device_state()` so presets capture it
7. `protocol.rs`: roundtrip test for the new packet
8. `README.md`: add a row to the collapsed Device support table (with the models that have
   it) and update the key table if a shortcut changed. Keep the top of the readme for
   install and first run; don't add sections there

## TUI / Focus Model

- `Tab` selects the visible panel; `Focus` selects the active control within it
- `adjust_focused()` handles ←/→ for sliders; `toggle_focused()` handles Enter/Space for
  booleans and enum cycling
- Both return `Option<DeviceAction>`. `None` means a UI-only change with no HID write
- Preset name editing lives in `main.rs::handle_key()`, not `toggle_focused()`. While
  `editing_preset_name` is true, chars append (including `q`, which only quits outside
  editing), Enter commits (`PersistPresetName`), and Esc or clicking outside cancels

## Cross-Device UI Consistency

A user owns one device but reads one readme, one help overlay, and one set of screenshots.
If "Gain Lock" on the Gen 2 is "Lock" on the MV6, the docs stop matching reality. Divergence
is also the largest maintenance cost here: `ui.rs` has eleven `draw_main_left_*` variants and
a six-way `DeviceModel` match in `draw_info_tab()`, and `app.rs` matches on `DeviceModel` in
`reset_focus_for_tab()`, `focus_next()`, and `focus_prev()`. The Main tab's Summary is one
ordered `SummaryRow` list; a model picks its rows in `summary_rows()` but can't reorder them.

**The rule:** any control that exists on more than one model has identical label, units,
value formatting, keybinding, and position relative to its neighbours on all of them. Only
genuinely device-exclusive hardware features may differ.

Intended differences (hardware, not divergence; don't flag them):

- Gain: 0–60 dB on the MVX2U Gen 1 and Gen 2, 0–36 dB elsewhere; 1.5 dB steps on the MV7
- EQ band steps: 0.5 dB on the MVX2U Gen 2, 2 dB on the Gen 1 and MV6 Gen 2
- "Monitor Mix" (a balance) on the MVX2U Gen 1 and MV7; "Mic Level" on models with Playback Level
- "Target Level" on the MV6 Gen 2 and "Auto Gain" on the MVX2U Gen 1 are different settings
- EQ and Dynamics locked in Auto only on the MVX2U Gen 1 and MV7 (`is_tab_locked()`)
- Tabs and controls hidden per model, as the readme's Device support table lists

Shared across all models unless the hardware makes it impossible:

- **Tab set and order**: `Tab::ALL` is the source of truth. Models filter it, never reorder it
- **Keybindings**: Tab/Shift-Tab cycles tabs, ↑/↓ moves focus, ←/→ adjusts, Enter/Space
  toggles, `?` help. No model-specific keys
- **Focus traversal**: mode → mute → gain → monitor mix → device extras. Models skip entries
  they lack; they don't reshuffle
- **Labels, units, formatting**: "Gain", "Monitor Mix", "Denoiser" are spelled the same
  everywhere. dB values, percentages, and enum names render through the same code path
- **Drawing helpers**: `draw_mode_mute_row()` (`draw_mode_block()`, `draw_mute_block()`),
  `draw_gain_gauge()`, `draw_monitor_mix_gauge()`, `draw_gain_lock_block()`,
  `draw_config_lock_block()`, `draw_phantom_block()`, `draw_auto_controls()`,
  `segmented_span()`, `draw_main_shared()`; block styles `focus_border()`, `focus_block()`,
  `alert_block()`, `card_block()`; sliders `slider_gauge()`; Dynamics cards
  `draw_card_grid()`; focus hints `with_hint()`; EQ `eq_band_column()`; click targets
  `control_rows()`. Extend a helper with a parameter rather than forking a near-copy
- **Status and error wording**: same phrasing for the same condition on every model

**Hiding vs. locking** (the convention from `draw_tabs()`, applied to controls too):

- Permanently unsupported on this hardware → hide it (Reverb on non-MV7+, LED on models
  without LED settings, Factory Reset except on the MV7+, MV6 Gen 2 and MVX2U Gen 2)
- Supported but currently unavailable → show it with 🔒 and a notice (EQ/Dynamics on Gen 1
  in Auto mode, via `draw_tab_locked_notice()`)
- Never leave a control visible and focusable but inert. It reads as a bug

**Before adding anything model-specific, in order:**

1. Do other models have this capability under a different vendor name? Use the existing
   shurectl name and control. Vendor naming stops at `protocol.rs`.
2. Can an existing shared helper render it? Extend the helper.
3. If a per-model `draw_*` fork is genuinely required, keep block order, borders, and spacing
   identical to its siblings.
4. Update *every* `DeviceModel` match: focus fns, `draw_main_left()`, `summary_rows()`,
   `draw_info_tab()`, and preset serialization. Wherever `_` was used, a missing arm is a
   silent UX divergence rather than a compile error.
5. Verify with `--demo mvx2u`, `--demo mvx2u-gen2`, `--demo mv6`, `--demo mv6-gen2`, `--demo mv7`,
   and `--demo mv7plus`.

**Antipatterns:**

- Copying `draw_main_left_gen2_manual()` to bootstrap a new model and tweaking strings
- A keybinding only one model responds to
- Reordering tabs or focus for one model because it "reads better" there
- Hardcoding a model name in a shared helper instead of passing behaviour in
- Documenting a shortcut in `README.md` that only works on some devices without saying so

## Rust Rules

- No `unwrap()`/`expect()` in production paths; no `panic!()` outside tests; no
  `todo!()`/`unimplemented!()` in final code
- No `println!()` once the TUI runs: use `eprintln!()` only at startup, and the TUI status
  bar after that. The one-shot commands (`--list`, `--mute`, `--preset`) and
  `shurectl-probe` print their results to stdout, so scripts can read them.
  State readback collects optional settings it couldn't read in `Readback::skipped` for this
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
| New protocol command | One roundtrip test in `protocol.rs` first: the packet bytes (match a usbmon capture when there is one) and `apply_response()` decoding the reply. Length and CRC come from `build_packet()`, so don't re-test them per command |
| Packet encoding changes (`build_packet()`, `build_packet_hdr0()`, `crc16_ansi()`) | Test CRC correctness and the 64-byte length invariant there |
| State decode changes | Test `apply_response()` with hand-crafted response buffers |
| Focus/navigation changes | A scenario, or manual test in `--demo` for all six models |
| `ui.rs` rendering changes | `renders_match_golden` must pass unchanged for a refactor. For an intended change, rerun with `UPDATE_GOLDEN=1` and check the listed screens in `--demo`. `every_screen_fits_the_min_size` fails when a screen no longer fits 80×24 (`MIN_SIZE`), or a focus hint is cut mid-word instead of going through `with_hint()` |
| User-visible behaviour (keys, clicks, status, packet order, reconnects) | A scenario in `tests/scenarios/`, run by `cargo test --features qa --test e2e` on every model in its header, with a fake device and in demo mode. Record one with `shurectl --qa` (`.claude/skills/qa/SKILL.md`) |
| `main()` / CLI args | No tests |

Performance is not a concern (~100 ms input-driven tick). No benchmarks unless a specific
bottleneck has been identified.

## Review

The verification gate and CI already cover clippy, fmt, tests, the end-to-end scenarios and jscpd;
don't report what they catch. `renders_match_golden` proves a `ui.rs` change is invisible when its golden file
is unchanged; a changed golden file lists the screens to look at.

**Severity.** Label each finding and lead with P0 and P1. When there are none, say so
rather than promoting a lower one.

- **P0**: harms hardware, ears or privacy: 48V switched on without the user asking (ribbon
  mics), a gain or level jump from a write based on default or stale state (the 36 dB default
  gain, 996c882), the mic live while the UI shows Muted, a packet with the wrong encoding or
  address for the model (a800e26), any firmware-update packet
- **P1**: a setting written, read back or shown wrong on some model (48V switched off
  included), a preset load that stops or skips settings, a panic, a failure reported wrong
  either way, a control visible but inert (see Hiding vs. locking)
- **P2**: users notice, but nothing is written or shown wrong: misleading status, help or
  readme wording, a refusal without its reason, a cross-model UI divergence, noticeable
  slowness. Also a broken Rust rule or regression trap that makes a P0/P1 likely later
  (`_` over `DeviceModel`, a raw byte outside `protocol.rs`, `unwrap()` in production)
- **P3**: no effect on users yet: naming, comments, duplication within the jscpd budget,
  refactor preferences. List them in one line at the end, or leave them out

**Regression traps.** Check every diff that touches these:

- **Readback order**: changing a `getters` slice, `reports_after_confirm()`, or reply routing
  can shift every later GET by one packet (481dc76). Replies route by address. The prefix
  byte routes only FEAT_MIX 0x03 (Playback Mix), confirmed on the MV6 Gen 2 and MVX2U Gen 2.
  `get_state()` fails unless every GET got a usable reply, so no field is left at its default
  (36 dB gain). `query_optional()` is only for a GET unchecked on that model's hardware.
- **Gain Lock**: a locked device ignores gain writes. `apply_binary_preset()` unlocks,
  writes the gain, then re-locks straight away (835c213, 551a5ae). Don't reorder it.
- **New `PresetSlot` field**: files saved before it existed, or on a model without it, must
  not zero the setting on load (73476a4). A per-model setting is an `Option`, saved only in
  `from_device_state()`'s `match` for models that have it. Check `#[serde(default)]` and
  `apply_to_device_state()`.
- **Failed writes**: `recover_from_failure()` puts back the key's `before` snapshot only for
  one write that clearly didn't land (`Disconnected`, or the MV7's `Rejected`). A timeout, or
  a preset load or `f` that stopped partway, re-reads the device or marks the state unread.
  `step()` writes a pending wheel adjustment before the next event changes the screen, so
  each snapshot holds only its own change.
- **Model checks**: use `DeviceModel::has_*()` and exhaustive matches, never a model name or
  `_`. A wildcard sent the MV6 Gen 2 the wrong EQ encoding (a800e26).
- **Click targets**: `mouse.rs` regions come from drawn rows. Short terminals squash blocks;
  check heights from 24 (`MIN_SIZE`) up to 50 (bc3bb3f). Below 80×24 only a notice draws.
- **Colors**: new colors use the `C_*` palette constants in `ui.rs`, never a raw
  `Color::Rgb` (the MV7+ LED channel sliders excepted). Lime is reserved for focus and active
  states; values stay white. Red means errors, Muted, clipping and destructive actions, amber
  cautions; neither is decoration. Text people read, including a bar's label in its bar color,
  needs 4.5:1, so `C_DISABLED` is for inactive things only. See `.claude/rules/ui.md`.
- **Stale prose**: when a capability moves between models, update the doc comments, help
  overlay, status strings, Info tab and readme row too. Most "tidy" commits fix this.

**Intentional, don't flag:**

- A preset's Playback Mix of 0 means "not saved", not 0%
- `ACK_TIMEOUT_MS` is 15 ms on every model
- `unsafe` `dup`/`dup2` in `meter.rs` (silences ALSA/JACK probing)
- `send_if_connected()` succeeding silently in demo mode
- No firmware-update packets anywhere
- `field_reassign_with_default` in `app.rs` tests (see Commands)
- The MV7 bypassing `send_set()`/`send_get()` for its text shell
- Factory Reset not marking the device disconnected; the presence poll handles the reboot
- `load_mv7_preset()` reading the mic itself, more than once, instead of trusting the TUI's
  possibly stale copy
- Presets saved before 2.13.1 applying every per-model setting they hold, and before 2.12.0
  `phantom_power = false`, even from a model without it; nothing records which model saved them
- `fake_hid.rs` agreeing with `protocol.rs` by construction: it proves sequencing and
  routing, never that a packet is right. Don't cite it as protocol evidence

## Workflow

- For non-trivial features, explore the relevant code and confirm a plan before implementing.
- If a byte offset or command value is uncertain, say so and propose a usbmon capture rather
  than guessing.
- When a cross-model UI divergence is unavoidable, name the affected models in the commit/PR
  body.
