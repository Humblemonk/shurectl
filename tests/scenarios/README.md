# e2e scenarios

Each `.jsonl` file here is one scenario. `cargo test --features qa --test e2e` runs it through
`shurectl --qa` on every model in its header, once with a fake device and once in demo mode
(where packet checks are skipped). Scenarios marked **fake only** need the device. See
`.claude/skills/qa/SKILL.md` for the line format and how to record one.

Keep this list in step with the files: add a row when you add a scenario.

## Navigation and app shell

| Scenario | Models | Checks that |
| --- | --- | --- |
| `nav-tab-ring` | all | Tab and Shift-Tab cycle the tabs and wrap at both ends without sending anything |
| `nav-tab-order-mv7plus` | MV7+ | The MV7+ shows all seven tabs in `Tab::ALL` order, Reverb and LED included |
| `nav-tab-order-hides-reverb-and-led` | MVX2U, MVX2U Gen 2, MV6, MV6 Gen 2 | Models without the hardware hide Reverb and LED rather than reordering tabs |
| `nav-focus-ring` | all | Main starts on Mode; ↑/↓ walk Mode → Mute → Gain; `j`/`k` don't move focus |
| `help-overlay` | all | `?` opens and closes help, Esc closes it, and keys and clicks behind it do nothing |
| `mouse-click-targets` | all | Every control's click region focuses that control and sends nothing, at sizes from 80x24 (the minimum) to 120x50 |
| `terminal-too-small` | all | Below 80x24 only the size notice draws: keys, clicks and the wheel change nothing, `q` is typed into a name being edited rather than quitting, and `q` quits otherwise |
| `mouse-wheel-adjusts-gain` | all but MV7 | The wheel over Gain steps it ±1 dB; the wheel over Mode only focuses it |
| `quit` | all | `q` quits without sending anything |
| `quit-ctrl-c` | all | Ctrl-C quits |

## Main tab

| Scenario | Models | Checks that |
| --- | --- | --- |
| `mute-toggle` | all but MV7 | Enter and Space toggle Mute, send MUTE, and show `● ON` in the error colour |
| `gain-adjust` | all but MV7 | ←/→ step gain by 1 dB and send GAIN |
| `gain-limits-60db` | MVX2U, MVX2U Gen 2 | Gain clamps at 0 and 60 dB |
| `gain-limits-36db` | MV6, MV6 Gen 2, MV7, MV7+ | Gain clamps at 0 and 36 dB |
| `gain-lock-blocks-gain` | MV6, MV6 Gen 2, MVX2U Gen 2 | Gain Lock sends GAIN_LOCK and shows LOCKED; ←/→ on Gain then send nothing |
| `monitor-mix-adjust` | all | Monitor Mix (Mic Level on some models) moves 1% per press and clamps at 0 and 100% |
| `monitor-mix-packets` | MVX2U | The Gen 1 slider sends MONITOR_MIX |
| `monitor-mic-playback-levels` | MVX2U Gen 2, MV6, MV6 Gen 2, MV7+ | Mic Level and Playback Level are separate controls that send MIC_LEVEL and PLAYBACK_LEVEL |
| `mv7-monitor-mix-block` | MV7 | Monitor Mix at 10%, 50% and 100% sends the `setBlock 22` words from the usbmon captures |
| `phantom-power` | MVX2U, MVX2U Gen 2 | Enter and Space toggle 48V and send PHANTOM; ←/→ do nothing |
| `config-lock` | MVX2U, MV7 | Enter toggles Config Lock and the block shows LOCKED or Unlocked |
| `config-lock-packets` | MVX2U | Config Lock sends LOCK |
| `mv6-gen2-mic-preset` | MV6 Gen 2 | Enter cycles Mic Preset through Speech, Singing and Instrument; ←/→ do nothing |
| `mode-auto-locks-eq` | MVX2U | Switching to Auto sends MODE and shows EQ and Dynamics as locked (🔒), and Tab skips them |

## Auto Level

| Scenario | Models | Checks that |
| --- | --- | --- |
| `auto-level-position-tone` | MVX2U, MV7 | Auto shows Auto Level Controls; Mic Position cycles Near/Far and Tone cycles Natural → Bright → Dark → Natural |
| `auto-level-packets-mvx2u` | MVX2U | Those controls send AUTO_POSITION and AUTO_TONE, and Auto Gain cycles and sends AUTO_GAIN |
| `mv7-auto-level-dsp-mode` | MV7 | Mode, Mic Position and Tone each send one `dspMode` number |

## EQ

| Scenario | Models | Checks that |
| --- | --- | --- |
| `eq-gen1-enable-bands` | MVX2U | The EQ switch sends EQ (Enabled/Bypass) and each band's On switch sends its EQ_BAND_n_ENABLE |
| `eq-band-step-clamp` | MVX2U, MV6 Gen 2 | ←/→ on the band selector send nothing; band gain steps 2 dB, clamps at +6/−8 dB; `f` flattens all five bands |
| `eq-band-step-gen2` | MVX2U Gen 2 | The same on the Gen 2, which steps 0.5 dB |
| `eq-tone-steps` | MVX2U Gen 2, MV6, MV6 Gen 2, MV7+ | Tone steps 10% between 100% Dark and 100% Bright and sends TONE; `f` doesn't touch it |
| `mv6-gen2-tone-mode` | MV6 Gen 2 | Tone Mode switches between Simple (Tone slider) and Advanced (5-band EQ) and sends TONE_MODE |
| `mv7-lock-eq-compressor` | MV7 | Config Lock sends `lock on`/`off`; the EQ preset cycles four values (`setBlock 31`); the Compressor cycles four values (`setBlock 19`) |

## Dynamics, Reverb and LED

| Scenario | Models | Checks that |
| --- | --- | --- |
| `dynamics-limiter-compressor` | MVX2U, MVX2U Gen 2, MV7+ | Limiter toggles and sends LIMITER; Compressor cycles Off → Light → Medium → Heavy → Off |
| `dynamics-denoiser-popper` | MVX2U Gen 2, MV6, MV6 Gen 2, MV7+ | Clicking focuses without sending; Denoiser and Popper Stopper toggle and send |
| `dynamics-hpf-cycle` | all but MV7 | HPF cycles Off → 75 Hz → 150 Hz → Off and sends HPF |
| `mute-button-setting` | MV6, MV6 Gen 2, MV7+ | Mute Button toggles between Works and Ignored and sends MUTE_BUTTON |
| `reverb-controls` | MV7+ | Reverb output, monitoring, type and intensity (clamped 0–100%) each send their feature |
| `led-brightness` | MV7+ | Enter cycles LED brightness and sends LED_BRIGHTNESS |
| `led-custom-rgb` | MV7+ | Pulsing with a Custom theme reveals R/G/B sliders that send LED_PULSING_COLOR; choosing Shure hides them |
| `led-mv7-live-meter-night-mode` | MV7 | Live Meter and Night Mode toggle and send `meterMode` and `dimMode` |

## Presets

| Scenario | Models | Checks that |
| --- | --- | --- |
| `preset-save-load-delete` | all | `s` saves the current settings, Enter loads them back (undoing a mute), `d` deletes without sending |
| `preset-name-edit` | all | Renaming takes typed text including `q`; Ctrl-C doesn't quit while editing; Esc or clicking away cancels |
| `preset-load-unlocks-relocks` | MV6, MV6 Gen 2, MVX2U Gen 2 | Loading under Gain Lock sends unlock → GAIN → re-lock, in that order, and the gain applies |
| `preset-old-file-keeps-fields` | MVX2U Gen 2, MV6, MV6 Gen 2, MV7+ | A pre-Playback-Level preset file (`fixtures/presets-v2.9`) loads without zeroing Playback Level |
| `mv7-text-shell` | MV7 | Gain, mute, a preset load that has to restore a changed gain, and Mode all go over the MV7 text shell |

## Info tab and device lifecycle

| Scenario | Models | Checks that |
| --- | --- | --- |
| `info-tab-readback` | all | The Info tab shows the serial number and firmware read from the device |
| `rename-device` | all but MV7 | An empty device name is refused; a valid one sends DEVICE_NAME, without its non-ASCII characters |
| `factory-reset-needs-two-enters` | MV7+, MV6 Gen 2, MVX2U Gen 2 | The first Enter only arms Factory Reset; moving focus or clicking cancels it; a second Enter sends it |
| `factory-reset-reconnects` | MV7+, MV6 Gen 2, MVX2U Gen 2 | **Fake only.** After a reset the device drops off, reconnects, and reads back factory state |
| `unplug-replug` | all | **Fake only.** An unplug shows DISCONNECTED; replugging reconnects on the second poll or straight away on `r` |
| `readback-order` | MVX2U Gen 2, MV7+ | **Fake only.** A dropped ack doesn't shift the replies of a later `r` |
| `refused-command-stays-connected` | all | **Fake only.** An unrecognised reply is reported but doesn't mark the device disconnected |
| `readback-failed-reconnect-refuses-writes` | all | **Fake only.** If a reconnect can't read the device, writes are refused and the screen kept until `r` succeeds |
