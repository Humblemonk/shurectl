---
paths:
  - "src/presets.rs"
---

# Presets

TOML files in `~/.config/shurectl/presets/`, 4 fixed slots (`preset_1.toml`–`preset_4.toml`).

- **Mirror types:** `presets.rs` defines `Ser*` enums with serde derives, so `protocol.rs`
  types stay serde-free and the on-disk format is decoupled from internal enum changes
- `PresetSlot` captures all DSP settings from `DeviceState`. Identity fields
  (`serial_number`, `firmware_version`) are excluded and preserved on apply
- `load_all_presets()` runs at startup; missing files → `None`
- `DeviceAction` variants: `SavePreset`, `LoadPreset` (applies, then sends all SETs),
  `DeletePreset`, `PersistPresetName`
- Tests use `tempfile` for hermetic temp dirs
