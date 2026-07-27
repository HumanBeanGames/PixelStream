//! PixelStream is a Bevy app shell for low-resolution, palette-indexed browser
//! streams. Downstream games render into the provided stream target while this
//! crate owns capture, palette conversion, browser transport, chat, panels,
//! overlays, audio, and preview tooling.

#![allow(
    clippy::enum_variant_names,
    clippy::items_after_test_module,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

mod app;
mod audio;
mod capture;
mod chat;
mod config;
mod constants;
mod custom_host;
mod demo;
mod direct_backdrop_sprite;
mod direct_text;
mod direct_world_sprite;
mod frames;
mod gpu_lookup;
mod gpu_palette;
mod palette;
pub mod palette_lut;
mod plugin;
mod preview;
mod public_types;
mod scene;
mod stats;
mod stream_control;
mod surface_primer;
mod web;

pub use app::{direct_stream_app, direct_stream_app as pixel_stream_app, run_with_game};
pub use audio::{
    DirectStreamAudioTarget, DirectStreamAudioTarget as PixelStreamAudioTarget, PlayStreamSound,
    StreamAudioClip,
};
pub use chat::{
    ChatAudience, CustomHostViewerNameRefresh, CustomHostViewerNameResolver, LocalChatEntryOptions,
    LocalViewerProfile, StreamChatCommand, StreamChatMessage, StreamChatRoles, StreamChatSender,
    StreamCommandAppExt, StreamCommandRouter,
};
pub use constants::{
    DIRECT_STREAM_AUDIO_CHANNELS, DIRECT_STREAM_AUDIO_SAMPLE_RATE, DIRECT_STREAM_FPS,
    DIRECT_STREAM_HEIGHT, DIRECT_STREAM_WIDTH, PIXEL_STREAM_AUDIO_CHANNELS,
    PIXEL_STREAM_AUDIO_SAMPLE_RATE, PIXEL_STREAM_FPS, PIXEL_STREAM_HEIGHT, PIXEL_STREAM_WIDTH,
};
pub use custom_host::{
    CustomHostBranding, CustomHostChatPanelHub, CustomHostChatPanelSnapshot, CustomHostLayout,
    CustomHostOverlayElement, CustomHostOverlayHub, CustomHostPanel, CustomHostPanelAction,
    CustomHostPanelAnchor, CustomHostPanelAudience, CustomHostPanelElement,
    CustomHostPanelElementStyle, CustomHostPanelHub, CustomHostPanelPage, CustomHostPanelRegion,
    CustomHostPanelSize, CustomHostPanelStyle, OverlayCoordinateSpace, OverlayElementKind,
    OverlayElementStyle, PagedTextControls, PagedTextControlsPosition, PanelOverflowMode,
    PanelWhiteSpace, StreamPointerClick,
};
pub use demo::{
    DemoMusicClip, DemoMusicStarted, DemoSfxClip, HelloWorldText, handle_demo_boing_command,
    pulse_hello_world_text, run_demo, setup_demo_scene, start_demo_music,
};
pub use direct_backdrop_sprite::{
    DirectBackdropLayer, DirectBackdropSprite, DirectBackdropSpritePlugin,
    DirectBackdropSpriteSettings,
};
pub use direct_text::{DirectText, DirectTextPlugin};
pub use direct_world_sprite::{
    DirectWorldSprite, DirectWorldSpritePlugin, DirectWorldSpriteSettings, SpriteDepthMode,
    SpriteFacing,
};
pub use frames::{
    DirectStreamFrame, DirectStreamFrame as PixelStreamFrame, DirectStreamFrameAppExt,
    DirectStreamFrameAppExt as PixelStreamFrameAppExt,
};
pub use pixel_stream_settings::{
    PixelSettingCategory, PixelSettingChanged, PixelSettingField, PixelSettingKind,
    PixelSettingNumberRange, PixelSettingValue, PixelSettingsAppExt, PixelSettingsPlugin,
    PixelSettingsRegistry,
};
pub use plugin::{DirectStreamPlugin, DirectStreamPlugin as PixelStreamPlugin};
pub use public_types::{
    AudioSyncMode, DirectColorLookup, DirectStreamAudioSyncConfig, DirectStreamControlAction,
    DirectStreamControlResult, DirectStreamDitherSettings, DirectStreamMode, DirectStreamSet,
    DirectStreamStartRequest, DirectStreamState, DirectStreamStopRequest, DirectStreamTarget,
    DirectStreamWindowLayout,
};
pub use public_types::{
    DirectColorLookup as PixelColorLookup,
    DirectStreamAudioSyncConfig as PixelStreamAudioSyncConfig,
    DirectStreamControlAction as PixelStreamControlAction,
    DirectStreamControlResult as PixelStreamControlResult,
    DirectStreamDitherSettings as PixelStreamDitherSettings, DirectStreamMode as PixelStreamMode,
    DirectStreamSet as PixelStreamSet, DirectStreamStartRequest as PixelStreamStartRequest,
    DirectStreamState as PixelStreamState, DirectStreamStopRequest as PixelStreamStopRequest,
    DirectStreamTarget as PixelStreamTarget, DirectStreamWindowLayout as PixelStreamWindowLayout,
};
pub use web::{
    export_static_palette_stream_page, static_palette_stream_page_html,
    static_palette_stream_page_html_with_options,
};
