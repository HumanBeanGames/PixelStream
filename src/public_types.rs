//! Public Bevy resources, messages, and settings exposed by PixelStream.
//!
//! `DirectStream*` names are retained as compatibility aliases while downstream
//! applications can import the exported `PixelStream*` aliases from `lib.rs`.

use bevy::prelude::*;

#[derive(Clone, Debug, Resource)]
/// Extra window space requested by a downstream app for setup/editor UI.
pub struct DirectStreamWindowLayout {
    pub right_panel_width: f32,
}

impl Default for DirectStreamWindowLayout {
    fn default() -> Self {
        Self {
            right_panel_width: 0.0,
        }
    }
}

impl DirectStreamWindowLayout {
    pub fn with_right_panel_width(mut self, width: f32) -> Self {
        self.right_panel_width = width.max(0.0);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Selects whether a stream-space overlay should use the direct or altered
/// palette lookup table.
pub enum DirectColorLookup {
    Direct,
    Altered,
}

#[derive(Clone, Resource)]
/// The render target and cameras PixelStream exposes to downstream games.
///
/// Game cameras that should appear in the stream render into `image`. The
/// palette/readback pipeline converts `output_image` into indexed stream frames.
pub struct DirectStreamTarget {
    pub camera: Entity,
    pub overlay_camera: Entity,
    pub image: Handle<Image>,
    pub output_image: Handle<Image>,
    pub output_is_indexed: bool,
    pub overlay_layer: usize,
    pub raw_overlay_layer: Option<usize>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

#[derive(Clone, Copy, Debug, Resource)]
/// Runtime Bayer dither settings applied to captured scene pixels before
/// palette lookup. DirectText and direct sprites can use direct lookup paths so
/// their requested colors stay stable.
pub struct DirectStreamDitherSettings {
    pub scale: f32,
    pub intensity: f32,
    pub value_strength: f32,
    pub chroma_strength: f32,
    pub hue_strength: f32,
}

impl Default for DirectStreamDitherSettings {
    fn default() -> Self {
        Self {
            scale: 1.0,
            intensity: 0.0,
            value_strength: 0.0,
            chroma_strength: 0.0,
            hue_strength: 0.0,
        }
    }
}

#[derive(Clone, Resource)]
/// Public stream state for systems that should pause or adapt when custom-host
/// streaming is stopped.
pub struct DirectStreamState {
    pub mode: DirectStreamMode,
    pub active: bool,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl DirectStreamState {
    pub fn is_streaming(&self) -> bool {
        self.active
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// The active PixelStream operating mode.
pub enum DirectStreamMode {
    Preview,
    CustomHost,
}

#[derive(Clone, Copy, Debug, Message)]
/// Request a programmatic stream start.
pub struct DirectStreamStartRequest {
    pub mode: DirectStreamMode,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl DirectStreamStartRequest {
    pub fn custom_host(width: u32, height: u32, fps: u32) -> Self {
        Self {
            mode: DirectStreamMode::CustomHost,
            width,
            height,
            fps,
        }
    }
}

#[derive(Clone, Copy, Debug, Message)]
/// Request a programmatic stream stop.
pub struct DirectStreamStopRequest;

#[derive(Clone, Debug, Message)]
/// Result emitted after a programmatic start/stop request is processed.
pub struct DirectStreamControlResult {
    pub action: DirectStreamControlAction,
    pub success: bool,
    pub status: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Start/stop action represented in a stream-control result.
pub enum DirectStreamControlAction {
    Start,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Selects how browser audio should be delayed relative to video playback.
pub enum AudioSyncMode {
    Fixed,
    MatchEstimatedVideoLatency,
    MatchMeasuredVideoLatency,
}

#[derive(Clone, Resource)]
/// Runtime audio synchronization settings.
pub struct DirectStreamAudioSyncConfig {
    pub mode: AudioSyncMode,
    pub fixed_delay_ms: u32,
    pub extra_delay_ms: i32,
}

impl Default for DirectStreamAudioSyncConfig {
    fn default() -> Self {
        Self {
            mode: AudioSyncMode::Fixed,
            fixed_delay_ms: 1_000,
            extra_delay_ms: 0,
        }
    }
}

impl DirectStreamAudioSyncConfig {
    pub fn effective_delay_ms(
        &self,
        estimated_video_latency_ms: u32,
        measured_video_latency_ms: Option<u32>,
    ) -> u32 {
        let base = match self.mode {
            AudioSyncMode::Fixed => self.fixed_delay_ms,
            AudioSyncMode::MatchEstimatedVideoLatency => estimated_video_latency_ms,
            AudioSyncMode::MatchMeasuredVideoLatency => {
                measured_video_latency_ms.unwrap_or(estimated_video_latency_ms)
            }
        };
        (base as i64 + self.extra_delay_ms as i64).max(0) as u32
    }
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// System set that runs after PixelStream has created its render target.
pub enum DirectStreamSet {
    Setup,
}
