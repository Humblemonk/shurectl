---
paths:
  - "src/meter.rs"
  - "src/main.rs"
---

# Meter

`meter.rs` runs a cpal capture stream (default input device only) on a background thread and
publishes via `Arc`:

- `meter_level: Arc<AtomicI32>`: instantaneous peak dBFS × 10, lock-free
- `peak_window: Arc<Mutex<PeakWindow>>`: the `short` (0.3 s) window drives bar height; the
  `long` (3.0 s) window drives the peak-hold marker

Flow: cpal callback → `meter_level` + `peak_window` → read by `ui.rs` each render tick.

- `start_meter()` returns `MeterStatus`. The caller must keep the `Stream` in
  `MeterStatus::Running` alive; dropping it stops capture.
- The meter does not start in demo mode.
- `libc` `dup`/`dup2` suppresses stderr while cpal probes ALSA/JACK.
