//! Library-wide defaults and endpoint names.
//!
//! Public aliases keep older DirectStream naming usable while new code can use
//! PixelStream constants.

pub(crate) const WINDOW_TITLE: &str = "PixelStream";
pub(crate) const WEB_ADDR: &str = "127.0.0.1:8080";
pub(crate) const STREAM_PATH: &str = "/stream.mjpg";
pub(crate) const PALETTE_STREAM_PATH: &str = "/palette.bin";
pub(crate) const AUDIO_STREAM_PATH: &str = "/audio.pcm";
pub(crate) const LOCAL_CHAT_PATH: &str = "/local-chat";
pub(crate) const LOCAL_CHAT_FEED_PATH: &str = "/local-chat-feed";
pub(crate) const CUSTOM_PANELS_PATH: &str = "/custom-panels";
pub(crate) const CUSTOM_PANEL_ACTION_PATH: &str = "/custom-panel-action";
pub(crate) const CUSTOM_OVERLAYS_PATH: &str = "/custom-overlays";
pub(crate) const STREAM_CLICK_PATH: &str = "/stream-click";
pub(crate) const STREAM_STATUS_PATH: &str = "/status.json";
pub(crate) const STREAM_WIDTH: u32 = 256;
pub(crate) const STREAM_HEIGHT: u32 = 256;
pub(crate) const PREVIEW_DISPLAY_PIXELS: f32 = 768.0;
pub(crate) const PREVIEW_EDITOR_HEIGHT: u32 = 360;
pub(crate) const INITIAL_RENDER_SETTLE_FRAMES: usize = 2;
pub(crate) const STATS_WINDOW_WIDTH: u32 = 560;
pub(crate) const STATS_WINDOW_HEIGHT: u32 = 680;
pub(crate) const STREAM_FPS: u32 = 5;
pub(crate) const STREAM_AUDIO_SAMPLE_RATE: u32 = 48_000;
pub(crate) const STREAM_AUDIO_CHANNELS: usize = 2;
pub(crate) const CUSTOM_AUDIO_SAMPLE_RATE: u32 = 8_000;
pub(crate) const CUSTOM_AUDIO_CHANNELS: usize = 1;
// Must cover the maximum custom-host audio delay. MatchEstimatedVideoLatency can
// reach several seconds with large frame batches, and CustomStreamState clamps
// that delay to 10s.
pub(crate) const STREAM_AUDIO_BUFFER_SECONDS: usize = 12;
pub(crate) const STREAM_AUDIO_MAX_MIX_FRAMES_PER_UPDATE: usize =
    STREAM_AUDIO_SAMPLE_RATE as usize / 10;

pub const DIRECT_STREAM_WIDTH: u32 = STREAM_WIDTH;
pub const DIRECT_STREAM_HEIGHT: u32 = STREAM_HEIGHT;
pub const DIRECT_STREAM_FPS: u32 = STREAM_FPS;
pub const DIRECT_STREAM_AUDIO_SAMPLE_RATE: u32 = STREAM_AUDIO_SAMPLE_RATE;
pub const DIRECT_STREAM_AUDIO_CHANNELS: usize = STREAM_AUDIO_CHANNELS;
pub const PIXEL_STREAM_WIDTH: u32 = DIRECT_STREAM_WIDTH;
pub const PIXEL_STREAM_HEIGHT: u32 = DIRECT_STREAM_HEIGHT;
pub const PIXEL_STREAM_FPS: u32 = DIRECT_STREAM_FPS;
pub const PIXEL_STREAM_AUDIO_SAMPLE_RATE: u32 = DIRECT_STREAM_AUDIO_SAMPLE_RATE;
pub const PIXEL_STREAM_AUDIO_CHANNELS: usize = DIRECT_STREAM_AUDIO_CHANNELS;

pub(crate) fn preview_display_scale(width: u32, height: u32) -> f32 {
    PREVIEW_DISPLAY_PIXELS / width.max(height).max(1) as f32
}
