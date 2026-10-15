---
name: qa
description: Drive shurectl headlessly with `--qa` (JSON lines over stdin/stdout) to QA a change across all six models, or to turn a bug into a regression scenario in tests/scenarios/. Use for a cross-device consistency pass, to reproduce a UI or packet-order bug without hardware, or when asked to "QA", "walk every tab", or "write an e2e scenario".
---

# shurectl QA pass

`shurectl --qa` runs the real app (`handle_key()`, `mouse::handle_mouse()`,
`apply_action()`, `ShureDevice`) against a fake device and renders into a buffer
instead of a terminal. Each line you send is answered with one JSON line. Nothing
touches real hardware or `~/.config/shurectl`.

```
cargo build --features qa
./target/debug/shurectl --qa --fake-device mv7plus   # packets logged by name
./target/debug/shurectl --qa --demo mv6              # no device: navigation only
printf '{"key":"Tab"}\n{"query":"screen"}\n' | ./target/debug/shurectl --qa --demo mv6
```

Models: `mvx2u`, `mvx2u-gen2`, `mv6`, `mv6-gen2`, `mv7`, `mv7plus`. Both modes start
from the same state: Manual mode, 24 dB, defaults elsewhere. Run a pass once per
model; a bug on one model is usually a divergence from the others.

## Lines you send

Inputs:

| Line | Does |
| --- | --- |
| `{"key": "Tab"}`, `{"key": "BackTab", "mods": "shift"}`, `{"key": "c", "mods": "ctrl"}` | One key press, named as crossterm names it (`Enter`, `Esc`, `Up`, `Left`, `Backspace`, `Space`, `F1`, or one character). `"repeat": n` presses it n times |
| `{"type": "My Preset"}` | One key press per character |
| `{"click": [col, row]}`, `{"wheel": "up", "at": [col, row]}` | Mouse at a cell |
| `{"click_on": "Mute:"}` | Click the first cell of the first row containing that text |
| `{"resize": [80, 24]}` | Redraw at a new size (max 300x120) |
| `{"tick": 2100}` | Advance the fake clock (ms, max 60000); runs the presence poll |
| `{"device": "unplug"}`, `{"device": "replug"}` | Fake device only |
| `{"device": {"drop_acks": 1, "extra_acks": 1, "refuse": 1, "fail_io": true}}` | Inject faults: fewer or more acks after the next CONFIRM, answer the next command with an unknown address (`[Failed]` on the MV7), fail all I/O |
| `{"meter": -12.5}` | Push a peak (dBFS) into the meter |
| `{"preset_dir": "fixtures/presets-v2.9"}` | Copy preset files into this session's temp preset dir |
| `{"reset": true}` | Start over on the same model |

Queries (answer in `result`): `{"query": "screen"}`, `"screen_styled"` (runs with
palette names like `C_ERROR`), `"state"` (every `DeviceState` field), `"hitmap"`
(every click region: use it to find targets on a squashed terminal), `"packets"`
(the whole log), `"focus_ring"` (what Down visits from here), `"models"`,
`"features"` (names the fake decodes).

Every reply has `ok`, `app` (`tab`, `focus`, `status`, `status_is_error`,
`help_visible`, `editing`, `connected`, `quit`, `fits`, …), `screen`, `changed` (rows that
differ from the previous reply), `packets` since the previous line, `failures` and
`errors`. A packet the fake refuses (malformed, not something `protocol.rs` builds)
is an error: report it.

## Expectations

`{"expect": {...}}` checks every key and reports the actual value on failure;
exploring continues.

| Key | Checks |
| --- | --- |
| `screen_contains`, `screen_not_contains` (string or list), `screen_row: [n, "text"]` | Screen text. Prefer `screen_contains` over row numbers |
| `screen_hash` | The `renders_match_golden` hash, for a pinned screen |
| `style: {"text": "Muted", "fg": "C_ERROR"}` | Color of the text's first cell, by palette name |
| `tab`, `focus`, `status` (substring), `status_is_error`, `help_visible`, `editing`, `quit`, `confirming_factory_reset` | `App` fields |
| `fits` | `ui::fits()` for the last frame: `false` below 80x24 (`MIN_SIZE`), where only the size notice draws and keys only quit |
| `connected` | `App::device_connected`, and the header agrees |
| `state: {"muted": true}` | `DeviceState` fields (see the `state` query for names) |
| `set: {"feature": "GAIN", "value": 250}` or `set: "GAIN"` | A SET since the previous `expect` line. `"applied": false` means the device ignored it |
| `sets: [...]` | SETs in this order (others may come between) |
| `get: "SERIAL"`, `text_sent: "micMute on"` (MV7), `no_packets: true`, `packets_raw: ["01 12 11 22"]` | Other packets since the previous `expect` |
| `click_targets: true` | Clicks every unfocused control region: each must focus that control and send nothing |

In demo mode the packet keys are skipped, not failed.

## A QA pass

Read **Cross-Device UI Consistency** in `AGENTS.md` first. Then, for every model:

1. Walk every tab with Tab and BackTab, and every focus with Up/Down (`focus_ring`).
2. On each focus, press Left, Right and Enter, and check the status message, the
   screen and the packets. Every visible, focusable control must do something.
3. Repeat the Main tab at heights 24, 30, 40 and 50 with `click_targets`. Below 80x24 only
   a notice draws (`terminal-too-small`).
4. Compare models: the same control must have the same label, units, formatting,
   key, status wording and position relative to its neighbours.

Report each divergence as a recorded scenario whose `expect` fails today. Classes
worth looking for: a focusable control that does nothing on one model, a label or
status spelled differently on two models, a click region on the wrong control at one
height, a key only one model answers, a packet sent in demo mode.

## Saving a scenario

```
{"record": "start"}
{"key": "Down"}
{"key": "Enter"}
{"expect": {"set": {"feature": "MUTE", "value": true}}}
{"record": "save", "as": "tests/scenarios/mute-on-mv6.jsonl", "models": ["mv6"]}
```

Paths are relative to where `--qa` runs (`tests/` under the e2e runner). Trim the
file to the lines that matter. `cargo test --features qa --test e2e` runs every file
in `tests/scenarios/` per model in its header and in both fake and demo mode (add
`"modes": ["fake"]` when it needs the device). The runner refuses a file with no
`expect` line.

A recording asserts what the app did, which may be the bug. In the PR body, say for
each `expect` line whether it states intended behaviour or only observed behaviour.

## Limits

The fake is built from `protocol.rs`, so it agrees with the app on every address and
encoding. It catches sequencing, routing, UI and state bugs, never a protocol
misreading; hardware truth is usbmon captures. It does not emulate the MV7's `help`
listing (that needs a capture from a real MV7) or the side effects of an MV7 mode
change on EQ and compressor.
