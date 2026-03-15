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
- **Device Info** — firmware version, serial number
- **Demo mode** — run without a device plugged in (`--demo`)

All settings are sent to the device in real-time as you adjust them.
Settings persist on the device after disconnect (no host software needed after configuration).

---

## Requirements

- Linux (kernel ≥ 4.0)
- Rust ≥ 1.85 (`rustup` recommended, see below)
- `libhidapi-dev` and `libudev-dev`

```bash
sudo apt install libhidapi-dev libudev-dev   # Debian/Ubuntu
sudo pacman -S hidapi                        # Arch/CachyOS (no extra dev pkg needed)
sudo dnf install hidapi-devel systemd-devel  # Fedora
```

---

## udev Rule (Required for Non-Root Access)

Without a udev rule, `/dev/hidrawN` for the MVX2U is only accessible by root.

Create `/etc/udev/rules.d/99-mvx2u.rules` with the content for your distro:

**Arch Linux** (uses the `input` group, which exists by default):
```
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="14ed", ATTRS{idProduct}=="1013", MODE="0660", GROUP="input"
```

**Debian / Ubuntu** (uses the `plugdev` group):
```
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="14ed", ATTRS{idProduct}=="1013", MODE="0660", GROUP="plugdev"
```

Then reload udev and replug your device:

```bash
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Add your user to the appropriate group:

```bash
# Arch
sudo usermod -aG input $USER

# Debian / Ubuntu
sudo usermod -aG plugdev $USER
```

Log out and back in for the group change to take effect, or apply it to your current shell immediately:

```bash
newgrp input    # Arch
newgrp plugdev  # Debian / Ubuntu
```

Verify the device appears:

```bash
shurectl --list
# Found 1 MVX2U device(s):
#   /dev/hidraw2 | S/N: MVX2U-XXXXXXXX
```

---

## Building

```bash
git clone <repo-url>
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

---

## Usage

```bash
shurectl              # Connect to device and launch TUI
shurectl --demo       # Run without a device (explore the UI)
shurectl --list       # List detected MVX2U devices and exit
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

### Protocol Notes

The MVX2U uses plain USB HID Output/Input Reports for device configuration. The protocol
was reverse-engineered by [PennRobotics](https://gitlab.com/PennRobotics/shux)
and is publicly documented (Apache 2.0). Key details:

- **USB IDs**: VID `0x14ED`, PID `0x1013`
- **Interface**: HID interface (accessed via `/dev/hidrawN`, not the USB audio interface)
- **Packet size**: 64 bytes
- **Header**: `0x11 0x22` (fixed magic bytes, never change)
- **Checksum**: CRC-16/ANSI — poly `0x8005`, init `0x0000`, reflected input and output (not CCITT-FALSE)
- **Report ID**: `0x01` (first byte of every packet; required by hidapi)
- **Transport**: plain HID Output Reports via `hid_write()` for commands; Input Reports via `hid_read()` for responses. Not Feature Reports — `HIDIOCSFEATURE`/`HIDIOCGFEATURE` are not used.

Packet layout:
```
[0x01] [0x11] [0x22] [seq] [0x03] [0x08] [data_len] [0x70] [data_len] [cmd0][cmd1][cmd2] [feat_addr...] [value...] [crc_hi] [crc_lo] [0x00 padding...]
  ↑─── Report ID ────↑                                                   ↑──────── CRC covers from 0x11 onward (excluding CRC bytes themselves) ─────────↑
```

Every SET command must be followed immediately by a CONFIRM packet; the device will not apply
the change without it. GET commands receive one response packet on the next `hid_read()`.

State is read back by issuing individual GET packets for each feature (not a single bulk
GET_STATE). Each response is dispatched through `apply_response()` in `protocol.rs`, which
decodes the 2-byte feature address and writes the value into the appropriate `DeviceState` field.

All command byte values and response field offsets are documented inline in
`src/protocol.rs`. If a command doesn't behave as expected on your firmware
version, capture packets with `usbmon` + Wireshark while using MOTIV Desktop
on Windows/Mac and compare to the byte sequences in `protocol.rs`.

### Capturing Packets for Protocol Verification

```bash
# Load the usbmon kernel module
sudo modprobe usbmon

# Find the bus number for the MVX2U
lsusb | grep -i shure   # note the bus number

# Capture on that bus with Wireshark
sudo wireshark -i usbmon2   # replace '2' with your bus number

# Or with tcpdump
sudo tcpdump -i usbmon2 -w mvx2u.pcap
```

Filter for HID output and interrupt transfers in Wireshark: `usb.transfer_type == 0x01` (interrupt) for responses, `usb.transfer_type == 0x03` (bulk/interrupt OUT) for commands. Both endpoints are on the HID interface, not the audio interface.

---

## Troubleshooting

**"Cannot open MVX2U"** — udev rule not installed, not in the correct group (`input` on Arch, `plugdev` on Debian/Ubuntu), or device not plugged in.
Run `shurectl --list` to check detection. Try `sudo shurectl` to confirm it's a permissions issue.

**Settings don't seem to apply** — Every SET command must be followed by a CONFIRM packet, which `device.rs` handles automatically via `send_set()`. If you've patched `device.rs`, ensure `send_set()` is still called rather than `write()` directly. Use usbmon to confirm two Output Report transfers appear on the wire for each setting change.

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

This project was developed with the assistance of Claude
(Anthropic). Claude acted as a pair-programmer throughout: writing and reviewing
Rust code, reasoning about the HID protocol, and catching issues during implementation.
All code was reviewed and tested by the author before merging.

---

## Legal

Protocol implementation is based on publicly documented USB HID packet captures
by PennRobotics (shux project, Apache 2.0). No Shure software was used, decompiled,
or examined in the creation of this tool.

shurectl is not affiliated with or endorsed by Shure Incorporated.
