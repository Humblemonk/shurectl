---
paths:
  - "src/presets.rs"
---

# Presets

TOML files in `~/.config/shurectl/presets/`, 4 fixed slots (`preset_1.toml`–`preset_4.toml`).

- **Mirror types:** `presets.rs` defines `Ser*` enums with serde derives, so `protocol.rs`
  types stay serde-free and the on-disk format is decoupled from internal enum changes
- `PresetSlot` captures all DSP settings from `DeviceState`. Identity (serial number,
  firmware version, device name) isn't saved, so a load keeps the device's own. Neither is
  mute: it is live state, and a load must never mute or unmute the mic. Older files still
  hold `muted`, which loading ignores. The Mute Button setting (Works / Ignored) is saved
- **Per-model settings** are `Option`s, saved only by a model that has the setting (the
  `match` in `from_device_state()`, which mirrors `main.rs::apply_binary_preset()` and the
  MV7's `setting_changes()`). `None` keeps the device's value on load, so a preset from
  another model can't reset what that model lacks (Gain Lock, Mute Button, Limiter;
  checked on hardware between the MVX2U Gen 2 and MV6 Gen 2). The summary leaves `None`
  parts out. A new per-model field goes in that `match` and that load too
- **Phantom power** is per-model, saved only on models with `has_phantom_power()`, so a
  mic's preset can't cut power to a condenser mic
- **Loading** writes only what the preset saved (`apply_binary_preset()`), never the shown
  state's copy, which may be stale. The MV7 applies the preset onto its own fresh readback
  (`load_mv7_preset()`)
- **Fitting** a preset to a model's state is `PresetSlot::for_load()` then
  `apply_preset_to_state()`, here, used by the load and by the TUI's load box
  (`ui.rs::draw_preset_load_box()`), so the box lists exactly what the load sets. Change the
  fitting only there. The box compares against the shown state, which can be stale for
  settings the live poll doesn't read
- **Gain Lock**: a locked device ignores gain writes, so `apply_binary_preset()` unlocks,
  writes the gain, and re-locks straight away. A preset without Gain Lock re-locks to the
  shown setting
- **EQ band switches** (`SerEqBand::enabled`) exist only on the MVX2U Gen 1, so only it
  saves them and only it is sent them (`write_eq_bands()`); the Gen 2 and MV6 Gen 2 save
  and get band gains alone
- **New fields** need `#[serde(default)]`, and `apply_to_device_state()` must keep the
  current value when the field is missing. Older files otherwise zero it on load (Playback
  Mix 0 = not saved, 73476a4)
- **Monitor Mix** is a balance on the MVX2U Gen 1 and MV7 but the Mic Level elsewhere.
  `monitor_mix_is_mic_level` records which (`from_device_state()` takes the model), and
  `apply_preset_to_state()` skips a Monitor Mix of the other kind, without writing it.
  `None` (older files) loads as before
- **Older files** hold every field from every model and still apply them all: before
  2.12.0 that includes `phantom_power = false`, before 2.13.1 every other per-model setting.
  Nothing in them records which model saved them
- `load_all_presets()` runs at startup; missing files → `None`
- `DeviceAction` variants: `SavePreset`, `LoadPreset` (applies, then sends a SET for each
  saved setting; the MV7 only the changed ones), `DeletePreset`, `PersistPresetName`
