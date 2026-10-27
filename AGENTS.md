# shurectl

Terminal UI configurator for Shure USB audio interfaces (MVX2U Gen 1/Gen 2, MV6, MV6 Gen 2,
MV7, MV7+) on Linux, macOS and Windows, all three covered by CI. Talks to the device
directly over USB HID, replacing the Windows/Mac-only ShurePlus MOTIV Desktop app.
Single-crate Rust binary. Prefer the simple, obvious solution over clever abstractions.

File-specific detail lives in `.claude/rules/`. Read the matching file before editing:
`protocol.md` for `protocol.rs`/`device.rs`/`probe.rs` (HID packet format, usbmon
debugging), `meter.md` for `meter.rs`/`main.rs`, `presets.md` for `presets.rs`, `ui.md` for
`ui.rs` (MOTIV Mix palette and controls). Claude Code loads these only when a matching file
is read, so anything a reviewer must know from a diff alone belongs in **Review** below.

## Commands

```
cargo run -- --demo mv7plus     # no hardware; also mvx2u, mvx2u-gen2, mv6, mv6-gen2, mv7
cargo run -- --list             # list connected devices
scripts/check.sh                # verification gate; CI runs the same script
```

Run `scripts/check.sh` before calling any change complete. It runs clippy and
`cargo build`, both `--all-targets --features probe,qa`, then `cargo fmt --check`,
`cargo test` and the end-to-end scenarios, and any warning fails it, test code included.
Change the gate in the script, not in docs or CI.

- Without `--features probe,qa`, clippy skips `shurectl-probe` and the `--qa` harness.
- CI also runs jscpd (budget 5% in `.github/linters/.jscpd.json`, ~2% actual):
  `npx jscpd -c .github/linters/.jscpd.json .`. Fix duplication by extending a shared
  helper; don't raise the threshold.

## Architecture

```
src/
  main.rs       # Entry point, CLI args, event loop, apply_action()
  app.rs        # App state: Tab, Focus, DeviceAction; DeviceState lives in protocol.rs
  cli.rs        # One-shot commands: --json, --mute, --preset, --mode, --gain, monitor levels
  device.rs     # hidapi wrapper: open device, send/receive HID reports
  meter.rs      # cpal capture: dBFS metering, RollingWindow, PeakWindow
  mouse.rs      # HitMap from the last rendered frame; clicks/wheel → the same key actions
  presets.rs    # Host-side presets: TOML load/save/delete, PresetSlot
  protocol.rs   # Packet encoding, CRC-16/ANSI, command constructors, apply_response()
  ui.rs         # ratatui rendering: the tabs in Tab::ALL + help overlay
  ui_renders.golden  # Render hashes checked by ui.rs `renders_match_golden`
  bin/probe.rs  # Maintainer-only HID address sweeper, built only under `--features probe`
  firmware.rs   # Info tab firmware check: background lookup in Shure's online archive
  fake_hid.rs   # Fake device behind device.rs's HidTransport (tests and `--features qa` only)
  harness.rs    # `--qa` JSON-lines driver over the real event loop step (tests and qa only)
tests/
  e2e.rs        # Runs tests/scenarios/*.jsonl through `shurectl --qa` per model; PTY smoke
  scenarios/    # One JSON-lines file per scenario
  fixtures/     # Older preset files, preset-loads.golden (main.rs `preset_loads_match_golden`),
                #   firmware-archive.json (a recorded archive reply, firmware.rs tests)
```

**Control flow:** key event → `step()` → `handle_key()` → `DeviceAction` → `apply_action()`
(main.rs) → `device.rs` → `HidTransport` → `protocol.rs` packet. `run_event_loop()` and the
`--qa` harness both call `step()`; only the harness swaps in a fake transport and clock.

**Layering rules (strict):**
- `apply_action()` in `main.rs` is the *only* place that writes to the device. Never call
  `device.rs` from `ui.rs`, `app.rs` or `mouse.rs`. Mouse input only maps to key actions.
- Raw protocol byte values live *only* in `protocol.rs` as named constants.
- `shurectl-probe`, `--qa` and `fake_hid.rs` must never ship to end users via `cargo install`
  or Homebrew. Keep the `probe`/`qa` features and `cfg(any(test, feature = "qa"))` gates intact.
- Never write firmware-update packets. Those byte sequences are intentionally omitted (see
  the readme Credits and legal section).

**Demo mode:** `--demo` runs with `device: None`, and `send_if_connected()` silently succeeds.
State changes still apply; only HID writes are skipped. Demo mode must stay fully navigable.

**Connection state:** `poll_presence()` checks the OS device list every 2 s, with no HID
traffic. A device that is back is reconnected one poll later through `DeviceAction::Reconnect`
(`r` does it on demand), so `apply_action()` stays the only writer. Only a
`device::Disconnected` error marks it disconnected; one that refuses a command stays connected.

**Live poll:** `poll_live()` reads mute, mode, gain and the monitor levels every second
(`read_live()`: one GET each, kept only when answered at the address asked,
`answers_at_its_address()`; on the MV7 one text line each) while connected, so a change made
elsewhere shows without `r`: the mic's own mute button, a script, MOTIV. Otherwise the next
arrow key would step from a stale value and jump the level. A mode change re-reads everything.
While the settings are unread only mute is announced (the rest would read as changes from
defaults), and in Auto a gain change is kept but not announced. On the MV7 an unanswered
`micMute` skips the poll's other lines; the MV6's unconfirmed Playback Level GET (#99) isn't
polled. It runs while the other settings are unread too: mute is a fixed write (`ActionKind`),
allowed before a readback in the TUI and CLI, because refusing it would leave the mic live. A
mute whose outcome nothing could read back shows as OFF, never Muted over a live mic
(`recover_from_failure()`). The poll only reads. It ignores an unusable answer and marks the
device disconnected on a `Disconnected` error. The binary models drain queued input first. On
the MV6 Gen 2 under Linux, a pressed mute button sends no input report, and the poll shows the
press (tested 2026-10-23). On the MVX2U Gen 2, gain, Monitor Mix, mode and mute changed from
the command line showed in a running TUI, and the next arrow key stepped from the new gain
(tested 2026-10-24). On the MV6 Gen 2, the same held for gain and mute, and a button press
still showed (tested 2026-10-25). On the MV7+, a gain change from the command line showed
within about a second (reported by mike-callahan, 2026-10-27). Unconfirmed elsewhere: whether
other models report a press, whether their GETs reflect changes made elsewhere, and whether the
MV7 answers each line within the poll's 200 ms. Any write from the TUI pushes the next poll
back a second (`send_from_tui()`), since whether a device applies a write before acking it is
unconfirmed.

## Adding a New Command

In order, without skipping steps:

1. `protocol.rs`: `FEAT_*` constant, `cmd_get_*`/`cmd_set_*` constructors, an
   `apply_response()` branch decoding into `DeviceState`, and one roundtrip test (see
   **Testing**)
2. `device.rs`: typed `get_*`/`set_*` methods on `ShureDevice`. If it's part of full
   readback, add the getter to the `getters` slice in the `get_state_*()` function of every
   model that supports it, and its UI name to `GET_LABELS` in `protocol.rs`
   (`every_readback_query_names_its_setting` fails until you do)
3. `fake_hid.rs`: add the setting to `features()` so the fake stores and answers it
   (`get_state_reads_back_the_seeded_state_on_every_model` fails until you do)
4. `app.rs`: `DeviceAction` variant if user-triggerable, wired into `adjust_focused()` or
   `toggle_focused()`, and listed in `App::offers()`; `main.rs`: handle it in
   `apply_action()`. It gets no command-line flag: see **Command line**
5. `ui.rs`: UI element if needed, following **Cross-Device UI Consistency**
6. `presets.rs`: if it's a DSP setting, add it to `PresetSlot`, `from_device_state()` and
   `apply_to_device_state()` (see the **New `PresetSlot` field** trap under Review)
7. `README.md`: a row in the collapsed Device support table, with the models that have it,
   and the key table if a shortcut changed. Keep the top of the readme for install and first
   run; don't add sections there

## Command line

The CLI stays small: one-shot commands for scripts, hotkeys and Stream Deck buttons, with no
long-running modes and no general get/set (maintainer decisions 2026-10-23 and 2026-10-24, after
#58 and #126). `--json` prints identity, mute and the settings as `PresetSlot` serializes them
(EQ band gains converted from tenths to dB), so a new setting shows there through step 6 with no
extra work. Only mute, preset loading, mode, gain, Monitor Mix and Playback Level are written;
don't add another, or a flag that repeats one. Every write goes through `apply_action()` behind
`App::offers()` (`cli.rs`), so the CLI changes only what the TUI would change right now.

## TUI / Focus Model

`Tab` is the panel, `Focus` the control in it. `adjust_focused()` (←/→) and `toggle_focused()`
(Enter/Space) return `Option<DeviceAction>`; `None` is a UI-only change. Name editing lives in
`main.rs::key_action()`: chars (`q` included) append, Enter commits, Esc or a click away cancels.

## Cross-Device UI Consistency

A user owns one device but reads one readme, one help overlay, and one set of screenshots.
If "Gain Lock" on the Gen 2 is "Lock" on the MV6, the docs stop matching reality. Divergence
is also the largest maintenance cost here: `ui.rs` has a `draw_main_left_*` variant per
model and mode plus a `DeviceModel` match in `draw_info_tab()`, and `app.rs` matches on
`DeviceModel` in `reset_focus_for_tab()`, `focus_next()` and `focus_prev()`.

**The rule:** any control that exists on more than one model has identical label, units,
value formatting, keybinding, and position relative to its neighbours on all of them. Only
genuinely device-exclusive hardware features may differ.

Intended differences (hardware, not divergence; don't flag them):

- Gain: 0–60 dB on the MVX2U Gen 1 and Gen 2, 0–36 dB elsewhere; 1.5 dB steps on the MV7
- EQ band steps: 0.5 dB on the MVX2U Gen 2, 2 dB on the Gen 1 and MV6 Gen 2
- "Monitor Mix" (a balance) on the MVX2U Gen 1 and MV7; "Mic Level" on models with Playback Level
- "Auto Gain" (MVX2U Gen 1) and "Target Level" (MV6 Gen 2): one setting at FEAT_AUTO_GAIN,
  labelled per model, as MOTIV labels it Gain (Quieter / Normal / Louder) on the Gen 1 and
  Target Level on the MV6 Gen 2 (MOTIV screenshots; Gen 1 values per shux)
- EQ and Dynamics locked in Auto only on the MVX2U Gen 1 and MV7 (`is_tab_locked()`)
- Tabs and controls hidden per model, as the readme's Device support table lists

Shared across all models unless the hardware makes it impossible:

- **Tab set and order**: `Tab::ALL` is the source of truth. Models filter it, never reorder it
- **Keybindings**: Tab/Shift-Tab cycles tabs, ↑/↓ moves focus, ←/→ adjusts, Enter/Space
  toggles, `?` help. No key only one model responds to. A readme shortcut that works on
  some devices only says which
- **Focus traversal**: mode → mute → gain → monitor mix → device extras. Models skip entries
  they lack; they don't reorder them because it "reads better" there
- **Summary**: the Main tab's Summary is one ordered `SummaryRow` list; a model picks its
  rows in `summary_rows()` but can't reorder them
- **Labels, units, formatting**: "Gain", "Monitor Mix", "Denoiser" are spelled the same
  everywhere. dB values, percentages, and enum names render through the same code path
- **Drawing helpers** (listed in `.claude/rules/ui.md`): extend a shared helper with a
  parameter rather than forking a near-copy, and pass behaviour in rather than hardcoding a
  model name in it
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
3. A per-model `draw_*` fork, if genuinely required, keeps its siblings' block order,
   borders and spacing; don't start it by copying one and tweaking strings.
4. Update every `DeviceModel` match (focus fns, `draw_main_left()`, `summary_rows()`,
   `draw_info_tab()`, preset serialization), then check `--demo` on all six models.

## Rust Rules

- No `unwrap()`/`expect()` in production paths; no `panic!()` outside tests; no
  `todo!()`/`unimplemented!()` in final code
- No `println!()` once the TUI runs: `eprintln!()` only at startup, and the TUI status bar
  after that. The one-shot commands (`--list` and those in `cli.rs`) and `shurectl-probe`
  print results to stdout so scripts can read them
- `anyhow::Result<T>` for all fallible functions
- Prefer borrowing; justify every `.clone()`
- Exhaustive match arms; no wildcard `_` that silently swallows variants. Check models with
  `DeviceModel::has_*()` or an exhaustive match, never a model name
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
| User-visible behaviour (keys, focus, clicks, status, packet order, reconnects) | A scenario in `tests/scenarios/`, run on every model in its header, with a fake device and in demo mode. Record one with `shurectl --qa` (the `qa` skill, `.claude/skills/qa/SKILL.md`) |
| `ui.rs` rendering changes | `renders_match_golden` must pass unchanged for a refactor. For an intended change, rerun with `UPDATE_GOLDEN=1` and check the listed screens in `--demo`. `every_screen_fits_the_min_size` fails when a screen no longer fits 80×24 (`MIN_SIZE`), or a focus hint is cut mid-word instead of going through `with_hint()` |
| Preset save or load changes (`presets.rs`, `apply_binary_preset()`, `load_mv7_preset()`) | `preset_loads_match_golden` loads every preset source on every model, checks its rules (see the test) and compares the SETs with `tests/fixtures/preset-loads.golden`. For an intended change, rerun with `UPDATE_GOLDEN=1` and read the diff |
| `main()` / CLI args | No tests |

Performance is not a concern (~100 ms input-driven tick). No benchmarks unless a specific
bottleneck has been identified.

## Review

The verification gate and CI already cover clippy, fmt, tests, the end-to-end scenarios and
jscpd; don't report what they catch. `renders_match_golden` proves a `ui.rs` change is
invisible when its golden file is unchanged; a changed golden file lists the screens to look at.

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
  byte routes only FEAT_MIX 0x03 (Playback Mix), confirmed on the MV6 Gen 2, MVX2U Gen 2
  and MV7+ (a MOTIV capture on firmware 1.6, checked 2026-10-27).
  `get_state()` fails unless every required GET got a usable reply, so no writable field is
  left at its default (36 dB gain). It drains queued input first, and
  `send_get_past_strays()` passes over reports that can't be the GET's answer (a late reply,
  or on Linux another shurectl's: hidraw gives every open handle each report). For a
  live-poll GET (mute, mode, gain, the monitor levels), answered at its own address, that is
  any other report; for other GETs, a reply at a live-poll address. When every report read
  is stray, the GET has no answer and the readback fails rather than pass with a default.
  Whether every GET is answered at its own address is unconfirmed, so a general check needs
  captures first. `query_as()` with
  `Need::Optional` is for a GET unchecked on that model's hardware or missing from some
  firmware. A new one's field is an `Option`
  left `None`, as the Mute Button's is, so nothing shows, saves or writes a default.
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
- **Model checks**: a wildcard `_` over `DeviceModel` sent the MV6 Gen 2 the wrong EQ
  encoding (a800e26). See Rust Rules.
- **Click targets**: `mouse.rs` regions come from drawn rows. Short terminals squash blocks;
  check heights from 24 (`MIN_SIZE`) up to 50 (bc3bb3f). Below 80×24 only a notice draws.
- **Colors** (`.claude/rules/ui.md`): `C_*` constants only, no raw `Color::Rgb` (MV7+ LED
  sliders excepted). Lime only for focus and active states; values stay white. Red and amber
  mean errors and cautions, never decoration. Readable text needs 4.5:1; `C_DISABLED` doesn't.
- **Stale prose**: when a capability moves between models, update the doc comments, help
  overlay, status strings, Info tab and readme row too. Most "tidy" commits fix this.
- **Hardware claims**: a diff that states or copies a claim about device behaviour with
  neither its evidence nor "unconfirmed" (see **Workflow**). Copying an older claim counts:
  check where it came from before repeating it.

**Intentional, don't flag:**

- A preset's Playback Mix of 0 means "not saved", not 0%
- `ACK_TIMEOUT_MS` is 15 ms on every model
- `unsafe` `dup`/`dup2` in `meter.rs` (silences ALSA/JACK probing)
- `send_if_connected()` succeeding silently in demo mode
- `firmware.rs` contacting Shure's firmware archive once at every non-demo start of a
  compared model, with no opt-out and no retry. The request names every compared model, so
  it says a supported Shure device is connected but not which
- The MV7 bypassing `send_set()`/`send_get_past_strays()` for its text shell
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
- A comment, doc, changelog line or commit message that says how a device behaves names its
  evidence: a usbmon capture, a probe run or a test on hardware, with the model and date
  ("probed 2026-10-12 on the MV6 Gen 2"). Without evidence it says "unconfirmed". An
  explanation that fits a result is not evidence: "the MV6 answers its Mic Level only after
  a SET" explained a probe bug and was repeated for six months (0c3a0de).
- When a cross-model UI divergence is unavoidable, name the affected models in the commit/PR
  body.
- Review a change that touches device writes, readback, presets or a release in a session
  that didn't write it: give it the diff, the intent and the hardware results, not the
  author's conclusions about whether the code is right. Docs- or test-only changes can skip
  this.
