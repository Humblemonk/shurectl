# shurectl

An open-source terminal UI for configuring Shure USB microphones and audio interfaces on Linux,
macOS, and Windows. A lightweight alternative to the ShurePlus MOTIV desktop app.

![Project Example Screenshot](images/shurectl.png)

---

## Supported Devices
- MVX2U Gen 1 — Digital Audio Interface
- MVX2U Gen 2 — Digital Audio Interface
- MV6 — USB Gaming Microphone
- MV7 — USB/XLR Dynamic Microphone (original)
- MV7+ — USB/XLR Dynamic Microphone

---

## Features

On every supported device:

- **Real-time Level Meter** — dBFS input meter with peak-hold display
- **4 Preset Slots** — save and load named presets stored as TOML files (see [Presets](#presets))
- **Device Info** — factory serial number, device name, and firmware version
- **Command-line mute** — toggle mute without opening the TUI (`--mute`)
- **Demo mode** — explore the UI without a device plugged in (`--demo`)

Device controls by model:

✓ = available · **Manual** / **Auto** = only in that mode · — = not on this device

| Control | MVX2U Gen 1 | MVX2U Gen 2 | MV6 | MV7 | MV7+ |
|---------|:-----------:|:-----------:|:---:|:---:|:----:|
| **Main tab** | | | | | |
| Mode (Auto Level / Manual) | ✓ | ✓ | ✓ | ✓ | ✓ |
| Mute | ✓ | ✓ | ✓ | ✓ | ✓ |
| Gain | Manual, 0–60 dB | Manual, 0–60 dB | Manual, 0–36 dB | Manual, 0–36 dB | Manual, 0–36 dB |
| Gain Lock | — | Manual | Manual | — | — |
| Monitor Mix (mic ↔ playback, 0–100%) | ✓ | ✓ | ✓ | ✓ | ✓ |
| Playback Mix (second mix, independent of Monitor Mix) | — | — | — | — | ✓ |
| Phantom Power (48V) | ✓ | ✓ | — | — | — |
| Config Lock | ✓ | — | — | ✓ | — |
| Mic Position (Near / Far) | Auto | — | — | Auto | — |
| Auto Tone (Dark / Natural / Bright) | Auto | — | — | Auto | — |
| Gain Environment (Quiet / Normal / Loud) | Auto | — | — | — | — |
| **EQ tab** | | | | | |
| 5-band Parametric EQ (−8 to +6 dB) | Manual, 2 dB steps | Manual, 0.5 dB steps | — | — | — |
| Tone (Dark ↔ Natural ↔ Bright slider, 10% steps) | — | Auto | ✓ | — | ✓ |
| EQ Preset (Flat / High Pass / Presence Boost / both) | — | — | — | Manual | — |
| **Dynamics tab** | | | | | |
| Limiter | Manual | Manual | — | — | ✓ |
| Compressor (Off / Light / Medium / Heavy) | Manual | Manual | — | Manual | ✓ |
| High-Pass Filter (Off / 75 Hz / 150 Hz) | Manual | ✓ | ✓ | — | ✓ |
| Denoiser | — | ✓ | ✓ | — | ✓ |
| Popper Stopper | — | ✓ | ✓ | — | ✓ |
| Mute Button Disable | — | — | ✓ | — | ✓ |
| **Other tabs** | | | | | |
| Reverb (Plate / Hall / Studio, intensity 0–100%, output and monitor on/off) | — | — | — | — | ✓ |
| LED Panel | — | — | — | Live Meter, Night Mode | Behavior, Brightness, Theme, custom RGB |
| Factory Reset (Info tab) | — | — | — | — | ✓ |

Notes:

- Gain moves in 1 dB steps, except on the MV7, which uses the hardware's own 1.5 dB step.
- On the MVX2U Gen 1 and MV7, the device manages EQ and Dynamics itself in Auto Level
  mode, so those tabs are locked until you switch to Manual.
- On the MVX2U Gen 1, the EQ also has a master enable and a per-band enable.
- The limiter MOTIV shows for the MV7 is processing inside the MOTIV app, not a mic
  setting, so shurectl does not offer it.
- Factory Reset asks for confirmation, and the mic disconnects afterwards.

---

## Installing

### Via the AUR (Arch Linux)

```bash
paru -S shurectl        # or: yay -S shurectl
```

The package installs the udev rules to `/usr/lib/udev/rules.d/62-shure.rules` for you, so
the [Linux setup step](#linux--udev-rules-required-for-non-root-access) below can be
skipped — **replug the device after installing** so udev applies the new rules to it.

### Via Homebrew (macOS / Linux)

```bash
brew install humblemonk/shurectl/shurectl
```

Updates arrive through `brew upgrade` like any other formula. The formula builds from
source, so the first install pulls in a Rust toolchain and takes a minute or two. On Linux,
Homebrew can't install system udev rules, so you still need the
[Linux setup step](#linux--udev-rules-required-for-non-root-access) below.

### Via cargo install

```bash
cargo install --git https://github.com/Humblemonk/shurectl.git
```

### From source

```bash
git clone https://github.com/Humblemonk/shurectl.git
cd shurectl
cargo build --release
```

The binary will be at `target/release/shurectl`.

To install system-wide:

```bash
sudo install -m 755 target/release/shurectl /usr/local/bin/
```

Or for your user only:

```bash
install -m 755 target/release/shurectl ~/.local/bin/
```

On Windows, building from source requires the MSVC toolchain. Install
[Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
with the "Desktop development with C++" workload, then ensure MSVC is your active rustup
toolchain:

```
rustup default stable-x86_64-pc-windows-msvc
```

---

## Platform Setup

### Linux — udev Rules (Required for Non-Root Access)

Skip this section if you installed via the AUR package — it ships these rules already.

Without a udev rule, `/dev/hidrawN` for the device is only accessible by root.

Create `/etc/udev/rules.d/62-shure.rules`:

```
ACTION!="remove", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="14ed", ATTRS{idProduct}=="1013", TAG+="uaccess"
ACTION!="remove", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="14ed", ATTRS{idProduct}=="1033", TAG+="uaccess"
ACTION!="remove", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="14ed", ATTRS{idProduct}=="1026", TAG+="uaccess"
ACTION!="remove", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="14ed", ATTRS{idProduct}=="1012", TAG+="uaccess"
ACTION!="remove", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="14ed", ATTRS{idProduct}=="1019", TAG+="uaccess"
```

Then reload udev and replug your device:

```bash
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Verify the device appears:

```bash
shurectl --list
# Found 1 Shure device(s):
#   /dev/hidraw2 | Shure MVX2U Gen 2 | S/N: MVX2U GEN 2#2-a646351d...
```

### macOS

On macOS, IOKit grants user-space access to HID devices without extra configuration.
Plug in your device and run `shurectl --list` to confirm detection.

### Windows

Windows grants user-space HID access out of the box via the `setupapi` backend — no driver installation or equivalent of a udev rule is needed. Plug in your device and run `shurectl --list` to confirm detection. Device paths look like `\\?\HID#VID_14ED&PID_1026&...` rather than `/dev/hidrawN`.

---

## Usage

```bash
shurectl                         # Connect to first detected device and launch TUI
shurectl --device <path>         # Connect to a specific device (use --list to find paths)
shurectl --demo                  # Run without a device (simulates an MVX2U Gen 1)
shurectl --demo mv7              # Demo a specific model: mvx2u, mvx2u-gen2, mv6, mv7, mv7plus
shurectl --list                  # List detected Shure devices and exit
shurectl --mute                  # Toggle mute without launching the TUI
shurectl --mute on               # Mute
shurectl --mute off              # Unmute
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `Tab` / `Shift+Tab` | Switch section |
| `↑` / `k` | Focus previous control |
| `↓` / `j` | Focus next control |
| `←` / `h` | Decrease value |
| `→` / `l` | Increase value |
| `Enter` / `Space` | Toggle boolean / cycle option |
| `f` | Flatten EQ (zero all bands) — EQ tab, MVX2U Gen 1 and Gen 2 only |
| `r` | Refresh state from device |
| `s` | Save preset (on Presets tab, focused slot) |
| `d` / `Delete` | Delete preset (on Presets tab, actions row of the focused slot) |
| `?` | Toggle help overlay |
| `q` / `Ctrl+C` | Quit |

---

## Presets

Presets are stored as human-readable TOML files in a `shurectl/presets/` folder under your
platform's config directory:

| Platform | Location |
|----------|----------|
| Linux | `~/.config/shurectl/presets/` |
| macOS | `~/Library/Application Support/shurectl/presets/` |
| Windows | `%APPDATA%\shurectl\presets\` |

Each slot is its own file, `preset_1.toml` through `preset_4.toml`. A file captures all
configurable DSP settings (gain, mode, EQ, dynamics, monitor mix, etc.) but not
hardware-identity fields like serial number or firmware version. Files are hand-editable.

On the **Presets tab**:
- Navigate to a slot with `↑`/`↓`
- Press `Enter` on the name field of a saved preset to rename it (type, then `Enter` or `Esc` to finish)
- Press `Enter` on the actions row to load a filled preset — all settings are applied to the device immediately
- Press `s` to save the current device state into the focused slot
- Press `d` on the actions row to delete the focused slot

---

## Troubleshooting

**"Cannot open device"** — device not found or a permissions issue.
Run `shurectl --list` to check detection. On Linux, try `sudo shurectl` to confirm it's a udev permissions issue. On macOS and Windows, ensure no other software (e.g. ShurePlus MOTIV) has exclusive access to the device.

**No gain slider in Auto Level mode** — This is correct hardware behaviour; the device
sets its own gain in Auto Level mode, so the slider is hidden. Switch to Manual mode first.

**Gain won't change** — On the MVX2U Gen 2 and MV6, check that Gain Lock is off.

**MV7: MOTIV stops showing the mic's settings** — The MV7 has a single command channel
that MOTIV and shurectl share. Quit MOTIV before using shurectl with an MV7; if MOTIV
already lost the mic, quit it, replug the mic, and reopen it.

**PipeWire/PulseAudio volume vs. device gain** — This tool controls the **hardware DSP gain**
on the device itself, not the OS capture volume level. Both can be set independently.

---

## Acknowledgements

Initial protocol reverse-engineering credit goes to **PennRobotics** and the
[shux project](https://gitlab.com/PennRobotics/shux) (Apache 2.0), without which
this tool would not exist. If you find shurectl useful, consider starring their
repository.

---

## Legal

Protocol implementation is based on publicly documented USB HID packet captures
by PennRobotics (shux project, Apache 2.0) as well as author's own usbmon captures.

shurectl does not update device firmware. The firmware-update commands are intentionally
left out of its protocol implementation, and it never sends them. To update firmware, use
ShurePlus MOTIV; the Info tab shows the firmware version your device is running.

shurectl is not affiliated with or endorsed by Shure Incorporated.
