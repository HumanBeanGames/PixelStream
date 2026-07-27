# pixel_stream_settings

`pixel_stream_settings` is a small Bevy-friendly settings description crate used
by PixelStream. It does not know about any specific game. A host application
publishes categories and fields, and PixelStream can render those settings in
preview or setup UI.

```rust
use pixel_stream_settings::{
    PixelSettingCategory, PixelSettingField, PixelSettingNumberRange,
    PixelSettingsAppExt,
};

app.add_pixel_settings_category(
    PixelSettingCategory::new("terrain", "Terrain")
        .with_order(10)
        .with_field(PixelSettingField::number(
            "roughness",
            "Roughness",
            0.8,
            PixelSettingNumberRange::new(0.0, 1.0, 0.01),
        )),
);
```

Downstream games own the real resources and systems that apply settings. The
registry is only the editable description and current UI value. Listen for
`PixelSettingChanged` messages to update game resources or persist changes.

## Field Types

- `Number`: slider, numeric text input, and drag adjustment.
- `Bool`: checkbox/toggle.
- `Text`: plain string entry.
- `Choice`: one value selected from a fixed list.

## Persistence

`PixelSettingsRegistry::save_json` and `PixelSettingsRegistry::load_json` provide
a simple schema snapshot for tools and editor state. Games can also serialize
their native settings directly and rebuild the registry on startup.
