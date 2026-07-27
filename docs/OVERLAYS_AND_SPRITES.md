# PixelStream Overlays And Sprites

PixelStream has two separate annotation paths: browser overlays and world
sprites.

## Browser Overlays

Browser overlays are published through `CustomHostOverlayHub` and are drawn by
the custom-host page above the stream canvas. They are ideal for local UI
annotations that should not affect the game render.

Supported element kinds include:

- circles and ellipses;
- flags;
- text labels;
- sprite/image markers.

Overlays support:

- global, viewer-identity, and viewer-name audiences;
- stream-pixel and normalized coordinate spaces;
- ordering;
- TTL/expiry;
- clearing by id or region.

Use browser overlays for selected-town rings, flags, click hints, and
client-only focus indicators.

## World Sprites

`DirectWorldSprite` attaches to a normal Bevy entity with `Transform` and
`GlobalTransform`. PixelStream projects the entity through the stream camera and
renders a readable sprite in stream-pixel units.

Important behavior:

- `pixel_size` is measured in output pixels, not world units.
- placement uses integer output-pixel coordinates by design;
- atlas layouts and frame indices are supported;
- sprites depth-test against the rendered scene;
- optional depth write allows nearer sprites to occlude farther sprites;
- sprites are unlit and do not receive scene shadows;
- DirectText renders after world sprites.

Use world sprites for tiny moving actors that still need depth interaction with
terrain, towns, and other world geometry.

