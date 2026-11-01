---
paths:
  - "src/meter.rs"
  - "src/main.rs"
---

# Meter

`meter.rs` runs a cpal capture stream on a background thread and publishes sample-peak
dBFS × 10 via `peak_window: Arc<Mutex<PeakWindow>>`: the `short` (0.3 s) window drives bar
height; the `long` (3.0 s) window drives the peak-hold readout.

Flow: cpal callback → `peak_dbfs_x10()` → `peak_window` → read by `ui.rs` each render tick.

- `RollingWindow::max(now)` ignores expired entries, so a stalled stream reads as no data
  rather than freezing on its last value.
- **Which input:** `start_meter()` picks the connected device's own input when exactly one
  input matches its `InputTarget` (`matching_input()`): the USB serial in the input's ID or
  name, else the USB product string in its name. Otherwise, or on plain ALSA, it uses the
  system default input. The Info tab's Meter Input row says which (`App::meter_input`).
- **Linux:** cpal's `pulseaudio` feature makes `default_host()` PipeWire or PulseAudio when
  one runs. Their input IDs hold the USB serial with spaces and `#` as `_`, and the
  `.monitor` sources carry it too (seen on the MVX2U Gen 2 under PipeWire, 2026-10-31). Never
  match ALSA's own devices: `hw:`/`plughw:` open the hardware itself and can keep PipeWire,
  and every app recording through it, off the mic. Left to choose, PipeWire called back
  about every 300 ms, so the Pulse host asks for a 20 ms buffer (`PULSE_CALLBACK_HZ`).
  Tested 2026-10-31 under PipeWire: with an MVX2U Gen 2 and an MV6 Gen 2 connected, the meter
  heard the one chosen in the picker, and the MVX2U Gen 2 with the MV6 Gen 2 as the system
  default input; `pw-record` recorded the mic while the meter ran; with `PULSE_SERVER`
  pointed nowhere it fell back to ALSA's default input, said so, and still read levels
- **macOS and Windows:** matching on the product string or serial is unconfirmed on hardware.
  The product string matches as whole words, and not when "Gen" follows it, so an MV6
  never takes an MV6 Gen 2's input, nor an MV7 an MV7+'s (`names_product()`)
- The meter reads after the OS input volume, i.e. what apps record.
- `main.rs` stops the meter when the device is lost, so it never shows another input the
  system moves the stream to, and starts it again whenever `reconnect()` replaces the handle
  or the device comes back from lost, once the device shows as connected
  (`App::meter_restart_pending`). A restart waits while a gain check listens, so one check
  never hears two inputs. `r` replaces the handle only when its readback fails; a successful
  `r` asks for the meter again only while it isn't on the device's own input (on "System
  default input", or unavailable), for an input that came up after the device did. The meter
  doesn't start at all when the first readback finds the device gone. If the matched input
  or the 20 ms period fails to open, `start_meter()` tries the default period, then the default input. Both `r`
  finding a late input and that fallback order are unconfirmed on hardware.
  Tested 2026-10-31 on the MVX2U Gen 2 under PipeWire, with an MV6 Gen 2 as the default
  input: unplugged, the meter showed no reading while the MV6 Gen 2 was spoken into; after a
  replug it read again with no `r`; `r` kept it reading; a gain check stopped on unplug and
  worked after the replug. A replug quick enough to skip the disconnect couldn't be made by
  hand, as the live poll noticed every unplug first.
- `start_meter()` returns `MeterStatus`. The caller must keep the `Stream` in
  `MeterStatus::Running` alive; dropping it stops capture.
- The meter does not start in demo mode.
- The gain check (`gain_check.rs`) samples the `short` window from `step()`; a 0.3 s window
  read every ≤100 ms tick misses no peak. It only reads `peak_window`.
- `libc` `dup`/`dup2` suppresses stderr while cpal probes ALSA/JACK.
