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
  about every 300 ms, so the Pulse host asks for a 20 ms buffer (`PULSE_CALLBACK_HZ`)
- **macOS and Windows:** matching by-product string or serial is unconfirmed on hardware
- The meter reads after the OS input volume, i.e. what apps record.
- `main.rs` starts the meter again after a reconnect, as the old stream's input left with the
  device. After an unplug and replug, the meter read again with no `r` (MVX2U Gen 2 under
  PipeWire, tested 2026-10-31).
- `start_meter()` returns `MeterStatus`. The caller must keep the `Stream` in
  `MeterStatus::Running` alive; dropping it stops capture.
- The meter does not start in demo mode.
- The gain check (`gain_check.rs`) samples the `short` window from `step()`; a 0.3 s window
  read every ≤100 ms tick misses no peak. It only reads `peak_window`.
- `libc` `dup`/`dup2` suppresses stderr while cpal probes ALSA/JACK.
