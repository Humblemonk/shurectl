---
paths:
  - "src/presets.rs"
---

# Presets

TOML files in `~/.config/shurectl/presets/`, 4 fixed slots (`preset_1.toml`–`preset_4.toml`).

- **Mirror types:** `presets.rs` defines `Ser*` enums with serde derives, so `protocol.rs`
  types stay serde-free and the on-disk format is decoupled from internal enum changes
- `PresetSlot` captures all DSP settings from `DeviceState`. Identity fields
  (`serial_number`, `firmware_version`) are excluded and preserved on apply. The device
  name is the exception: saved, and written on load by `main.rs::apply_preset_to_state()`
  only when the model has one and it changed ("" = not saved)
- **New fields** need `#[serde(default)]`, and `apply_to_device_state()` must keep the
  current value when the default means "not saved". Older files and presets from models
  without the setting otherwise zero it on load (Playback Mix 0 = not saved, 73476a4)
- **Gain Lock** in `main.rs::apply_preset_to_device()`: unlock, write the gain, re-lock
  straight away. A locked device ignores gain writes
- `load_all_presets()` runs at startup; missing files → `None`
- `DeviceAction` variants: `SavePreset`, `LoadPreset` (applies, then sends all SETs),
  `DeletePreset`, `PersistPresetName`
- Tests use `tempfile` for hermetic temp dirs
