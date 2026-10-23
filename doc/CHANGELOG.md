# Changelog

What changed in each shurectl release, newest first. Only changes you can see or feel are
listed; internal refactors, test work and CI changes are left out. To check which version you
have, run `shurectl -V` or open the Info tab.

Releases that need you to do something after upgrading have an **Upgrading** note.

## 2.14.2 — 2026-10-23

Mute follows the mic's own button, and presets leave mute alone.

### Changed

- Mute follows the mic: when you mute or unmute with the mic's own button, shurectl shows
  it within a second, without pressing `r`. shurectl reads the mute state once a second.
- Presets no longer save or change mute. Loading a preset never unmutes a muted mic, or
  mutes a live one. Presets you saved before keep working; the mute state in them is
  ignored. The Mute Button setting (Works / Ignored) is still saved.

## 2.14.1 — 2026-10-20

The Info tab says whether your firmware is up to date.

### Added

- The Info tab's Firmware row says "Up to date", or which newer version Shure has released
  and that it's in the MOTIV app. shurectl looks this up in Shure's online firmware archive
  at startup; without a connection, the row shows only your version, as before. Not on
  the MV7, whose firmware reports its version differently from the archive, or the MVX2U
  Gen 1, whose version format is unconfirmed.

## 2.14.0 — 2026-10-20

The MV7+ works again on firmware older than 1.4.0.

### Fixed

- **MV7+:** on firmware older than 1.4.0, shurectl reads and changes the mic's settings
  again, and `--mute` works. Since 2.13.1 it refused every change with "Couldn't read a
  setting: no usable reply to Mute Button", because that firmware doesn't have the Mute
  Button setting.

### Changed

- **MV7+:** the Mute Button setting needs firmware 1.4.0 or newer. On older firmware, the
  Summary and the Mute Button card say so, and the card shows your firmware version. Update
  the mic with Shure's MOTIV app to use it.
- **MV7+:** if shurectl can't read the Mute Button setting, it shows Unknown and can't be
  changed, instead of blocking every other setting. Presets neither save nor load it then,
  and a preset load says it was left out.

## 2.13.2 — 2026-10-17

Clearer errors when shurectl can't read a setting.

### Changed

- When shurectl can't read every setting from the device, the status bar now names the
  setting, such as "Couldn't read a setting: no usable reply to Mic Level at feature 0x01
  0x86 (prefix 0x00)", instead of showing only the address. Please include that message
  in a bug report.

## 2.13.1 — 2026-10-16

Safety fixes: what's on screen now always matches what the device has.

### Fixed

- After a write that failed, shurectl no longer rolls the whole screen back to an old copy.
  It undoes only a change that clearly didn't reach the device, and otherwise reads the
  device again. Before, a preset load or Flatten EQ that stopped partway could leave the
  screen showing Muted while the mic was live.
- If shurectl can't read every setting from the device, it says so ("Couldn't read a
  setting") and refuses changes until a full read succeeds, instead of showing defaults
  such as 36 dB gain. Press `r` to retry. A gain step from that default could make the
  level jump.
- Loading a preset writes only the settings the preset saved, rather than filling the gaps
  from what's on screen.
- Presets saved on the MVX2U Gen 2 no longer switch its EQ bands off. Only the MVX2U Gen 1
  has per-band on/off switches, and only it saves or receives them.
- A preset without a Playback Level leaves your current Playback Level alone.
- Scrolling the mouse wheel and then pressing a key writes the scrolled value first, so
  each change is handled on its own.

## 2.13.0 — 2026-10-16

### Changed

- Every model now fits a standard 80×24 terminal. The header and tab bar are one row each,
  Mode and Mute sit side by side on the Main tab, and the preset cards and help overlay are
  more compact.
- In a terminal smaller than 80×24, shurectl shows the current size and the size it needs.
  Only `q` and `Ctrl+C` work there, and mouse clicks are ignored.
- Dynamics cards wrap onto more rows instead of cutting off their titles. On a wide
  terminal the MV7+ shows two rows of three.
- A focused control's key hint is left out when it doesn't fit, rather than being cut off
  mid-word.
- Shorter labels so warnings fit: Config Lock reads "⚠️ Changes ignored" (MVX2U Gen 1,
  MV7), Phantom Power is labelled "48V:" (MVX2U Gen 1 and Gen 2), and a Mic Preset that
  doesn't match any preset shows "(custom)" (MV6 Gen 2).

## 2.12.0 — 2026-10-15

### Fixed

- **MV7:** loading a preset no longer stops at the first setting that was already correct.
  It sends only the settings that differ, skips one the mic refuses (and names it in the
  status bar, marked "(locked)" if Config Lock is on), and loads the rest.
- **MV7:** `shurectl --mute on` succeeds when the mic is already muted.
- **MVX2U Gen 1 and Gen 2:** 48V phantom power is now saved only in presets made on these
  two models, so loading a preset from another model can't switch phantom power off under
  a condenser mic. Presets saved before 2.12.0 still apply the value they contain.
- If reading the device's settings failed at startup or on reconnect, changes and preset
  saves are refused until you press `r`, so nothing is written based on guessed values.

### Upgrading

- Presets saved before 2.12.0 on an MV6, MV6 Gen 2, MV7 or MV7+ still switch 48V off when
  loaded on an MVX2U Gen 1 or Gen 2. If you load them there with a condenser mic, load each
  one on the mic it was made for and save it again, so it no longer holds 48V. Without that
  mic, delete the `phantom_power` line from the preset file instead.

## 2.11.1 — 2026-10-12

### Changed

- Playback Level is drawn in a lighter blue instead of orange, so a full bar no longer
  looks like a warning.
- Gain Lock and Config Lock show LOCKED in amber rather than error red, and Unlocked in
  plain white.
- The Summary shows "Muted: NO" in gray instead of green.
- Hints, the help overlay's close hint, the meter ruler and empty preset slots use a
  brighter gray that's easier to read. EQ band frequencies are white like other values.
- **MV7+:** the red and blue LED color sliders are lighter so their values are readable.

## 2.11.0 — 2026-10-12

A new look based on Shure's MOTIV Mix app, and more MVX2U Gen 2 controls.

### Added

- **MVX2U Gen 2:** Factory Reset on the Info tab, as in MOTIV Mix. It also clears the
  device name.
- **MVX2U Gen 2:** Monitor Mix is now two sliders, Mic Level and Playback Level, as in
  MOTIV. Both are read from the device and saved in presets.

### Changed

- Colors follow Shure's MOTIV Mix palette: lime for the focused control and active states,
  white for values, green for ON, red for Muted and errors. The focused control also gets a
  thick border, so focus doesn't rely on color alone.
- The selected tab is bold with a lime underline. Option rows such as Input Mode, Auto
  Level, Mic Preset and Tone Mode are segmented controls on every model, and Input Mode
  reads "Auto Level | Manual".
- The Info tab lists Device Actions before Capabilities, so Factory Reset stays on screen
  in short terminals.
- Dynamics and Reverb card descriptions wrap instead of being cut off. Hints read
  "[Enter] toggle" and "[Enter] cycle" everywhere.
- The physical mute button setting reads "Pressing it: Works / Ignored". The old "Mute
  Button: ON" looked like the mic was muted (MV6, MV6 Gen 2, MV7+).
- The Summary panel lists settings in the same order on every model.

### Fixed

- Loading a preset never writes a Playback Level of 0, which would silence playback.
- A preset from a model whose Monitor Mix is a mic/playback balance (MVX2U Gen 1, MV7) no
  longer sets Mic Level on other models, and the other way round. Before, a Gen 1 "all
  mic" preset could mute the mic on a Gen 2 or MV7+.
- A preset's Monitor Mix is written last, so if the device refuses it, the other settings
  still load.
- The "no device found" message lists the MV6 Gen 2.

## 2.10.0 — 2026-10-11

### Changed

- **MV6, MV6 Gen 2 and MV7+:** Monitor Mix is one block with Mic Level and Playback Level
  rows, as in MOTIV, instead of two separate blocks. Status messages, the Info tab and the
  readme use the same names. The MVX2U Gen 1, MVX2U Gen 2 and MV7 keep their single
  balance slider.

### Fixed

- A late reply from the device for Playback Level no longer lands in Monitor Mix and
  shifts every setting after it by one.
- Loading a preset onto a device with Gain Lock on unlocks it, writes the gain, and locks
  it again straight away, so a failed write can't leave the device unlocked.
- After a Factory Reset, shurectl waits for the device to finish restarting before
  reconnecting.
- **MV7:** trying to set the high-pass filter now says "HPF is not available on the MV7".

## 2.9.1 — 2026-10-10

### Fixed

- **Meter:** if the audio capture stalls, the meter drops to "---" instead of freezing on
  its last level. Short peaks and clips are no longer missed, and levels round to 0.1 dB
  rather than truncating, so -2.96 dB now reaches the red zone. The ruler's "0" label is
  drawn. Audio inputs in less common sample formats now work.
- **MVX2U Gen 2, MV6 and MV6 Gen 2:** loading a preset onto a device with Gain Lock on now
  applies the preset's gain. The device ignored the gain write before.
- **MV6, MV6 Gen 2 and MV7+:** loading a preset that has no Playback Level saved (one from
  another model or an older version) no longer silences headphone playback.
- After a Factory Reset, the status bar says the device is restarting and will reconnect on
  its own, instead of asking you to restart shurectl.

## 2.9.0 — 2026-10-09

### Added

- **MV6 Gen 2:** full support, with every setting MOTIV offers:
  - Gain, mute, Gain Lock, high-pass filter, Denoiser, Popper Stopper, mute button and
    Tone, as on the MV6
  - Auto Level settings: Target Level (Low / Med / High) and Adaptation Rate (Slow / Fast)
  - Denoiser strength from 1 to 100%
  - A Simple / Advanced Tone Mode on the EQ tab: Simple shows the Tone slider, Advanced a
    5-band EQ in 2 dB steps that `f` flattens
  - Mic Presets (Speech / Singing / Instrument) on the Main tab
  - Factory Reset on the Info tab
  - Everything is read from the device and saved in presets. Older presets load the
    defaults (Slow, 50%, Simple).
- `--demo mv6-gen2` to try the MV6 Gen 2 screens without the device.
- **MV6, MV6 Gen 2 and MV7+:** Mic Level and Playback Level as two separate sliders, as in
  MOTIV. The MV7+ used to show both as one mic/playback balance.

### Fixed

- EQ band changes now use the right encoding for each model.

## 2.8.1 — 2026-10-06

### Added

- Early MV6 Gen 2 support: it is recognised and works with the MV6's controls. Its new
  controls arrived in 2.9.0.

## 2.8.0 — 2026-10-05

### Added

- Rename your device: on the Info tab, press `Enter` on Device Name, type a name (1–31
  characters), and press `Enter` to save or `Esc` to cancel. The new name shows only once
  the device accepts it. Available on the MVX2U Gen 1, MVX2U Gen 2, MV6 and MV7+; the MV7
  has no device name.
- `shurectl --mute status` prints "on" or "off" without changing anything, for status-bar
  mic indicators.
- `shurectl --preset N` loads preset slot 1–4 onto the device without opening the TUI, for
  hotkeys and Stream Deck buttons.

## 2.7.0 — 2026-09-28

### Added

- Mouse support on every model:
  - Click a tab to switch to it. Locked tabs ignore clicks.
  - Click a control to select it, and click it again to toggle or cycle it.
  - Scroll over a slider or an EQ band to adjust it one step at a time. Fast trackpad
    scrolling no longer lags behind your gesture.
  - Preset Load, Save and Delete need a second click on the same button, so a stray click
    never deletes a preset.
  - Factory Reset still needs `Enter` to confirm; any click cancels it.
  - Only clicks and scrolling are captured. Most terminals let you hold `Shift` to select
    text.

### Changed

- `Esc` now cancels a preset rename and throws the new name away (it used to save it).
  `Enter` saves, and clicking outside the name field also cancels.

## 2.6.2 — 2026-09-27

### Added

- The header shows `[CONNECTED]` or `[DISCONNECTED]`. shurectl checks for the device every
  2 seconds and reconnects on its own after you unplug it and plug it back in.
- `r` refreshes, and reconnects straight away after a replug.

### Fixed

- Refresh reports an error when the device doesn't answer, instead of showing defaults
  with "State refreshed from device".
- A malformed reply from the device no longer crashes shurectl. If shurectl does crash, the
  terminal is restored instead of being left in a broken state.
- Unrecognised device replies are shown in the status bar instead of garbling the screen.
- Preset names can contain the letter `q`.
- `Shift+Tab` moves to the previous tab.
- `f` (Flatten EQ) does nothing on the MVX2U Gen 2 in Auto Level mode, where the EQ bands
  are hidden. It used to zero them.
- Gain Lock in a preset applies only on models that have it, so it no longer freezes gain
  on the MVX2U Gen 1, MV7 or MV7+.
- Out-of-range values in a hand-edited preset are clamped when it loads.
- EQ gains in a preset snap to the loading model's step size.
- After a refresh, reconnect or preset load, focus moves off controls that are now hidden
  or locked, so the arrow keys can't change them.
- **MV7+:** switching to Manual focuses Gain, as on every other model.

## 2.6.0 — 2026-09-27

### Added

- **MV7 (original):** support for mode, mute, gain (1.5 dB steps), monitor mix, Auto Level
  mic position and tone, Config Lock, EQ presets (Flat / High Pass / Presence Boost /
  both), compressor, and the Live Meter and Night Mode LEDs. EQ and Dynamics are locked in
  Auto Level, as on the MVX2U Gen 1.
- `--demo mv7` to try the MV7 screens without the device.

### Fixed

- Factory Reset is offered only on the MV7+. Before, pressing `Enter` twice on the Info tab
  could send the reset to other models with no button on screen.
- `f` (Flatten EQ) works only on models with EQ bands. It used to send EQ band changes to
  the MV6 and MV7+.
- A gain is rounded to the model's step size before it's sent, so a preset from a model
  with a higher maximum (60 dB on an MVX2U) loads as 36 dB on an MV6 and shows that.
- **MVX2U Gen 2:** settings no longer read back as defaults after loading a preset.
- **MVX2U Gen 2:** loading a preset is faster.

## 2.5.4 — 2026-09-06

No user-facing changes.

## 2.5.3 — 2026-09-03

### Changed

- **MV7+:** Reverb hints read "[Enter] to toggle" and "[Enter] to cycle", matching the
  Dynamics tab.

## 2.5.2 — 2026-08-28

Dependency updates. No user-facing changes.

## 2.5.1 — 2026-08-03

### Added

- Install with Homebrew: `brew install humblemonk/shurectl/shurectl`.

### Changed

- `cargo install` no longer puts the maintainer-only `probe` tool on your PATH.

## 2.5.0 — 2026-06-24

### Added

- The Info tab shows the device name, factory serial number and firmware version, when the
  device reports them.
- The header shows your device's name, or the model name if it has none, followed by its
  serial number.
- `--list` shows each device's factory serial number.

## 2.4.0 — 2026-06-22

### Added

- Windows support.
- `shurectl --mute` toggles mute without opening the TUI; `--mute on` and `--mute off` set
  it. Combine with `--device` to pick a device. Handy for hotkeys and Stream Deck buttons.

## 2.3.0 — 2026-05-09

### Added

- **MV7+:** support for gain, mode, mute, monitor mix and playback mix, high-pass filter,
  limiter, compressor, Denoiser, Popper Stopper, Tone, Factory Reset, plus:
  - A Reverb tab: on/off for the output and the monitor, type (Plate / Hall / Studio) and
    intensity
  - An LED tab: behavior (Live / Pulsing / Solid), brightness, themes and custom colors
  - LED settings are saved in presets

### Changed

- The Reverb and LED tabs appear only on the MV7+.

## 2.2.1 — 2026-05-01

### Added

- `--demo` takes a model name (`mvx2u`, `mvx2u-gen2`, `mv6`), and the status bar shows which
  model is being demoed.

## 2.2.0 — 2026-04-22

### Added

- **MVX2U Gen 2:** support for gain (0–60 dB), Gain Lock, monitor mix, 48V phantom power,
  a 5-band EQ in 0.5 dB steps, Tone (in Auto Level), Denoiser, Popper Stopper, high-pass
  filter, limiter and compressor. Unlike the Gen 1, its EQ and Dynamics tabs stay available
  in Auto Level.
- `f` on the EQ tab flattens the 5-band EQ (MVX2U Gen 1 and Gen 2).

### Changed

- EQ gains show one decimal place (for example, "+0.5 dB").

### Upgrading

- EQ gains in presets saved before 2.2.0 load at a tenth of their value (a saved -4 dB
  loads as -0.4 dB). Load each preset, set the EQ again, and save it.

## 2.1.0 — 2026-04-21

### Added

- **MV6:** a Monitor Mix slider on the Main tab, in both modes.

### Changed

- **MV6:** the mute button setting is read from the device instead of being stored by
  shurectl.

### Upgrading

- `~/.config/shurectl/mv6_state.toml` is no longer used and can be deleted.

## 2.0.0 — 2026-04-17

### Added

- **MV6:** support for gain (0–36 dB) and Auto Level, Tone from dark to bright, Denoiser,
  Popper Stopper, a setting to disable the physical mute button, and a high-pass filter
  (Off / 75 Hz / 150 Hz). shurectl detects which model is plugged in, and `--list` shows
  both.

### Changed

- Presets are saved safely: an interrupted save can no longer leave a half-written file.

## 1.3.1 — 2026-04-03

### Added

- macOS support.

## 1.3.0 — 2026-04-01

### Added

- `--device <PATH>` (`-D`) picks a specific device when more than one is plugged in. `--list`
  shows the paths.

## 1.2.0 — 2026-03-18

### Added

- `-l` as a short form of `--list`, and `-V` / `--version` to print the version.

### Changed

- **Linux:** the udev rule now grants access to whoever is logged in, with no group
  membership or logout needed. Reinstall the rule from the readme to switch to it.

## 1.1.0 — 2026-03-14

### Added

- A Presets tab with 4 named slots, saved as TOML files you can edit by hand. `s` saves the
  current settings into a slot, `Enter` loads it, and `d` / `Delete` removes it.

## 1.0.0 — 2026-03-14

First release, for the MVX2U Gen 1 on Linux:

- Gain (0–60 dB), Auto Level / Manual mode, and the Auto Level mic position, tone and gain
- Mute, 48V phantom power (with a ribbon-mic warning), Config Lock and monitor mix
- 5-band EQ with per-band on/off, limiter, compressor and high-pass filter
- A live level meter with peak hold, an Info tab, and `--demo` to try it without a device
