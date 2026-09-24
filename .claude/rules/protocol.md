---
paths:
  - "src/protocol.rs"
  - "src/device.rs"
  - "src/bin/probe.rs"
---

# USB HID Protocol

Every packet is exactly 64 bytes (65 with hidapi's report-ID byte 0), sent via `hid_write()`
/ `hid_read()` on `/dev/hidrawN`. Not the USB audio class interface, and not
`HIDIOCSFEATURE`/`HIDIOCGFEATURE`.

```
[0x01] [0x11] [0x22] [seq] [0x03] [0x08] [len] [0x70] [len] [cmd0..cmd2] [feat_addr..] [value..] [crc_hi] [crc_lo] [pad..]
  ^report ID    ^magic, never changes            CRC-16/ANSI covers 0x11 onward
```

- **USB IDs:** VID `0x14ED`; PID `0x1013` (MVX2U Gen 1), `0x1033` (MVX2U Gen 2), `0x1026` (MV6), `0x1019` (MV7+)
- **CRC:** CRC-16/ANSI: poly `0x8005`, init `0x0000`, reflected in/out (NOT CCITT-FALSE)
- **SET + CONFIRM:** every SET must be followed immediately by a `CMD_CONFIRM` packet, or the
  device won't apply the change
- **State readback:** there is no monolithic GET_STATE. `device.rs::get_state()` issues
  individual `cmd_get_*` packets; `apply_response()` dispatches on the 2-byte feature address
  and writes into `DeviceState`. The address → field mapping is documented inline in
  `protocol.rs`.
- `hidapi` uses the `linux-native` feature (`/dev/hidrawN` access).

## Debugging on real hardware

Capture packets before guessing:

```
sudo modprobe usbmon
lsusb | grep -i shure            # find bus number
sudo wireshark -i usbmonN        # filter: usb.transfer_type == 0x01
```

Compare captures against `cmd_*` constructor output. Firmware-version differences almost
always show up as `FEAT_*` addresses or value encoding. Fix those in `protocol.rs` only.
