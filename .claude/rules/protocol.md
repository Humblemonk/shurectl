---
paths:
  - "src/protocol.rs"
  - "src/protocol/*.rs"
  - "src/device.rs"
  - "src/bin/probe.rs"
---

# USB HID Protocol

Every packet is exactly 64 bytes, report ID `0x01` included, sent as hidapi output reports
(`write()`/`read()`; `/dev/hidrawN` on Linux). Not the USB audio class interface, and not
`HIDIOCSFEATURE`/`HIDIOCGFEATURE`. The full layout is the module doc at the top of
`protocol.rs`; `build_packet()` and `build_packet_hdr0()` frame every binary packet.

```
[0x01] [len] [0x11 0x22] [seq] [hdr] [0x08] [dlen] [0x70] [dlen] [cmd0..cmd2] [prefix] [feat_addr..] [value..] [crc_hi crc_lo] [pad..]
 ^report ID   ^magic             ^HDR_CONSTANT 0x03; 0x00 via build_packet_hdr0()   ^byte 13 (PREFIX_OFFSET)
CRC-16/ANSI covers 0x11 up to the CRC
```

- **USB IDs:** VID `0x14ED`; PID `0x1013` (MVX2U Gen 1), `0x1033` (MVX2U Gen 2), `0x1026`
  (MV6), `0x1035` (MV6 Gen 2), `0x1012` (MV7), `0x1019` (MV7+)
- **Header constant:** `0x00` for the MV7+ SET class and the commands other models share
  with it (Mic/Playback Level, mute button, factory reset); `0x03` for everything else
- **CRC:** CRC-16/ANSI: poly `0x8005`, init `0x0000`, reflected in/out (NOT CCITT-FALSE)
- **SET + CONFIRM:** every SET must be followed immediately by a `CMD_CONFIRM` packet, or the
  device won't apply the change
- **Acks after CONFIRM:** the MV7+ and MVX2U Gen 2 each send two unsolicited input reports
  after a CONFIRM. Unread, they shift every later GET by one packet and refresh shows stale or
  default values. `DeviceModel::reports_after_confirm()` sets how many `send_set()` discards.
  The MVX2U Gen 1, MV6 and MV6 Gen 2 are assumed to send two as well, unchecked on hardware.
  Some settings send only one (Gen 2 mute), which costs one `ACK_TIMEOUT_MS` wait. That
  timeout is 15 ms on every model, measured on the Gen 2 (acks in ≤3 ms). On the MV7+ (a
  MOTIV capture on firmware 1.6, mike-callahan) the two are the CONFIRM ack and a SET echo
  carrying the value, in either order; LED-page SETs sometimes send the ack alone. The echo
  comes only when the SET changes the value (every changing SET got one, no same-value SET
  did; re-count by mike-callahan, 2026-10-31), so a same-value SET costs one harmless ack
  timeout. Ack and echo came 9–19 ms after the CONFIRM, so the 15 ms wait can leave one
  queued; every readback and live poll drains queued input first. For stale values after a
  preset load or refresh, change a value with SET + CONFIRM and time and count the reports
  that arrive: too few acks is harmless, too slow or too many is the bug
- **State readback:** there is no monolithic GET_STATE. `device.rs::get_state()` issues
  individual `cmd_get_*` packets; `apply_response()` dispatches on the 2-byte feature address
  and writes into `DeviceState`. The address → field mapping is documented inline in
  `protocol.rs`. `apply_reply()` tells Playback Mix (FEAT_MIX, prefix 0x03) from Monitor Mix
  by the reply's prefix byte (`buf[13]`). That echo is confirmed on the MV6 Gen 2 and MVX2U
  Gen 2 (capture of MOTIV Mix) and on the MV7+ (MOTIV capture on firmware 1.6: all 106
  replies at FEAT_MIX echo their request's prefix, among them a Playback Level of 99 read
  while the Mic Level was 100). It is unchecked on the MV6 and MVX2U Gen 1, so don't route
  anything else by prefix without a capture
- **MOTIV subscribes, shurectl polls:** on the MV7+ capture, MOTIV first sends a `07 02 0x`
  packet per feature (acked `0c 02 0x`), then never polls. MOTIV never sends a HID SET for
  the MV7+'s mute or gain: it changes them through the USB Audio class controls on EP0, and
  the subscribed mic then reports the change as a `04 02 02` push at prefix 0x00 (re-read of
  the captures by mike-callahan, 2026-10-31). Whether it pushes without that subscription
  is unknown, and so is whether the OS's own input volume and mute (the same class controls)
  move the gain and mute the HID GETs read. shurectl doesn't subscribe, and
  `send_get_past_strays()` passes over such a report anyway. Don't send `07 02 0x` without a
  capture of what it changes
- **MV7+ connect handshake, unknown:** at connect, before anything else, MOTIV reads three
  lock-class prefix 0x03 values (`00 10`, `00 20`, `00 02`) and sends one nonstandard packet
  (byte 6 = `2d`, byte 8 = `00`; payload `00 00 01`, 16 bytes from MOTIV, then the mic's
  `00 10` value), which the mic doesn't answer. In every session's capture (mike-callahan,
  2026-10-31). shurectl works without it; don't send it
- **Live poll per model** (the design is under Live poll in `AGENTS.md`): the binary models
  drain queued input before each poll. In Auto a gain change is kept but not announced.
  After any mode switch the MV7 re-reads everything; after a switch to Manual the MVX2U
  Gen 1 and MV6, whose gain read in Auto is unconfirmed, re-read the gain
  (`resync_after_mode()`). On the MV7 an unanswered `micMute` skips the poll's other lines.
  The MV6's unconfirmed Playback Level GET (#99) isn't polled. A TUI write pushes the poll
  back a second because whether a device applies a write before acking it is unconfirmed
- **Live poll on hardware** (`read_live()`): on
  the MV6 Gen 2 under Linux, a pressed mute button sends no input report, and the poll shows
  the press (tested 2026-10-23). On the MVX2U Gen 2, gain, Monitor Mix, mode and mute changed
  from the command line showed in a running TUI, and the next arrow key stepped from the new
  gain (tested 2026-10-24). On the MV6 Gen 2, the same held for gain and mute, and a button
  press still showed (tested 2026-10-25). On the MV7+, a gain change from the command line
  showed within about a second (reported by mike-callahan, 2026-10-27), and so did muting and
  unmuting with a tap on the touch panel; Mic Level 40 and Playback Level 30 set from the
  command line read back as such (firmware 1.6.0.1 on Debian, reported by mike-callahan,
  2026-10-31). The MV7+'s touch panel has no gain slide (checked in MOTIV on macOS, same
  report); a probe of page 0x01 before and after a swipe differed only in mute. On the MVX2U
  Gen 2, switching to Auto, talking, then back to Manual and pressing → raised the gain 1 dB
  from its Manual value, so the gain GET in Auto reads the Manual gain, not a moving auto
  gain (tested 2026-10-28); the same on the MV6 Gen 2 (tested 2026-10-31) and the MV7+
  (`--json` in Auto after talking read the Manual 20 dB, firmware 1.6.0.1, reported by
  mike-callahan, 2026-10-31). A gain SET in Auto is kept as the Manual gain: on the MVX2U
  Gen 2 a preset saved in Auto at 40 dB, loaded in Auto over a Manual gain of 25 dB, left
  40 dB after a switch to Manual (tested 2026-10-31), and the same on the MV6 Gen 2
  (tested 2026-10-31); unconfirmed on the MVX2U Gen 1, MV6 and MV7+.
  Unconfirmed elsewhere: whether the MVX2U Gen 1 and MV6
  report a press, whether their GETs reflect changes made elsewhere, whether the gain GET in
  Auto reads the Manual gain on the MVX2U Gen 1 and MV6 (the MV7 re-reads everything after a
  mode change), and whether the MV7 answers each line within the poll's 200 ms
- `hidapi` uses the `linux-native` feature (`/dev/hidrawN` access).

## Original MV7: text shell, not binary packets

The MV7 (PID `0x1012`) ignores everything above. Its vendor interface takes report ID `0x00`
plus one ASCII line ending in `\n` (`micMute on`) and answers with a line (`micMute=on`).
DSP settings are hex-addressed blocks (`setBlock 19 00000001`). All of it lives in
`protocol::mv7_text` (`src/protocol/mv7_text.rs`); `device.rs` sends it via `send_text()`,
and the binary `send_set()`/`send_get_past_strays()` refuse to run on the MV7.

- `help` and `help <cmd>` on the device list commands and their syntax. They are safe to
  send; `bootDSP` and `setBlock` with unmapped data are not
- `[Failed]` also means "value unchanged" (a gain that rounds to the current one). Seen for
  gain and Monitor Mix and assumed for every SET, unchecked on hardware: a preset load
  (`load_mv7_preset()`) sends only what differs from a fresh readback (nothing if a reply
  didn't parse, since the readback then fails) and reports refusals without stopping, and
  gain, mute and Monitor Mix re-read the value before calling `[Failed]` an error. Monitor
  Mix 1% and 2% encode as 0% (`MIX_CURVE`), so a step from 0% onto them changes nothing;
  that it then answers `[Failed]` follows from the above and is unconfirmed on hardware
- `[Failed]` and `Locked` carry no command name, so `send_text()` drains queued input before
  every write. Without that, a stale one is read as the reply to the next command
- `dspMode` holds mode, mic position and tone together, and changing it also resets EQ and
  compressor (entering Auto) or reports a new gain (leaving it). `main.rs` re-reads the full
  state after any such change
- Map a new block by snapshotting `getBlock` before and after one change in MOTIV. MOTIV's
  exact command strings are in its bundled `ndl-addon.node` (`strings -a`), but that addon
  is shared with other Shure devices
- Except the Limiter block (`1F`): its ID and values come from Shure's
  `DSP-Settings-MV7.xml` (via schlarpc/shure-mv7-firmware-re), not a MOTIV snapshot, and
  MOTIV's own limiter toggle changed no block shurectl reads (2026-09-25). Unconfirmed on
  hardware, so its read is optional (`limiter_enabled` stays `None`) and a mic that doesn't
  answer it still works
- MOTIV and shurectl share one command channel, and shurectl's live poll sends `micMute`,
  `dspMode`, the gain and the Monitor Mix block every second (stopping after `micMute` when
  it goes unanswered). Quit MOTIV before testing against the MV7

## Debugging on real hardware

Capture packets before guessing:

```
sudo modprobe usbmon
lsusb | grep -i shure            # find bus number
sudo wireshark -i usbmonN        # filter: usb.transfer_type == 0x01
```

Compare captures against `cmd_*` constructor output. Firmware-version differences almost
always show up as `FEAT_*` addresses or value encoding. Fix those in `protocol.rs` only.
