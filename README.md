# PixelStream

PixelStream is a Bevy streaming library for games that are played through
a custom browser host. It renders your game to an offscreen stream target, reads
the final frame back from the GPU, palette-encodes it, and serves it through a
small local web server with audio, chat, panels, and click input.

The repository binary is a demo. The reusable library is exposed from
`src/lib.rs`.

Focused API docs:

- [Chat API](docs/CHAT_API.md)
- [Overlays And Sprites](docs/OVERLAYS_AND_SPRITES.md)
- [Stream Settings](docs/STREAM_SETTINGS.md)
- [Generic Settings Crate](crates/pixel_stream_settings/README.md)

## Minimal Example Project

[PixelStream Chat Dash](https://github.com/HumanBeanGames/PixelStream-Chat-Dash) is a small downstream game that uses PixelStream and `pixel_stream_settings` as remote Git dependencies. It demonstrates browser chat commands changing the streamed game, viewer-scoped game replies in chat, global score announcements, and live side-panel updates.

## What It Provides

- Bevy `0.19` app shell with a dedicated stream render target.
- GPU readback with bounded in-flight capture and fixed-size frame batching.
- GPU palette indexing with required `.ipsmap` lookup textures in custom-host mode.
- Indexed Pixel Stream Codec (`IPSC`) custom-host video.
- 8 kHz mono mu-law browser audio for low-bandwidth custom streams.
- Stream-only audio mixer. Bevy speaker output is disabled by default.
- Local browser chat with generated viewer names, command routing, bot replies,
  temporary messages, and purge support.
- Side-panel publishing for custom app UI outside the stream canvas.
- Stream canvas click events forwarded back into Bevy.
- Stats/control window with Start, End, Open, Purge Chat, resolution, and FPS
  controls.
- Palette Lab and PNG Converter Lab for creating palettes, LUTs, and IPSI still
  images.
- Demo scene with looping music, `!boing` sound effect, and drag-and-drop video
  background playback.

There is no external chat or RTMP path. The library specializes in the custom
browser host.

## Requirements

- Rust stable, currently verified with `rustc 1.95.0`.
- Bevy `0.19`.
- Windows/MSVC receives the most testing.
- Dynamic FFmpeg libraries with headers/import libs available at build time and
  DLLs available at runtime.

The app does not launch `ffmpeg.exe`. It links to FFmpeg through
`ffmpeg-next`/`ffmpeg-sys-next` for local preview/media tooling. For
closed-source distribution, keep FFmpeg dynamically linked and use an
LGPL-compatible FFmpeg build. Do not enable `ffmpeg-next` static, GPL, or
nonfree build features. See `FFMPEG-LGPL-COMPLIANCE.md`.

## FFmpeg Setup On Windows

The recommended route is vcpkg with the included `vcpkg.json` manifest.

```powershell
git clone https://github.com/microsoft/vcpkg C:\vcpkg
C:\vcpkg\bootstrap-vcpkg.bat -disableMetrics
winget install LLVM.LLVM

$env:VCPKG_ROOT = "C:\vcpkg"
$env:VCPKG_DEFAULT_TRIPLET = "x64-windows"
C:\vcpkg\vcpkg.exe install "ffmpeg[avcodec,avformat,openh264,swresample,swscale]:x64-windows" --classic
```

Then keep vcpkg on the environment when building/running:

```powershell
$env:VCPKG_ROOT = "C:\vcpkg"
$env:PATH = "C:\vcpkg\installed\x64-windows\bin;$env:PATH"
cargo run --bin PixelStream -- --stats-window --custom-host
```

## Running The Demo

```powershell
cargo run --bin PixelStream -- --stats-window --custom-host
```

Then press **Start** in the stats window and open:

```text
http://127.0.0.1:8080
```

Useful flags:

```text
--preview
--stats-window
--headless-window
--custom-host
--palette-lookup=palette.ipsmap
--stream-width=128
--stream-height=128
--stream-fps=5
--batch-size=30
```

In custom-host mode, width and height must be equal, 8-aligned, and between
`64` and `256`. The default stream rate is `5fps`.

## Demo Controls

The demo starts with a hue-gradient background and `HelloWorld` text. It also:

- Loops `music/Elijah_K - Iron.wav` as backing music when present.
- Plays `sfx/boing_x.wav` when chat sends `!boing`.
- Accepts video files dragged onto the Bevy window.

Supported demo video extensions:

```text
.mp4 .mov .m4v .webm .mkv .avi
```

Best first test file:

```text
MP4 container, H.264 video, yuv420p, small resolution, 24/25/30 fps
```

Video audio is ignored. The video loops and is scaled into the stream render
target. This is demo-only code, not part of the streaming library API.

## Step-By-Step PixelStream Setup

Use this checklist when adding PixelStream to a new Bevy game or bringing an
existing game onto the custom browser host.

1. Install the native prerequisites.

   Set up Rust/MSVC and the dynamic FFmpeg libraries described in
   [FFmpeg Setup On Windows](#ffmpeg-setup-on-windows). Keep the FFmpeg DLL
   directory on `PATH` when building or running the game.

2. Add PixelStream to the downstream game's `Cargo.toml`.

   ```toml
   [dependencies]
   bevy = "0.19"
   pixel_stream = { package = "PixelStream", git = "https://github.com/HumanBeanGames/PixelStream" }
   ```

   During local development, use a path dependency instead:

   ```toml
   pixel_stream = { package = "PixelStream", path = "../PixelStream" }
   ```

3. Build your app from PixelStream's app shell.

   ```rust
   use bevy::prelude::*;
   use pixel_stream::{pixel_stream_app, PixelStreamSet};

   fn main() {
       pixel_stream_app()
           .add_systems(Startup, setup.after(PixelStreamSet::Setup))
           .add_systems(Update, update)
           .run();
   }

   fn setup() {}
   fn update() {}
   ```

   Schedule setup after `PixelStreamSet::Setup` whenever it needs the stream
   target, stream camera, direct text, direct sprites, or custom-host resources.

4. Render the game into `PixelStreamTarget.image`.

   PixelStream creates a default 2D stream camera. If your game uses that camera,
   attach UI with `UiTargetCamera(target.camera)` and spawn visible content on
   render layer `0`.

   If your game needs its own 2D or 3D camera, create that camera after
   `PixelStreamSet::Setup`, render it to `PixelStreamTarget.image`, and update
   `PixelStreamTarget.camera` if your game replaces the provided camera. The
   custom-host pipeline captures the stream image, not the normal desktop window.
   A healthy stream that is pure black usually means the game is still rendering
   only to the window camera.

5. Add the optional PixelStream overlays you need.

   - Use `DirectText` for text that must stay readable at tiny resolutions.
   - Use `DirectWorldSprite` for small world-anchored sprites that should depth
     test against the 3D scene.
   - Use custom-host panels and overlays for browser-side UI outside the stream.
   - Use `StreamCommandAppExt` and `StreamChatSender` for local browser chat and
     command replies.

6. Provide an IPSMAP palette lookup.

   Custom-host mode requires a `.ipsmap` file. Put it in the game working
   directory, commonly as `palette.ipsmap`, or pass it explicitly:

   ```powershell
   cargo run -- --stats-window --custom-host --palette-lookup=palette.ipsmap
   ```

   Use the palette lab or `ipsc_build_palette_lut` tool to create IPSMAP files.
   Runtime palette TOML fallback is not part of the custom-host path.

7. Run and test locally.

   ```powershell
   cargo run -- --stats-window --custom-host --palette-lookup=palette.ipsmap --stream-width=128 --stream-height=128 --stream-fps=30 --batch-size=30
   ```

   Press **Start** in the stats window, then open:

   ```text
   http://127.0.0.1:8080
   ```

   Confirm the stats show captured/read/encoded/sent frames, no unexpected drops,
   and that the browser stream matches the preview.

8. Add programmatic startup if the game should not require pressing Start.

   ```rust
   use bevy::prelude::*;
   use pixel_stream::PixelStreamStartRequest;

   fn auto_start(mut requests: MessageWriter<PixelStreamStartRequest>) {
       requests.write(PixelStreamStartRequest::custom_host(128, 128, 30));
   }
   ```

   Downstream games often wrap this in their own quickstart JSON or command-line
   flag so resolution, frame rate, and batch size can be stored together.

9. Export the static browser player for hosting.

   ```powershell
   cargo run --bin ipsc_export_static_stream -- https://game.example.com --page-title "MY GAME" --header-title "MY GAME" --max-player-width 640 --minimizable-player
   ```

   This writes `dist/humanbeangames_stream/index.html` by default. For a custom
   project, either deploy that generated folder or call
   `export_static_palette_stream_page` from your own tooling.

10. Put the backend behind a tunnel or reverse proxy.

    The Rust game process listens on `127.0.0.1:8080`. For public hosting, expose
    that local backend through a tunnel hostname such as `game.example.com`, and
    host the static player on a separate static site such as
    `stream.example.com`. Do not expose the raw Rust HTTP server directly to the
    public internet without a real reverse-proxy/auth layer.

11. Deploy the static player.

    Upload the generated player folder to Cloudflare Workers Static Assets,
    Cloudflare Pages Direct Upload, or another static host. The public player
    will poll the backend `/status.json`, show **Not Online** while the game is
    closed, and connect automatically when the local game process starts.

12. Redeploy only the part that changed.

    Browser/player changes require redeploying the static player. Rust-only
    game/backend changes require rebuilding and restarting the local game
    process. Palette-lab changes require redeploying the lab only if you host it
    publicly.

## Using The Library

Add the library to your game:

```toml
[dependencies]
bevy = "0.19"
pixel_stream = { package = "PixelStream", git = "https://github.com/HumanBeanGames/PixelStream" }
```

For local development, a path dependency also works:

```toml
pixel_stream = { path = "../PixelStream" }
```

Use the pixel-stream app shell instead of `App::new().add_plugins(DefaultPlugins)`:

```rust
use bevy::prelude::*;
use pixel_stream::{direct_stream_app, PixelStreamSet, PixelStreamTarget};

fn main() {
    direct_stream_app()
        .add_systems(Startup, setup.after(PixelStreamSet::Setup))
        .add_systems(Update, update)
        .run();
}

fn setup(mut commands: Commands, target: Res<PixelStreamTarget>) {
    commands
        .spawn((
            Node {
                width: percent(100),
                height: percent(100),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            UiTargetCamera(target.camera),
        ))
        .with_child(Text::new("My Game"));
}

fn update() {}
```

Run startup systems after `PixelStreamSet::Setup` when they need
`PixelStreamTarget` or the stream camera. Systems that do not depend on the
stream target can be scheduled normally.

Downstream systems can read `PixelStreamState` to pause live-only work while a
custom-host stream is stopped:

```rust
use bevy::prelude::*;
use pixel_stream::{PixelStreamMode, PixelStreamState};

fn advance_world(time: Res<Time>, stream: Res<PixelStreamState>) {
    if stream.mode == PixelStreamMode::CustomHost && !stream.active {
        return;
    }

    // Tick simulation, AI, music scheduling, etc.
}
```

`PixelStreamState` also exposes the current stream `width`, `height`, and
`fps`. In custom-host mode these update when the stats-window Start button
retargets the stream; Stop preserves the last dimensions and sets
`active = false`.

Custom-host streams can also be started or stopped from game code with messages:

```rust
use bevy::prelude::*;
use pixel_stream::PixelStreamStartRequest;

fn auto_start(mut requests: MessageWriter<PixelStreamStartRequest>) {
    requests.write(PixelStreamStartRequest::custom_host(128, 128, 30));
}
```

Read `PixelStreamControlResult` messages if you need to react to success or
validation failures. The stats-window Start/End buttons use the same message
path.

Audio/video sync can be tuned with `PixelStreamAudioSyncConfig`:

```rust
use pixel_stream::{AudioSyncMode, PixelStreamAudioSyncConfig};

app.insert_resource(PixelStreamAudioSyncConfig {
    mode: AudioSyncMode::MatchEstimatedVideoLatency,
    fixed_delay_ms: 1_000,
    extra_delay_ms: 0,
});
```

`Fixed` preserves the old fixed-delay behavior. `MatchEstimatedVideoLatency`
uses the configured FPS and batch size to align stream audio with the expected
video presentation delay. `MatchMeasuredVideoLatency` currently falls back to
the estimate unless measured browser telemetry is supplied in a future version.

### Migrating An Existing Bevy Game

Before:

```rust
use bevy::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_systems(Startup, setup)
        .add_systems(Update, update)
        .run();
}
```

After:

```rust
use bevy::prelude::*;
use pixel_stream::{direct_stream_app, PixelStreamSet};

fn main() {
    direct_stream_app()
        .add_systems(Startup, setup.after(PixelStreamSet::Setup))
        .add_systems(Update, update)
        .run();
}
```

UI should be attached to the stream camera with `UiTargetCamera(target.camera)`.
Camera-heavy 2D/3D games may need an adapter so their main camera renders to
`PixelStreamTarget.image` or is replaced by the provided stream camera. The
library should remain usable by 3D projects; the custom stream path consumes the
final render target, not a specific 2D scene model.

## Stream Audio

The app disables Bevy's normal speaker audio plugin. Audio is sent to the stream
through `PixelStreamAudioTarget`.

Simple clip playback:

```rust
use bevy::prelude::*;
use pixel_stream::{PlayStreamSound, StreamAudioClip};

#[derive(Resource)]
struct HitSound(Handle<StreamAudioClip>);

fn setup_audio(mut commands: Commands, mut clips: ResMut<Assets<StreamAudioClip>>) {
    let samples = vec![0.0; 48_000 / 10];
    let clip = StreamAudioClip::from_mono_f32(samples, 48_000);
    commands.insert_resource(HitSound(clips.add(clip)));
}

fn play_hit(sound: Res<HitSound>, mut sounds: MessageWriter<PlayStreamSound>) {
    sounds.write(PlayStreamSound::once(sound.0.clone()).with_volume(0.5));
}
```

You can also load WAV files with `StreamAudioClip::from_wav_file`. The mixer
handles common WAV formats and caches the decode path per file. Lower-level
audio engines can push samples directly into `PixelStreamAudioTarget` with
`push_stereo_f32` or `push_mono_f32`.

The stream target expects `48_000 Hz`, stereo, `f32` samples in `[-1.0, 1.0]`.
Custom-host output currently sends browser audio as 8 kHz mono mu-law to keep
bandwidth low.

## Chat Commands

Register commands with `StreamCommandAppExt`.

```rust
use bevy::ecs::system::In;
use bevy::prelude::*;
use pixel_stream::{
    direct_stream_app, StreamChatCommand, StreamChatSender, StreamCommandAppExt,
};

fn main() {
    direct_stream_app()
        .add_stream_command("boing", handle_boing)
        .run();
}

fn handle_boing(In(command): In<StreamChatCommand>, chat: Option<Res<StreamChatSender>>) {
    if let Some(chat) = chat {
        chat.send(format!("Boing, {}!", command.display_name));
    }
}
```

`StreamChatCommand` includes:

- `user`
- `display_name`
- `command`
- `args`
- `roles`
- `message_id`

Local custom-host users receive generated names such as `BrightDragon-A1` based
on a hash of the viewer identity. Browser clients generate a stable
`directstream_device_id` in `localStorage` and send it with local chat, chat
feed, custom panel, and stream-click requests. That makes identity scoped to a
browser profile/device instead of collapsing everyone behind the same network
IP into one viewer. If the browser does not send a device id, the server falls
back to the old IP/proxy-header behavior.

This is a pseudonymous browser-profile id, not authentication or hardware
fingerprinting. It persists across reloads and normal browser restarts, but
changes if the viewer clears site data, uses another browser/profile, or opens
an incognito session. For dev/debug resets, run this in the browser console and
reload:

```js
localStorage.removeItem("directstream_device_id")
```

The active app session keeps a recent chat history and a generated-name cache.
The stats window **Purge Chat** button clears the current local chat feed.

Bot/system replies can be sent through `StreamChatSender::send`. Custom local
entries can be created with `StreamChatSender::send_local` and
`LocalChatEntryOptions`, including optional TTLs, mention metadata, and safe
per-message styling. Viewer-authored custom-host messages automatically get a
stable display-name color derived from their identity hash.

```rust
use std::time::Duration;
use bevy::prelude::*;
use pixel_stream::{LocalChatEntryOptions, StreamChatSender};

fn reply(chat: Res<StreamChatSender>) {
    chat.send_local(
        LocalChatEntryOptions::named("Market", "Salt is cheap today.")
            .with_display_name_color("#f7c548")
            .with_message_color("white")
            .with_css_class("market-reply")
            .with_ttl(Duration::from_secs(10)),
    );
}
```

Chat colors accept safe `#RGB`, `#RRGGBB`, `rgb(r,g,b)`, `hsl(h s% l%)`, or a
small named-color set. CSS classes are sanitized to short alphanumeric,
underscore, or hyphen tokens before they reach the browser.

Viewer-authored chat is moderated before it is echoed or dispatched as a
command. The built-in baseline blocks links, a small profanity/slur list,
messages faster than one every 5 seconds, and repeats of the same normalized
message within 30 seconds. Rejected messages are not added to chat history and
do not trigger command handlers; the viewer receives a short-lived private
system warning instead. Moderation state is in-memory and scoped to the current
app session plus viewer identity.

The custom-host chat window is opt-in. Downstream apps request it with
`CustomHostChatPanelHub`; otherwise the browser page does not create a chat
panel or poll the chat feed.

```rust
use bevy::prelude::*;
use pixel_stream::CustomHostChatPanelHub;

fn request_chat(chat_panel: Res<CustomHostChatPanelHub>) {
    chat_panel.show();
}
```

## Custom Host Page

The browser page can be branded and sized by replacing the default resources
before the app starts:

```rust
use pixel_stream::{CustomHostBranding, CustomHostLayout, direct_stream_app};

fn main() {
    direct_stream_app()
        .insert_resource(CustomHostBranding::new("MERCANTILE", "MERCANTILE"))
        .insert_resource(
            CustomHostLayout::default()
                .prefer_larger_player()
                .with_max_player_width(1280)
                .minimizable_player(),
        )
        .run();
}
```

`CustomHostLayout` controls the maximum player width, whether the page uses the
larger default player cap, and whether the browser shows a persistent
minimize/restore stream button. The minimized state is stored in browser
`localStorage`.

Runtime and static pages both derive their visible title/header from
`CustomHostBranding`. `/status.json` also reports the active branding, layout,
package version, and latency estimates, so a static Pages export can correct
stale visible branding when it connects to a differently branded runtime host.
Static exports include version and export-time metadata in the HTML.

## Panels And Clicks

Downstream games can publish arbitrary side-panel text:

```rust
use bevy::prelude::*;
use pixel_stream::{CustomHostPanelAnchor, CustomHostPanelHub};

fn update_panel(panels: Res<CustomHostPanelHub>) {
    panels.publish_text_at(
        "town-prices",
        "Northpass Prices",
        "wool 4g\nsalt 5g",
        CustomHostPanelAnchor::LeftOfStream,
        0,
    );
}
```

Panel anchors are `LeftOfStream`, `RightOfStream`, `AboveStream`,
`BelowStream`, `OverlayTopLeft`, `OverlayTopRight`, `OverlayBottomLeft`,
`OverlayBottomRight`, and `NamedRegion(String)`. Panels in each anchor are
ordered by `order`, then `id`. `publish_text` still uses the right-side default
stack below chat, and the older `CustomHostPanelRegion` helpers remain
available for compatibility. For full control, publish a `CustomHostPanel`
with `anchor`, `order`, optional `size_hint`, and optional `style_hint`.
Set `CustomHostPanelStyle { hide_header: true, ..default() }` to render a panel
body without title/header chrome.

For one-line route/status panels, use the helper style:

```rust
use pixel_stream::{CustomHostPanelStyle, PanelWhiteSpace};

let style = CustomHostPanelStyle::headerless()
    .with_body_white_space(PanelWhiteSpace::NoWrap);
```

For panels that should wrap and grow with their content without scrollbars, use
`PanelOverflowMode::WrapNoScroll` or the convenience helper:

```rust
use pixel_stream::CustomHostPanelStyle;

let style = CustomHostPanelStyle::default()
    .wrap_no_scroll()
    .with_region_css_class("market-left-column");
```

`WrapNoScroll` preserves newlines, allows long words to wrap, sets panel content
to `min-width: 0`, and avoids the horizontal/vertical scrollbar behavior used
by the default `Auto` mode. `region_css_class` is applied to the browser layout
region that receives the panel, after browser-side class-name validation.

Panels can be shared globally or filtered to one viewer. This mirrors local chat
audiences: `All`, `ViewerIdentity(String)`, or `ViewerName(String)`. The custom
host filters `/custom-panels` per request, so every viewer can safely have a
panel with the same app-level id.

```rust
use bevy::prelude::*;
use pixel_stream::{
    CustomHostPanel, CustomHostPanelAnchor, CustomHostPanelAudience, CustomHostPanelElement,
    CustomHostPanelElementStyle, CustomHostPanelHub, CustomHostPanelPage, PagedTextControls,
    PagedTextControlsPosition,
};

fn publish_viewer_panel(panels: Res<CustomHostPanelHub>, viewer_identity: String) {
    panels.publish(CustomHostPanel {
        id: "selected-town-prices".to_owned(),
        title: "Selected Town".to_owned(),
        body: "wool 4g\nsalt 5g".to_owned(),
        elements: vec![
            CustomHostPanelElement::Text("wool 4g\nsalt 5g\n".to_owned()),
            CustomHostPanelElement::StyledText {
                text: "14. CozyDryad-KN - 176g\n".to_owned(),
                style: CustomHostPanelElementStyle::default()
                    .with_text_color("#f7c548")
                    .with_css_class("personal-score")
                    .with_font_weight("700"),
            },
            CustomHostPanelElement::Button {
                label: "Buy Wool".to_owned(),
                action_id: "buy-wool".to_owned(),
                disabled: false,
            },
            CustomHostPanelElement::PagedText {
                id: "industries".to_owned(),
                pages: vec![
                    CustomHostPanelPage {
                        title: Some("Mill".to_owned()),
                        body: "grain > flour".to_owned(),
                    },
                    CustomHostPanelPage {
                        title: Some("Weaver".to_owned()),
                        body: "wool > cloth".to_owned(),
                    },
                ],
                initial_page: 0,
                controls: PagedTextControls {
                    position: PagedTextControlsPosition::BeforePage,
                    ..default()
                },
            },
        ],
        revision: 0,
        anchor: CustomHostPanelAnchor::LeftOfStream,
        order: 10,
        size_hint: None,
        style_hint: None,
        audience: CustomHostPanelAudience::ViewerIdentity(viewer_identity),
    });
}
```

Panel button clicks are emitted as `CustomHostPanelAction` messages. The event
includes `viewer_identity`, `viewer_name`, `panel_id`, and the stable
`action_id` string from the clicked button.
`CustomHostPanelElement::PagedText` is browser-local: previous/next controls
switch pages instantly without posting back to Bevy, and the page index is
preserved across `/custom-panels` refreshes as long as the page still exists.

```rust
use bevy::prelude::*;
use pixel_stream::CustomHostPanelAction;

fn handle_panel_actions(mut actions: MessageReader<CustomHostPanelAction>) {
    for action in actions.read() {
        if action.panel_id == "selected-town-prices" && action.action_id == "buy-wool" {
            // Apply this only to action.viewer_identity.
        }
    }
}
```

Browser clicks on the stream canvas are emitted as `StreamPointerClick` messages
with viewer identity, display name, raw browser client coordinates, corrected
stream pixel coordinates, and normalized stream coordinates. The browser maps
clicks against the actual rendered stream image rectangle, accounting for CSS
borders, aspect-ratio containment, and letterboxing. Clicks outside the rendered
image are ignored. Your game owns hit-testing and game-specific behavior.

Viewer-scoped browser overlays can draw local-only highlights above the stream
canvas without modifying the shared stream pixels:

```rust
use bevy::prelude::*;
use pixel_stream::{
    CustomHostOverlayElement, CustomHostOverlayHub, CustomHostPanelAudience,
    OverlayCoordinateSpace, OverlayElementKind, OverlayElementStyle,
};

fn highlight_town(overlays: Res<CustomHostOverlayHub>, viewer_identity: String) {
    overlays.publish(CustomHostOverlayElement {
        id: "selected-town".to_owned(),
        audience: CustomHostPanelAudience::ViewerIdentity(viewer_identity),
        x: 0.42,
        y: 0.61,
        coordinate_space: OverlayCoordinateSpace::NormalizedStream,
        kind: OverlayElementKind::Circle { radius: 8.0 },
        order: 0,
        style: OverlayElementStyle::default(),
        ttl_ms: None,
    });
}
```

Overlay coordinates can be stream pixels or normalized stream coordinates.
Supported overlay kinds are circles, flags, text, and simple sprite/image
references. Like panels, overlays are keyed internally by audience and id, so
two viewers can both have `selected-town` without colliding or leaking state.

## Direct Frame Processing

For exact pixel overlays, register a raw BGRA frame processor. Processors run
after GPU readback has produced CPU-writeable bytes and before the frame is sent
to preview/custom-host encoders.

```rust
use pixel_stream::{
    direct_stream_app, PixelStreamFrame, PixelStreamFrameAppExt,
};

fn main() {
    direct_stream_app()
        .add_direct_stream_frame_processor(draw_overlay)
        .run();
}

fn draw_overlay(mut frame: PixelStreamFrame) {
    let width = frame.width();
    let row_bytes = frame.row_bytes();
    let pixels = frame.bgra_mut();
    let _ = (width, row_bytes, pixels);
}
```

This is the right hook for integer-coordinate overlays such as DirectText,
because it avoids Bevy text, texture sampling, and GPU scaling artifacts.

## Direct World Sprites

`DirectWorldSprite` adds readable low-resolution sprites to ordinary world
entities without replacing their real 3D meshes, colliders, or shadows. Attach it
to an entity that already has `Transform` and `GlobalTransform`:

```rust
use bevy::prelude::*;
use pixel_stream::{
    DirectWorldSprite, SpriteDepthMode, SpriteFacing,
};

fn spawn_caravan(mut commands: Commands, assets: Res<AssetServer>) {
    commands.spawn((
        Transform::from_xyz(4.0, 0.0, -8.0),
        GlobalTransform::default(),
        DirectWorldSprite {
            image: assets.load("sprites/caravan.png"),
            atlas: None,
            atlas_index: 0,
            pixel_size: UVec2::new(8, 10),
            anchor: Vec2::new(0.5, 1.0),
            tint: Color::WHITE,
            facing: SpriteFacing::FaceStreamCamera,
            depth_mode: SpriteDepthMode::TestAndWrite,
            depth_bias: 0.0,
        },
    ));
}
```

The world anchor is projected through `PixelStreamTarget.camera`, snapped to an
integer stream pixel, and rendered at `pixel_size` in stream output pixels. The
sprite is drawn before palette conversion and before `DirectText`, so text still
lands on top.

Depth modes:

```text
TestAgainstScene       alpha-blend against scene depth without writing sprite depth
TestAndWrite           alpha-mask visible pixels and write depth, so nearer sprites occlude farther sprites
AlwaysOnTopBeforeText  draw as an overlay before DirectText
```

Texture atlases are supported from the start with `atlas` and `atlas_index`.
`DirectWorldSpriteSettings` controls whether the system is enabled and how many
sprites are synced each frame.

## Custom Browser Hosting

Local custom host:

```powershell
cargo run --bin PixelStream -- --stats-window --custom-host
```

PixelStream is split into two pieces:

- the Rust game process, which runs on the machine hosting the live game and
  serves the stream backend on `127.0.0.1:8080`;
- a static browser player, which can be hosted on Cloudflare and talks to that
  backend through a public tunnel hostname.

The public hosting layout used by this project is:

```text
humanbeangames.com
  Cloudflare static landing page

stream.humanbeangames.com
  Cloudflare static PixelStream player

game.humanbeangames.com
  Cloudflare Tunnel to http://localhost:8080 on the machine running the game
```

### Export The Static Player

Regenerate the static player whenever the browser host changes: branding,
layout, panel/chat/overlay behavior, audio/video client code, endpoint shape, or
any generated `src/web.rs` page content.

```powershell
cargo run --bin ipsc_export_static_stream -- https://game.humanbeangames.com --page-title MERCANTILE --header-title MERCANTILE --max-player-width 640 --minimizable-player
```

This writes:

```text
dist/humanbeangames_stream/index.html
```

Downstream tools can export the same page without invoking the CLI:

```rust
use pixel_stream::{
    CustomHostBranding, CustomHostLayout, export_static_palette_stream_page,
};

export_static_palette_stream_page(
    "dist/humanbeangames_stream",
    "https://game.humanbeangames.com",
    &CustomHostBranding::new("MERCANTILE", "MERCANTILE"),
    &CustomHostLayout::default().prefer_larger_player(),
)?;
```

Upload the contents of this directory, not the directory itself:

```text
dist/humanbeangames_stream
```

to the Cloudflare project that serves `stream.humanbeangames.com`.

### Cloudflare Workers Static Assets

Cloudflare's current static hosting path is Workers Static Assets. A minimal
`wrangler.toml` for the stream player looks like this:

```toml
name = "mercantile-stream"
compatibility_date = "2026-07-26"

[assets]
directory = "./dist/humanbeangames_stream"
binding = "ASSETS"
```

Then deploy from the PixelStream repo:

```powershell
npx wrangler deploy
```

If the project uses `wrangler.jsonc`, the equivalent assets block is:

```jsonc
{
  "name": "mercantile-stream",
  "compatibility_date": "2026-07-26",
  "assets": {
    "directory": "./dist/humanbeangames_stream",
    "binding": "ASSETS"
  }
}
```

The Worker custom domain should be `stream.humanbeangames.com`.

### Cloudflare Pages Direct Upload

Cloudflare Pages Direct Upload also works for this static player. After
exporting, deploy the folder with Wrangler:

```powershell
npx wrangler pages deploy dist/humanbeangames_stream --project-name mercantile-stream
```

or upload the same folder through the Cloudflare dashboard. If you use Direct
Upload, deploy the built folder; do not point Pages at the Rust source tree.

### Landing Page And Lab

Export the dummy landing page from:

```text
dist/humanbeangames
```

Deploy it only when the landing page changes. It embeds
`https://stream.humanbeangames.com`.

Deploy the palette lab from:

```text
dist/ipsc_lab
```

only when the hosted lab changes or you want public tools to pick up a new lab
build.

### Backend And Tunnel

The static stream page talks to `https://game.humanbeangames.com` for:

```text
/status.json
/palette.bin
/audio.pcm
/local-chat
/local-chat-feed
/custom-panels
/stream-click
```

`game.humanbeangames.com` should be a Cloudflare Tunnel or equivalent reverse
proxy to the local Rust server. Keep the Rust server loopback-bound unless you
have added a real public auth/reverse-proxy layer.

Because the player is static, `stream.humanbeangames.com` can show **Not Online**
even when the Rust game app is closed. The raw backend hostname may show a
Cloudflare tunnel error when the app is down; that is expected.

### What To Redeploy

Redeploy `dist/humanbeangames_stream` when:

- `src/web.rs` changes;
- chat, panels, overlays, audio, click handling, status parsing, or browser
  layout changes;
- branding or export flags change;
- the generated `dist/humanbeangames_stream/index.html` changes.

Redeploy `dist/humanbeangames` only when the landing page changes.

Redeploy `dist/ipsc_lab` only when the browser lab changes.

No Cloudflare static redeploy is needed for Rust-only backend/game changes that
do not change the generated browser player. Those changes require rebuilding and
restarting the local game process instead.

## IPSC Video Format

IPSC is an indexed-pixel live stream format for tiny browser-playable games. It
is closer to a live state-sync stream than a GIF.

Stream header:

```text
magic:       [u8; 4] = b"IPSC"
version:     u8
width:       u16
height:      u16
tile_size:   u8 = 8
palette_len: u16
palette:     [rgba; palette_len]
```

Each batch contains a header plus one or more length-prefixed frame payloads.
Frame payloads may contain keyframes, deltas, and batch-local cached tile
references.

Keyframes are raw indexed pixels: `width * height` bytes.

Delta frames contain an 8x8 tile-change bitmask followed by tile payloads for
changed tiles only. The encoder chooses the smallest tile representation:

```text
Skipped   unchanged tile, no payload
Raw       64 palette indices
Solid     one palette index
RLE       row-major color/length runs
Span      changed spans inside the old tile
XorRLE    row-major XOR/length runs against the old tile
Cached    reference to an identical tile earlier in the same batch
```

Custom-host recordings are written to:

```text
recordings/custom-*.ipsc
```

Replay a recording:

```powershell
cargo run --bin ipsc_player -- recordings\custom-1234567890.ipsc
```

The player serves `http://127.0.0.1:8090`.

## Palette And Image Tools

Custom-host mode requires a self-contained `.ipsmap` palette lookup file. Pass
`--palette-lookup=path/to/palette.ipsmap`, or place `palette.ipsmap` in the
current working directory. Missing, invalid, or stale lookup files fail startup
immediately.

The `.ipsmap` file is a direct sRGB-to-palette lookup table plus the binary
palette colours needed to build stream headers. New maps use the `IPSMAP5`
format. IPSMAP5 stores two 16,777,216-entry lookup tables: the altered table for
normal composited scene pixels, then the direct table for explicit colours that
should bypass input offsets. The format does not store palette TOML or
matching/bias settings: those authoring controls are cooked into the lookup
entries when the map is baked. The file hash validates the embedded palette
colours and cooked entries themselves. Older self-contained IPSMAP4 files still
load, but direct-colour pixels fall back to raw nearest-palette matching until
the map is regenerated as IPSMAP5.

Palette TOML remains the editable source format used by the palette tools and
lab. Palette matching during baking has two stages:

1. Convert the input sRGB colour to OKLCH, then apply the optional input
   biases from `[matching]`.
2. Compare the adjusted input colour against the palette colours using the
   priority weights from `[matching]`.

The priority weights are:

```toml
[matching]
lightness = 0.333
chroma = 0.333
hue = 0.334
```

The optional input biases are:

```toml
lightness_multiply = 0.0
lightness_add = 0.0
chroma_multiply = 0.0
chroma_add = 0.0
hue_add = 0.0
```

`lightness_multiply` and `chroma_multiply` are applied as `1.0 + value`, so
`0.5` means “treat this channel as 1.5x higher” and `-0.5` means “treat it as
0.5x”. Additive lightness/chroma offsets are applied after multiplication.
`hue_add` is measured in turns, so `0.25` is a 90 degree hue rotation.
After these offsets are applied, the adjusted OKLCH target is clamped back into
the reachable sRGB gamut by reducing chroma at the same lightness and hue. This
keeps creative chroma boosts from asking the matcher to chase impossible dark,
high-chroma colours.

Old source TOML files without these offset fields still work in the tools; the
missing values default to zero. The runtime no longer reads those settings from
the `.ipsmap`, so always rebake after changing weights, offsets, palette
colours, or PixelStream palette-matching versions.

Migration for existing apps:

1. Update the `PixelStream` dependency to a version that supports cooked
   self-contained `IPSMAP5` lookup files.
2. Keep a palette TOML in your downstream app or asset pipeline if useful, but
   ship `palette.ipsmap` as the runtime artifact.
3. Regenerate `.ipsmap` with `ipsc_build_palette_lut` or the Palette Lab.
4. Launch with `--palette-lookup=palette.ipsmap`, or leave the default
   `palette.ipsmap` in the process working directory.
5. Redeploy the static lab from `dist/ipsc_lab` if viewers or tools use the
   browser Palette Lab.

Combined browser lab:

```powershell
cargo run --bin ipsc_lab
```

Open:

```text
http://127.0.0.1:8092
```

The lab has Palette and Converter tabs. Palette generation can export:

```text
palette.toml
palette.ipsi
palette.ipsmap
```

The converter tab uses the current Palette Lab palette automatically, or a
palette TOML uploaded by the user. PixelStream does not ship a built-in
palette file.

Export the static lab:

```powershell
cargo run --bin ipsc_export_static_lab
```

Upload the contents of:

```text
dist/ipsc_lab
```

to a static host such as Cloudflare Pages.

CLI PNG to IPSI conversion:

```powershell
cargo run --bin ipsc_png_to_ipsi -- input.png output.ipsi palette.toml
cargo run --bin ipsc_png_to_ipsi -- input.png output.ipsi --palette palette.toml --size 128x128
cargo run --bin ipsc_png_to_ipsi -- input.png output.ipsi --palette palette.toml --no-dither
```

View IPSI still images:

```powershell
cargo run --bin ipsc_image_viewer -- output.ipsi
```

## Runtime Security Model

PixelStream's custom host is designed for a local game process and a browser
player, with optional tunneling to a known static player origin. The server binds
to `127.0.0.1:8080` by default, accepts only known endpoint paths, bounds request
headers and bodies, and rejects path traversal targets.

Read endpoints such as `/status.json`, `/palette.bin`, `/audio.pcm`,
`/custom-panels`, `/custom-overlays`, and `/local-chat-feed` are CORS-limited to
known player origins or local tools. Mutating endpoints (`/local-chat`,
`/stream-click`, and `/custom-panel-action`) also require the
`X-PixelStream-Session` token returned by `/status.json`. Viewer device ids are
used for local identity and audience filtering only; they are not authentication.

All browser-rendered app text is delivered as JSON and inserted with DOM text
APIs. CSS colors, classes, font weights, and device ids are validated before
they are accepted.

## Settings API

Generic runtime settings live in the `pixel_stream_settings` crate. Downstream
games publish `PixelSettingCategory` values containing fields such as numbers,
booleans, text, and choices. PixelStream owns the generic editor/presentation
layer; downstream games own the actual settings resources, persistence files,
and apply systems.

```rust
use pixel_stream::{
    PixelSettingCategory, PixelSettingField, PixelSettingNumberRange,
    PixelSettingsAppExt,
};

app.add_pixel_settings_category(
    PixelSettingCategory::new("camera", "Camera").with_field(
        PixelSettingField::number(
            "zoom",
            "Zoom",
            1.0,
            PixelSettingNumberRange::new(0.25, 4.0, 0.01),
        ),
    ),
);
```

Listen for `PixelSettingChanged` when the UI changes a value, then update your
game resource and save your own settings file.

## Chat API Summary

Use `StreamCommandAppExt::add_stream_command` to route local chat commands into
Bevy systems. Command systems receive `StreamChatCommand`, including the command
name, args, stable viewer identity, display name, roles, and optional message id.

Use `StreamChatSender` for app-to-chat messages:

- `send` posts a short-lived global system reply.
- `send_local(LocalChatEntryOptions)` supports global messages, viewer-only
  messages, TTLs, mentions, display-name color, message color, and safe CSS
  classes.
- `LocalChatEntryOptions::for_viewer_identity` and `for_viewer_name` scope
  replies to one browser/device or viewer name.

## Overlay And Sprite API Summary

Browser overlays are published through `CustomHostOverlayHub`. They draw above
the stream canvas and can be global or viewer-scoped. Supported overlay elements
include circles, flags, text, and simple sprites/images, positioned either in
stream pixels or normalized stream coordinates. Use these for UI annotations
such as selected-town rings, flags, and click hints.

`DirectWorldSprite` is different: it is attached to a world entity, projected
through the stream camera, depth-tested against the scene, rendered before
palette conversion, and composited before `DirectText`. Sprite size is specified
in stream pixels, and world sprites are unlit; they do not receive scene shadows.

## Stream Settings Summary

The important runtime stream knobs are:

- resolution: currently square, 64-256, divisible by 8;
- FPS: custom-host validation currently accepts 1-60;
- batch size: controls HTTP/video buffering and audio sync latency;
- `PixelStreamAudioSyncConfig`: fixed, estimated-video-latency, or
  measured-video-latency delay;
- `PixelStreamDitherSettings`: value/chroma/hue Bayer dithering before scene
  quantization;
- `CustomHostBranding` and `CustomHostLayout`: browser title, header, player
  width, and minimization behavior;
- `PixelStreamStartRequest` and `PixelStreamStopRequest`: programmatic
  custom-host control.

Custom-host runtime palette conversion is IPSMAP-only. Palette TOML is still an
authoring/tooling format, but the running app should ship and pass
`palette.ipsmap`.

## Project Structure

Key library modules:

```text
src/app.rs             app shell and plugin setup
src/plugin.rs          PixelStreamPlugin
src/capture.rs         GPU readback
src/frames.rs          frame hubs and direct frame processors
src/palette.rs         IPSC encoder
src/gpu_palette.rs     GPU palette indexing pipeline
src/audio.rs           stream audio mixer
src/chat.rs            local chat and command routing
src/custom_host.rs     custom-host packet/audio/chat/panel server state
src/web.rs             local HTTP server and browser player HTML
src/stream_control.rs  stats-window controls
src/scene.rs           stream target and stats UI
src/direct_text.rs     CPU post-readback text overlay support
src/demo.rs            demo-only game scene/audio/video
crates/pixel_stream_settings
                      generic setting descriptors and changed messages
```

Tools:

```text
src/bin/ipsc_lab.rs
src/bin/ipsc_palette_lab.rs
src/bin/ipsc_png_converter_lab.rs
src/bin/ipsc_export_static_lab.rs
src/bin/ipsc_export_static_stream.rs
src/bin/ipsc_player.rs
src/bin/ipsc_image_viewer.rs
src/bin/ipsc_png_to_ipsi.rs
```

## Current Caveats

- The custom host uses a small hand-written HTTP layer. It is intentionally
  loopback-first and should not be exposed to the public internet without an
  explicit reverse-proxy/auth layer.
- Local chat moderation is in-memory and session-scoped.
- Static player deployment currently assumes `game.humanbeangames.com` as the
  backend origin unless you pass another origin to `ipsc_export_static_stream`.
- The demo video player is intentionally simple and demo-only. It decodes on the
  main thread and is best tested with small H.264 MP4 files.
- FFmpeg is still used for local media/preview tooling; removing that is a
  separate dependency-reduction pass.

## Checks

Useful local checks:

```powershell
cargo check --bin PixelStream
cargo check --bin ipsc_lab --bin ipsc_export_static_stream
cargo test --lib
```
