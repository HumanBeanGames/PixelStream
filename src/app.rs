//! PixelStream app construction.
//!
//! This builds the Bevy app shell, shared resources, stream server, render
//! target, and default plugins before a downstream game attaches its own scene.

use crate::{
    DirectStreamPlugin,
    audio::{CustomAudioPacketHub, DirectStreamAudioTarget, start_custom_audio_packet_pump},
    chat::{CustomHostViewerNameRefresh, LocalChatHub},
    config::{AppConfig, WindowMode, effective_custom_batch_size},
    constants::{
        PREVIEW_EDITOR_HEIGHT, STATS_WINDOW_HEIGHT, STATS_WINDOW_WIDTH, WINDOW_TITLE,
        preview_display_scale,
    },
    custom_host::{
        CustomHostBranding, CustomHostChatPanelHub, CustomHostLayout, CustomHostOverlayHub,
        CustomHostPanelActionHub, CustomHostPanelHub, StreamPointerClickHub,
    },
    frames::{DirectStreamFrameProcessors, EncodedFrameHub, RawFrame, RawFrameSenders},
    palette::{
        PaletteFrameHub, SharedPaletteBias, load_palette_runtime, start_palette_preview_encoder,
    },
    preview::start_preview_encoder,
    public_types::{
        DirectStreamAudioSyncConfig, DirectStreamMode, DirectStreamState, DirectStreamWindowLayout,
    },
    stats::SharedStats,
    stream_control::{CustomStreamState, StreamControl},
};
use bevy::{
    audio::AudioPlugin,
    log::{DEFAULT_FILTER, LogPlugin},
    prelude::*,
    window::PresentMode,
    winit::WinitSettings,
};
use std::num::NonZeroU32;

pub fn direct_stream_app() -> App {
    let config = AppConfig::from_args();
    let frame_hub = EncodedFrameHub::new();
    let palette_frame_hub = PaletteFrameHub::new();
    let audio_target = DirectStreamAudioTarget::new();
    let custom_audio_hub = CustomAudioPacketHub::new();
    let local_chat = LocalChatHub::default();
    let custom_panels = CustomHostPanelHub::default();
    let custom_chat_panel = CustomHostChatPanelHub::default();
    let custom_panel_actions = CustomHostPanelActionHub::default();
    let custom_overlays = CustomHostOverlayHub::default();
    let stream_clicks = StreamPointerClickHub::default();
    let custom_stream_state = CustomStreamState::new();
    let stats = SharedStats::new();
    let palette_bias = SharedPaletteBias::new();
    if config.custom_host {
        let (_, matching) = load_palette_runtime(&config.palette_lookup_path);
        palette_bias.set(matching);
    }
    let (preview_sender, preview_receiver) = crossbeam_channel::bounded(2);
    let custom_frame_capacity = (config.stream_fps as usize * 30)
        .max(effective_custom_batch_size(config.custom_host_batch_size, config.stream_fps) * 8);
    let (custom_sender, custom_receiver) =
        crossbeam_channel::bounded::<RawFrame>(custom_frame_capacity);
    let preview_enabled = config.window_mode == WindowMode::Preview;
    let custom_host = config.custom_host;
    let direct_stream_state = DirectStreamState {
        mode: if custom_host {
            DirectStreamMode::CustomHost
        } else {
            DirectStreamMode::Preview
        },
        active: preview_enabled,
        width: config.stream_width,
        height: config.stream_height,
        fps: config.stream_fps,
    };
    let stream_control = StreamControl::new(
        &config,
        preview_enabled.then_some(preview_sender.clone()),
        custom_host.then_some(custom_sender.clone()),
        custom_stream_state.clone(),
    );
    let window_layout = DirectStreamWindowLayout::default();
    let base_window_resolution = match config.window_mode {
        WindowMode::Preview => {
            let scale = preview_display_scale(config.stream_width, config.stream_height);
            (
                (config.stream_width as f32 * 2.0 * scale).round() as u32,
                (config.stream_height as f32 * scale).round() as u32 + PREVIEW_EDITOR_HEIGHT,
            )
        }
        WindowMode::Stats => (STATS_WINDOW_WIDTH, STATS_WINDOW_HEIGHT),
    };
    let window_resolution = (
        base_window_resolution.0 + window_layout.right_panel_width.round() as u32,
        base_window_resolution.1,
    );

    if preview_enabled {
        custom_stream_state.set_active(true);
        custom_stream_state.set_audio_delay_ms(0);
    }

    if custom_host || preview_enabled {
        start_custom_audio_packet_pump(
            audio_target.clone(),
            custom_audio_hub.clone(),
            stats.clone(),
            custom_stream_state.clone(),
        );
    }

    if custom_host {
        start_palette_preview_encoder(
            custom_receiver,
            palette_frame_hub.clone(),
            stats.clone(),
            palette_bias.clone(),
            custom_stream_state.clone(),
            config.palette_lookup_path.clone(),
            effective_custom_batch_size(config.custom_host_batch_size, config.stream_fps),
        );
    } else if preview_enabled {
        start_preview_encoder(
            preview_receiver,
            frame_hub.clone(),
            stats.clone(),
            config.stream_width,
            config.stream_height,
            config.stream_fps,
        );
    }

    let mut primary_window = Window {
        title: WINDOW_TITLE.to_owned(),
        resolution: window_resolution.into(),
        ..default()
    };
    if custom_host {
        primary_window.present_mode = PresentMode::AutoNoVsync;
        primary_window.desired_maximum_frame_latency =
            Some(NonZeroU32::new(1).expect("one is non-zero"));
    }

    let mut app = App::new();
    if custom_host {
        app.insert_resource(WinitSettings::continuous());
    }

    app.insert_resource(ClearColor(Color::srgb(0.04, 0.05, 0.07)))
        .insert_resource(frame_hub)
        .insert_resource(palette_frame_hub)
        .insert_resource(audio_target)
        .insert_resource(custom_audio_hub)
        .insert_resource(local_chat)
        .insert_resource(custom_panels)
        .insert_resource(custom_chat_panel)
        .insert_resource(custom_panel_actions)
        .insert_resource(custom_overlays)
        .insert_resource(stream_clicks)
        .insert_resource(CustomHostBranding::default())
        .insert_resource(CustomHostLayout::default())
        .insert_resource(DirectStreamAudioSyncConfig::default())
        .insert_resource(window_layout)
        .insert_resource(CustomHostViewerNameRefresh::default())
        .insert_resource(custom_stream_state)
        .insert_resource(palette_bias)
        .insert_resource(DirectStreamFrameProcessors::default())
        .insert_resource(direct_stream_state)
        .insert_resource(stats.clone())
        .insert_resource(stream_control)
        .insert_resource(config)
        .insert_resource(RawFrameSenders {
            preview: preview_enabled.then_some(preview_sender),
            custom: None,
            stats: stats.clone(),
        })
        .add_plugins(
            DefaultPlugins
                .build()
                .disable::<AudioPlugin>()
                .set(LogPlugin {
                    filter: format!("{DEFAULT_FILTER},bevy_ecs::system::system=error"),
                    ..default()
                })
                .set(ImagePlugin::default_nearest())
                .set(WindowPlugin {
                    primary_window: Some(primary_window),
                    ..default()
                }),
        )
        .add_plugins(DirectStreamPlugin);
    app
}

pub fn run_with_game(configure_game: impl FnOnce(&mut App)) {
    let mut app = direct_stream_app();
    configure_game(&mut app);
    app.run();
}
