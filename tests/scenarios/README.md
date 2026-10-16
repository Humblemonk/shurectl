# End-to-end scenarios

Each `.jsonl` file here is one scenario. `cargo test --features qa --test e2e` runs it through
`shurectl --qa` on every model in its header, once with a fake device and once in demo mode
(where packet checks are skipped). The fake device starts in Manual at 24 dB; demo mode
starts where `shurectl --demo` does, in Auto Level at 36 dB. A scenario that depends on the
fake's settings or checks packets runs on the fake only. A **known bug** scenario states the
intended behaviour and must fail until the bug is fixed (see `"known_bug"` in
`tests/e2e.rs`). See `.claude/skills/qa/SKILL.md` for the line format and how to record one.

Keep this list in step with the files: add a row when you add a scenario.

## Not covered yet

Cases a QA sweep planned but no scenario checks. Remove a line when a scenario covers it.

- Navigation: EQ and Dynamics stay reachable in Auto on the MVX2U Gen 2, MV6s and MV7+;
  a click on a 🔒 tab does nothing; focus after switching Mode by click; `Q` quits
- Main: MV6 Gen 2 Auto rows (Target Level, Adaptation Rate); Mic Preset in Auto; the
  Summary block follows each change; a gain write while Config Lock is on
- EQ and Dynamics: `f` on a tab without bands sends nothing; a preset that switches the
  Gen 1 or MV7 to Auto re-locks EQ and Dynamics; MV6 Gen 2 Denoiser level (1–100%);
  Limiter and Compressor hidden on the Gen 2 in Auto
- LED (MV7+): every Behavior, and every Theme per Behavior; Custom RGB under Solid and Live
  (Edge, Middle, Interior)
- Mouse: click targets on the EQ, Reverb, LED, Presets and Info tabs and in Auto; a second
  click on a toggle fires it; preset Load/Delete arming by click; resize while editing a name
- Connection: keys while disconnected; I/O failure (`fail_io`) and recovery with `r`; poll
  timing (no disconnect before the first poll); `r` in demo mode
- Presets: an empty slot; one SET per stored setting in the same order on every model;
  presets from another model (Gain Lock, Monitor Mix kind); name length limit; `s` and `d`
  on the name row; MODE before EQ and Dynamics on load; loading while disconnected
- Info: device name length limit; Tab or `q` while Factory Reset is armed; the MV7 Info
  tab has nothing focusable; the help overlay matches the readme key table

## Navigation and app shell

| Scenario | Models | Runs in | Checks that |
| --- | --- | --- | --- |
| `nav-tab-ring` | all | fake | Tab and Shift-Tab cycle the tabs and wrap at both ends without sending anything |
| `nav-tab-order-mv7plus` | MV7+ | fake, demo | The MV7+ shows all seven tabs in `Tab::ALL` order, Reverb and LED included |
| `nav-tab-order-hides-reverb-and-led` | MVX2U, MVX2U Gen 2, MV6, MV6 Gen 2 | fake | Models without the hardware hide Reverb and LED rather than reordering tabs |
| `nav-focus-ring` | all | fake | Main starts on Mode; ↑/↓ walk Mode → Mute → Gain; `j`/`k` don't move focus |
| `help-overlay` | all | fake, demo | `?` opens and closes help, Esc closes it, and keys and clicks behind it do nothing |
| `mouse-click-targets` | all | fake, demo | Every control's click region focuses that control and sends nothing, at sizes from 80x24 (the minimum) to 120x50 |
| `terminal-too-small` | all | fake, demo | Below 80x24 only the size notice draws: keys, clicks and the wheel change nothing, `q` is typed into a name being edited rather than quitting, and `q` quits otherwise |
| `mouse-wheel-adjusts-gain` | all but MV7 | fake | The wheel over Gain steps it ±1 dB; the wheel over Mode only focuses it |
| `quit` | all | fake, demo | `q` quits without sending anything |
| `quit-ctrl-c` | all | fake, demo | Ctrl-C quits |
| `demo-starts-like-shurectl-demo` | all | demo | Demo mode starts where `--demo` does (Auto Level, 36 dB, "Demo mode" status, no serial), switches to Manual and steps gain down from 36 dB |
| `mouse-click-targets-dynamics` | all | fake | The same click-target check on the Dynamics tab, at 120x40 and 80x24 |
| `meter-peak-hold-expires` | all | fake | The level bar follows the newest peak for 0.3 s and the `pk` readout holds the loudest for 3 s, on the harness's fake clock |
| `harness-demo-skips-no-packets` | all | fake, demo | Harness: `no_packets` is a packet check, skipped in demo mode rather than failed |
| `harness-mv7-demo-has-no-device-name` | MV7 | fake, demo | Harness: the MV7 Info tab shows no device name, as the MV7 has none |

## Main tab

| Scenario | Models | Runs in | Checks that |
| --- | --- | --- | --- |
| `mute-toggle` | all but MV7 | fake, demo | Enter and Space toggle Mute, send MUTE, and show `● ON` in the error colour |
| `gain-adjust` | all but MV7 | fake | ←/→ step gain by 1 dB and send GAIN |
| `gain-limits-60db` | MVX2U, MVX2U Gen 2 | fake | Gain clamps at 0 and 60 dB |
| `gain-limits-36db` | MV6, MV6 Gen 2, MV7, MV7+ | fake | Gain clamps at 0 and 36 dB |
| `gain-lock-blocks-gain` | MV6, MV6 Gen 2, MVX2U Gen 2 | fake | Gain Lock sends GAIN_LOCK and shows LOCKED; ←/→ on Gain then send nothing |
| `monitor-mix-adjust` | all | fake, demo | Monitor Mix (Mic Level on some models) moves 1% per press and clamps at 0 and 100% |
| `monitor-mix-packets` | MVX2U | fake, demo | The Gen 1 slider sends MONITOR_MIX |
| `monitor-mic-playback-levels` | MVX2U Gen 2, MV6, MV6 Gen 2, MV7+ | fake, demo | Mic Level and Playback Level are separate controls that send MIC_LEVEL and PLAYBACK_LEVEL |
| `mv7-monitor-mix-block` | MV7 | fake, demo | Monitor Mix at 10%, 50% and 100% sends the `setBlock 22` words from the usbmon captures |
| `phantom-power` | MVX2U, MVX2U Gen 2 | fake, demo | Enter and Space toggle 48V and send PHANTOM; ←/→ do nothing |
| `config-lock` | MVX2U, MV7 | fake, demo | Enter toggles Config Lock and the block shows LOCKED or Unlocked |
| `config-lock-packets` | MVX2U | fake, demo | Config Lock sends LOCK |
| `mv6-gen2-mic-preset` | MV6 Gen 2 | fake | Enter cycles Mic Preset through Speech, Singing and Instrument; ←/→ do nothing |
| `mode-auto-locks-eq` | MVX2U | fake | Switching to Auto sends MODE and shows EQ and Dynamics as locked (🔒), and Tab skips them |

## Auto Level

| Scenario | Models | Runs in | Checks that |
| --- | --- | --- | --- |
| `auto-level-position-tone` | MVX2U, MV7 | fake | Auto shows Auto Level Controls; Mic Position cycles Near/Far and Tone cycles Natural → Bright → Dark → Natural |
| `auto-level-packets-mvx2u` | MVX2U | fake | Those controls send AUTO_POSITION and AUTO_TONE, and Auto Gain cycles and sends AUTO_GAIN |
| `mv7-auto-level-dsp-mode` | MV7 | fake | Mode, Mic Position and Tone each send one `dspMode` number |

## EQ

| Scenario | Models | Runs in | Checks that |
| --- | --- | --- | --- |
| `eq-gen1-enable-bands` | MVX2U | fake | The EQ switch sends EQ (Enabled/Bypass) and each band's On switch sends its EQ_BAND_n_ENABLE |
| `eq-band-step-clamp` | MVX2U, MV6 Gen 2 | fake | ←/→ on the band selector send nothing; band gain steps 2 dB, clamps at +6/−8 dB; `f` flattens all five bands |
| `eq-band-step-gen2` | MVX2U Gen 2 | fake | The same on the Gen 2, which steps 0.5 dB |
| `eq-tone-steps` | MVX2U Gen 2, MV6, MV6 Gen 2, MV7+ | fake | Tone steps 10% between 100% Dark and 100% Bright and sends TONE; `f` doesn't touch it |
| `mv6-gen2-tone-mode` | MV6 Gen 2 | fake, demo | Tone Mode switches between Simple (Tone slider) and Advanced (5-band EQ) and sends TONE_MODE |
| `mv7-lock-eq-compressor` | MV7 | fake | Config Lock sends `lock on`/`off`; the EQ preset cycles four values (`setBlock 31`); the Compressor cycles four values (`setBlock 19`) |

## Dynamics, Reverb and LED

| Scenario | Models | Runs in | Checks that |
| --- | --- | --- | --- |
| `dynamics-limiter-compressor` | MVX2U, MVX2U Gen 2, MV7+ | fake | Limiter toggles and sends LIMITER; Compressor cycles Off → Light → Medium → Heavy → Off |
| `dynamics-denoiser-popper` | MVX2U Gen 2, MV6, MV6 Gen 2, MV7+ | fake, demo | Clicking focuses without sending; Denoiser and Popper Stopper toggle and send |
| `dynamics-hpf-cycle` | all but MV7 | fake | HPF cycles Off → 75 Hz → 150 Hz → Off and sends HPF |
| `mute-button-setting` | MV6, MV6 Gen 2, MV7+ | fake, demo | Mute Button toggles between Works and Ignored and sends MUTE_BUTTON |
| `reverb-controls` | MV7+ | fake, demo | Reverb output, monitoring, type and intensity (clamped 0–100%) each send their feature |
| `led-brightness` | MV7+ | fake, demo | Enter cycles LED brightness and sends LED_BRIGHTNESS |
| `led-custom-rgb` | MV7+ | fake, demo | Pulsing with a Custom theme reveals R/G/B sliders that send LED_PULSING_COLOR; choosing Shure hides them |
| `led-mv7-live-meter-night-mode` | MV7 | fake, demo | Live Meter and Night Mode toggle and send `meterMode` and `dimMode` |

## Presets

| Scenario | Models | Runs in | Checks that |
| --- | --- | --- | --- |
| `preset-save-load-delete` | all | fake, demo | `s` saves the current settings, Enter loads them back (undoing a mute), `d` deletes without sending |
| `preset-name-edit` | all | fake, demo | Renaming takes typed text including `q`; Ctrl-C doesn't quit while editing; Esc or clicking away cancels |
| `preset-load-unlocks-relocks` | MV6, MV6 Gen 2, MVX2U Gen 2 | fake | Loading under Gain Lock sends unlock → GAIN → re-lock, in that order, and the gain applies |
| `preset-old-file-keeps-fields` | MVX2U Gen 2, MV6, MV6 Gen 2, MV7+ | fake, demo | A pre-Playback-Level preset file (`fixtures/presets-v2.9`) loads without zeroing Playback Level |
| `mv7-text-shell` | MV7 | fake | Gain, mute, a preset load that has to restore a changed gain, and Mode all go over the MV7 text shell |

## Info tab and device lifecycle

| Scenario | Models | Runs in | Checks that |
| --- | --- | --- | --- |
| `info-tab-readback` | all | fake | The Info tab shows the serial number and firmware read from the device |
| `rename-device` | all but MV7 | fake, demo | An empty device name is refused; a valid one sends DEVICE_NAME, without its non-ASCII characters |
| `factory-reset-needs-two-enters` | MV7+, MV6 Gen 2, MVX2U Gen 2 | fake, demo | The first Enter only arms Factory Reset; moving focus or clicking cancels it; a second Enter sends it |
| `factory-reset-reconnects` | MV7+, MV6 Gen 2, MVX2U Gen 2 | fake | After a reset the device drops off, reconnects, and reads back factory state |
| `unplug-replug` | all | fake | An unplug shows DISCONNECTED; replugging reconnects on the second poll or straight away on `r` |
| `readback-order` | MVX2U Gen 2, MV7+ | fake | A dropped ack doesn't shift the replies of a later `r` |
| `refused-command-stays-connected` | all | fake | An unrecognised reply is reported but doesn't mark the device disconnected |
| `readback-failed-reconnect-refuses-writes` | all | fake | If a reconnect can't read the device, writes are refused and the screen kept until `r` succeeds |
| `readback-unanswered-get-keeps-state` | all | fake | **Known bug.** A GET left without a usable reply during `r` should keep that setting, not reset gain to 36 dB |
| `readback-extra-ack-keeps-eq-band-5` | MVX2U Gen 2 | fake | **Known bug.** After one ack too many, `r` should still read EQ band 5's gain |
| `readback-extra-ack-keeps-serial` | MV6, MV7+ | fake | **Known bug.** After one ack too many, `r` should still read the serial number |
