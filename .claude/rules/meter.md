---
paths:
  - "src/meter.rs"
  - "src/main.rs"
---

# Meter

`meter.rs` runs a cpal capture stream (default input device only) on a background thread and
publishes sample-peak dBFS × 10 via `peak_window: Arc<Mutex<PeakWindow>>`: the `short` (0.3 s)
window drives bar height; the `long` (3.0 s) window drives the peak-hold readout.

Flow: cpal callback → `peak_dbfs_x10()` → `peak_window` → read by `ui.rs` each render tick.

- `RollingWindow::max(now)` ignores expired entries, so a stalled stream reads as no data
  rather than freezing on its last value.
- The meter reads the system default input after the OS input volume, i.e. what apps record.
  It is only the Shure's level when the Shure is the default input.

- `start_meter()` returns `MeterStatus`. The caller must keep the `Stream` in
  `MeterStatus::Running` alive; dropping it stops capture.
- The meter does not start in demo mode.
- `libc` `dup`/`dup2` suppresses stderr while cpal probes ALSA/JACK.
