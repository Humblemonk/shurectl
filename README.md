# shurectl

An open-source terminal UI configurator for the **Shure MVX2U** XLR-to-USB audio
interface on Linux. Replaces the Windows/Mac-only ShurePlus MOTIV Desktop app.

![Project Example Screenshot](images/shurectl.png)

## Features

- **Gain** — 0–60 dB with live gauge display
- **Input Mode** — Auto Level / Manual toggle
- **Mic Mute** — toggle with ribbon-mic phantom power warning
- **Phantom Power** — 48V on/off
- **Monitor Mix** — mic vs. playback blend slider
- **5-band Parametric EQ** — per-band enable, gain (−8 to +6 dB in 2 dB steps)
- **Limiter** — enable/disable
- **Compressor** — Off / Light / Medium / Heavy
- **High-Pass Filter** — Off / 75 Hz / 150 Hz
- **Panel Lock** — lock the physical panel controls on the device
- **Auto Level controls** — mic position (Near/Far), tone (Dark/Natural/Bright), gain environment (Quiet/Normal/Loud)
- **4 Preset Slots** — save and load named presets stored as TOML in `~/.config/shurectl/presets/`
- **Real-time Level Meter** — dBFS input meter with peak-hold display
- **Device Info** — serial number
- **Demo mode** — run without a device plugged in (`--demo`)

All settings are sent to the device in real-time as you adjust them.
Settings persist on the device after disconnect (no host software needed after configuration).

---

## udev Rule (Required for Non-Root Access)

Without a udev rule, `/dev/hidrawN` for the MVX2U is only accessible by root.

Create `/etc/udev/rules.d/62-mvx2u.rules`:

```
ACTION!="remove", SUBSYSTEMS=="hidraw", ATTRS{idVendor}=="14ed", ATTRS{idProduct}=="1013", TAG+="uaccess"
```
Then reload udev and replug your device:

```bash
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Verify the device appears:

```bash
shurectl --list
# Found 1 MVX2U device(s):
#   /dev/hidraw2 | S/N: MVX2U-XXXXXXXX

# With multiple devices:
# Found 2 MVX2U device(s):
#   /dev/hidraw3 | S/N: MVX2U#3-7d84d19...
#   /dev/hidraw2 | S/N: MVX2U#3-17b7a6c...
```

---

## Installing

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

### Via cargo install

```bash
cargo install --git https://github.com/Humblemonk/shurectl.git
```

---

## Usage

```bash
shurectl                        # Connect to first detected device and launch TUI
shurectl --device /dev/hidraw3  # Connect to a specific device (use --list to find paths)
shurectl --demo                 # Run without a device (explore the UI)
shurectl --list                 # List detected MVX2U devices and exit
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
| `r` | Refresh state from device |
| `s` | Save preset (on Presets tab, focused slot) |
| `d` | Delete preset (on Presets tab, focused slot) |
| `?` | Toggle help overlay |
| `q` / `Ctrl+C` | Quit |

---

## Presets

Presets are stored as human-readable TOML files in `~/.config/shurectl/presets/`:

```
~/.config/shurectl/presets/
├── preset_1.toml
├── preset_2.toml
├── preset_3.toml
└── preset_4.toml
```

Each file captures all configurable DSP settings (gain, mode, EQ, dynamics, monitor mix, etc.)
but not hardware-identity fields like serial number or firmware version. Files are hand-editable.

On the **Presets tab**:
- Navigate to a slot with `↑`/`↓`
- Press `Enter` on the name field to rename it (type, then `Enter` to confirm or `Esc` to cancel)
- Press `Enter` on the actions row to load a filled preset — all settings are applied to the device immediately
- Press `s` to save the current device state into the focused slot
- Press `d` to delete the focused slot

---

## Architecture

```
src/
├── main.rs       — Entry point, event loop, CLI args
├── app.rs        — Application state, focus/tab navigation, DeviceAction events
├── device.rs     — hidapi wrapper; open/send/receive for MVX2U
├── meter.rs      — cpal audio capture; real-time dBFS metering, RollingWindow, PeakWindow
├── presets.rs    — Host-side preset storage: TOML serialisation, load/save/delete, PresetSlot
├── protocol.rs   — USB HID packet encoding, CRC-16/ANSI, command constructors, apply_response()
└── ui.rs         — ratatui TUI rendering (all 5 tabs + help overlay)
```

All command byte values, feature addresses, and packet structure details are documented inline in `src/protocol.rs`.

---

## Troubleshooting

**"Cannot open MVX2U"** — udev rule not installed, or device not plugged in.
Run `shurectl --list` to check detection. Try `sudo shurectl` to confirm it's a permissions issue.

**Gain slider is greyed out in Auto Level mode** — This is correct hardware behaviour;
the device ignores gain commands in Auto Level mode. Switch to Manual mode first.

**PipeWire/PulseAudio volume vs. device gain** — This tool controls the **hardware DSP gain**
on the MVX2U itself, not the OS capture volume level. Both can be set independently.

---

## Acknowledgements

Protocol reverse-engineering credit goes to **PennRobotics** and the
[shux project](https://gitlab.com/PennRobotics/shux) (Apache 2.0), without which
this tool would not exist. If you find shurectl useful, consider starring their
repository.

This project was developed with the assistance of Claude (Anthropic) as a pair-programmer
throughout: writing and reviewing Rust code, reasoning about the HID protocol, and catching
issues during implementation. All code was reviewed and tested by the author before merging.

---

## Legal

Protocol implementation is based on publicly documented USB HID packet captures
by PennRobotics (shux project, Apache 2.0). No Shure software was used, decompiled,
or examined in the creation of this tool.

shurectl is not affiliated with or endorsed by Shure Incorporated.
