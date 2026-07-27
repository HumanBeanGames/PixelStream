# PixelStream Stream Settings

PixelStream exposes stream control through CLI flags, the stats/control window,
programmatic Bevy messages, quickstart settings, and preview mode.

## Video

Runtime custom-host video is IPSMAP-only. A valid `.ipsmap` lookup must be
provided; missing palette lookup data should fail fast.

Current custom-host validation:

- square resolution;
- 64 to 256 pixels;
- width divisible by 8;
- 1 to 60 FPS.

Batch size controls how many frames are grouped into each browser stream batch.
It affects video latency and therefore audio alignment.

## Programmatic Control

Downstream apps can start or stop the stream with:

- `PixelStreamStartRequest`;
- `PixelStreamStopRequest`;
- `PixelStreamControlResult`;
- `PixelStreamState`.

This is what Mercantile uses for quickstart streaming.

## Audio

Stream audio is mixed by PixelStream and sent through `/audio.pcm`. The browser
does not play Bevy speaker output. `PixelStreamAudioSyncConfig` controls whether
audio is delayed by a fixed value, by the estimated video latency, or by
measured stream latency.

For gameplay, play sound immediately in the app and let the stream audio delay
align it with the buffered video.

## Browser UI

Requested panels, chat visibility, overlays, and status are driven by the app.
When the app stops publishing them, the browser page should clear stale UI
instead of treating panels as permanent page furniture.

## Preview

Preview mode renders both raw and quantized views using the same stream
resolution and palette lookup path as custom-host mode. Downstream settings can
be exposed through the generic settings registry while remaining owned by the
downstream game.

