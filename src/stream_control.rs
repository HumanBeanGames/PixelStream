//! Start/stop control path for custom-host streaming.
//!
//! UI buttons and programmatic requests both flow through the same validation
//! and state update logic.

use crate::{
    audio::DirectStreamAudioTarget,
    chat::LocalChatHub,
    config::{AppConfig, effective_custom_batch_size},
    constants::INITIAL_RENDER_SETTLE_FRAMES,
    frames::{RawFrame, RawFrameSenders},
    gpu_palette::{
        GpuPalettePipeline, PaletteMaterial, RawPreviewCopyMaterial, RawPreviewSnapshotMaterial,
        make_stream_source_image, retarget_custom_host_pipeline,
    },
    palette::PaletteFrameHub,
    palette::load_palette_lookup_runtime,
    public_types::{
        DirectStreamAudioSyncConfig, DirectStreamControlAction, DirectStreamControlResult,
        DirectStreamMode, DirectStreamStartRequest, DirectStreamState, DirectStreamStopRequest,
        DirectStreamTarget,
    },
    scene::{ReadbackPixelFormat, StreamReadback},
    stats::SharedStats,
};
use bevy::{
    camera::RenderTarget, ecs::system::SystemParam, input::keyboard::KeyboardInput, prelude::*,
    window::WindowOccluded,
};
use crossbeam_channel::Sender;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
};

#[derive(Resource)]
pub(crate) struct StreamControl {
    pub(crate) custom_width: String,
    pub(crate) custom_height: String,
    pub(crate) custom_fps: String,
    pub(crate) focused_input: Option<StreamControlInput>,
    pub(crate) status: String,
    preview_sender: Option<Sender<RawFrame>>,
    custom_sender: Option<Sender<RawFrame>>,
    custom_stream_state: CustomStreamState,
}

#[derive(SystemParam)]
pub(crate) struct StreamControlAssets<'w> {
    images: ResMut<'w, Assets<Image>>,
    palette_materials: ResMut<'w, Assets<PaletteMaterial>>,
    raw_copy_materials: ResMut<'w, Assets<RawPreviewCopyMaterial>>,
    raw_snapshot_materials: ResMut<'w, Assets<RawPreviewSnapshotMaterial>>,
}

impl StreamControl {
    pub(crate) fn new(
        config: &AppConfig,
        preview_sender: Option<Sender<RawFrame>>,
        custom_sender: Option<Sender<RawFrame>>,
        custom_stream_state: CustomStreamState,
    ) -> Self {
        Self {
            custom_width: config.stream_width.to_string(),
            custom_height: config.stream_height.to_string(),
            custom_fps: config.stream_fps.to_string(),
            focused_input: None,
            status: "Ready".to_owned(),
            preview_sender,
            custom_sender,
            custom_stream_state,
        }
    }

    pub(crate) fn is_streaming(&self) -> bool {
        self.custom_stream_state.is_active()
    }

    fn start_custom_host(
        &mut self,
        width: u32,
        height: u32,
        fps: u32,
        senders: &mut RawFrameSenders,
        stats: &SharedStats,
        images: &mut Assets<Image>,
        palette_materials: &mut Assets<PaletteMaterial>,
        raw_copy_materials: &mut Assets<RawPreviewCopyMaterial>,
        raw_snapshot_materials: &mut Assets<RawPreviewSnapshotMaterial>,
        target: &mut DirectStreamTarget,
        direct_stream_state: &mut DirectStreamState,
        readback: &mut StreamReadback,
        gpu_palette: Option<&mut GpuPalettePipeline>,
        frame_hub: &PaletteFrameHub,
        audio_sync: &DirectStreamAudioSyncConfig,
        camera_targets: &mut Query<&mut RenderTarget>,
        quad_transforms: &mut Query<&mut Transform>,
        config: &AppConfig,
    ) -> DirectStreamControlResult {
        if self.is_streaming() {
            self.status = "Already streaming".to_owned();
            return self.control_result(DirectStreamControlAction::Start, false);
        }

        let Some(custom_sender) = self.custom_sender.clone() else {
            self.status = "Custom host unavailable".to_owned();
            return self.control_result(DirectStreamControlAction::Start, false);
        };
        if !valid_custom_dimensions(width, height, fps) {
            self.status = "Use an 8-aligned square size 64-256 and fps 1-60".to_owned();
            return self.control_result(DirectStreamControlAction::Start, false);
        };
        let Some(gpu_palette) = gpu_palette else {
            self.status = "GPU palette pipeline unavailable".to_owned();
            return self.control_result(DirectStreamControlAction::Start, false);
        };

        let batch_size = effective_custom_batch_size(config.custom_host_batch_size, fps);
        let palette_lookup = load_palette_lookup_runtime(&config.palette_lookup_path);
        let image = images.add(make_stream_source_image(width, height));

        if let Ok(mut camera_target) = camera_targets.get_mut(target.camera) {
            *camera_target = RenderTarget::Image(image.clone().into());
        } else {
            self.status = "Could not retarget stream camera".to_owned();
            return self.control_result(DirectStreamControlAction::Start, false);
        }

        if retarget_custom_host_pipeline(
            gpu_palette,
            images,
            palette_materials,
            raw_copy_materials,
            raw_snapshot_materials,
            camera_targets,
            quad_transforms,
            width,
            height,
            image.clone(),
            &palette_lookup,
            target,
            batch_size,
        )
        .is_err()
        {
            self.status = "Could not retarget GPU output pipeline".to_owned();
            return self.control_result(DirectStreamControlAction::Start, false);
        }

        target.image = image;
        target.width = width;
        target.height = height;
        target.fps = fps;
        direct_stream_state.active = true;
        direct_stream_state.width = width;
        direct_stream_state.height = height;
        direct_stream_state.fps = fps;
        readback.images = gpu_palette.output_images.clone();
        readback.pixel_format = ReadbackPixelFormat::IndexedRgba8;
        readback.batch_size = batch_size;
        readback.next_readback_entity = 0;
        readback.batch_started_at = None;
        readback.batch_in_progress = false;
        readback.frame_due = false;
        readback.textures_rendered_in_batch = 0;
        readback.frame_waiting_for_render = None;
        readback.rendered_batch_frames.clear();
        readback.rendered_batch_frames.reserve(batch_size);
        readback.render_settle_frames_remaining = INITIAL_RENDER_SETTLE_FRAMES;
        readback.frame_interval = std::time::Duration::from_secs_f64(1.0 / fps as f64);
        readback.frame_accumulator = std::time::Duration::ZERO;
        readback.pending_requests.clear();

        frame_hub.clear();
        senders.preview = None;
        senders.custom = Some(custom_sender);
        self.custom_stream_state.set_dimensions(width, height);
        self.custom_stream_state.set_fps(fps);
        self.custom_stream_state.set_batch_size(batch_size);
        let estimated_latency_ms =
            estimated_video_latency_ms(batch_size, fps, readback.frame_interval);
        let audio_delay_ms = audio_sync.effective_delay_ms(estimated_latency_ms, None);
        self.custom_stream_state.set_audio_delay_ms(audio_delay_ms);
        self.custom_stream_state.set_active(true);
        self.status = "Custom host streaming".to_owned();
        stats.with_mut(|stats| {
            stats.reset_custom_session();
            stats.custom_audio_delay_ms = audio_delay_ms;
        });
        self.control_result(DirectStreamControlAction::Start, true)
    }

    fn stop_custom_host(
        &mut self,
        senders: &mut RawFrameSenders,
        stats: &SharedStats,
        audio_target: &DirectStreamAudioTarget,
        direct_stream_state: &mut DirectStreamState,
        readback: &mut StreamReadback,
    ) -> DirectStreamControlResult {
        if !self.is_streaming() {
            self.status = "Not streaming".to_owned();
            return self.control_result(DirectStreamControlAction::Stop, false);
        }

        self.custom_stream_state.set_active(false);
        direct_stream_state.active = false;
        senders.custom = None;
        if self.preview_sender.is_some() {
            senders.preview = self.preview_sender.clone();
        }
        readback.pending_requests.clear();
        readback.batch_started_at = None;
        readback.batch_in_progress = false;
        readback.frame_due = false;
        readback.frame_accumulator = std::time::Duration::ZERO;
        readback.textures_rendered_in_batch = 0;
        readback.frame_waiting_for_render = None;
        readback.rendered_batch_frames.clear();
        readback.render_settle_frames_remaining = INITIAL_RENDER_SETTLE_FRAMES;
        audio_target.clear();
        self.status = "Custom host stopped".to_owned();
        stats.with_mut(|stats| {
            stats.custom_stage = "stopped";
            stats.custom_audio_packets_sent = 0;
            stats.custom_audio_bytes_sent = 0;
        });
        self.control_result(DirectStreamControlAction::Stop, true)
    }

    fn open_custom_host(&mut self) {
        match open_url("http://127.0.0.1:8080") {
            Ok(()) => self.status = "Opened custom host preview".to_owned(),
            Err(err) => self.status = format!("Could not open custom host: {err}"),
        }
    }

    fn custom_dimensions(&self) -> Result<(u32, u32, u32), ()> {
        let width = self.custom_width.trim().parse::<u32>().map_err(|_| ())?;
        let height = self.custom_height.trim().parse::<u32>().map_err(|_| ())?;
        let fps = self.custom_fps.trim().parse::<u32>().map_err(|_| ())?;

        if !valid_custom_dimensions(width, height, fps) {
            return Err(());
        }

        Ok((width, height, fps))
    }

    fn control_result(
        &self,
        action: DirectStreamControlAction,
        success: bool,
    ) -> DirectStreamControlResult {
        DirectStreamControlResult {
            action,
            success,
            status: self.status.clone(),
        }
    }
}

#[derive(Clone, Resource)]
pub(crate) struct CustomStreamState {
    active: Arc<AtomicBool>,
    width: Arc<AtomicU32>,
    height: Arc<AtomicU32>,
    fps: Arc<AtomicU32>,
    batch_size: Arc<AtomicUsize>,
    audio_delay_ms: Arc<AtomicU32>,
    session_token: Arc<String>,
}

impl CustomStreamState {
    pub(crate) fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            width: Arc::new(AtomicU32::new(1)),
            height: Arc::new(AtomicU32::new(1)),
            fps: Arc::new(AtomicU32::new(1)),
            batch_size: Arc::new(AtomicUsize::new(1)),
            audio_delay_ms: Arc::new(AtomicU32::new(1_000)),
            session_token: Arc::new(make_session_token()),
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub(crate) fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }

    pub(crate) fn fps(&self) -> u32 {
        self.fps.load(Ordering::Relaxed).max(1)
    }

    pub(crate) fn batch_size(&self) -> usize {
        self.batch_size.load(Ordering::Relaxed).max(1)
    }

    pub(crate) fn width(&self) -> u32 {
        self.width.load(Ordering::Relaxed).max(1)
    }

    pub(crate) fn height(&self) -> u32 {
        self.height.load(Ordering::Relaxed).max(1)
    }

    fn set_dimensions(&self, width: u32, height: u32) {
        self.width.store(width.max(1), Ordering::Relaxed);
        self.height.store(height.max(1), Ordering::Relaxed);
    }

    fn set_fps(&self, fps: u32) {
        self.fps.store(fps.max(1), Ordering::Relaxed);
    }

    fn set_batch_size(&self, batch_size: usize) {
        self.batch_size.store(batch_size.max(1), Ordering::Relaxed);
    }

    pub(crate) fn audio_delay_ms(&self) -> u32 {
        self.audio_delay_ms.load(Ordering::Relaxed)
    }

    pub(crate) fn set_audio_delay_ms(&self, audio_delay_ms: u32) {
        self.audio_delay_ms
            .store(audio_delay_ms.min(10_000), Ordering::Relaxed);
    }

    pub(crate) fn session_token(&self) -> &str {
        &self.session_token
    }
}

fn make_session_token() -> String {
    let mut bytes = [0u8; 24];
    if getrandom::fill(&mut bytes).is_err() {
        let fallback = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
            ^ u128::from(std::process::id());
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = ((fallback >> ((index % 16) * 8)) & 0xff) as u8;
        }
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn estimated_video_latency_ms(
    batch_size: usize,
    fps: u32,
    frame_interval: std::time::Duration,
) -> u32 {
    let fps = fps.max(1) as f64;
    let batch_ms = batch_size.saturating_sub(1) as f64 * 1000.0 / fps;
    let readback_ms = frame_interval.as_secs_f64() * 1000.0;
    (batch_ms + readback_ms).round().max(0.0) as u32
}

fn valid_custom_dimensions(width: u32, height: u32, fps: u32) -> bool {
    width == height
        && (64..=256).contains(&width)
        && (64..=256).contains(&height)
        && width.is_multiple_of(8)
        && (1..=60).contains(&fps)
}

#[derive(Component)]
pub(crate) struct CustomWidthInputBox;
#[derive(Component)]
pub(crate) struct CustomHeightInputBox;
#[derive(Component)]
pub(crate) struct CustomFpsInputBox;
#[derive(Component)]
pub(crate) struct CustomWidthInputText;
#[derive(Component)]
pub(crate) struct CustomHeightInputText;
#[derive(Component)]
pub(crate) struct CustomFpsInputText;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamControlInput {
    CustomWidth,
    CustomHeight,
    CustomFps,
}

#[derive(Component)]
pub(crate) struct StreamControlStatusText;
#[derive(Component)]
pub(crate) struct StartStreamButton;
#[derive(Component)]
pub(crate) struct StopStreamButton;
#[derive(Component)]
pub(crate) struct OpenStreamButton;
#[derive(Component)]
pub(crate) struct PurgeChatButton;

pub(crate) fn handle_stream_key_typing(
    keyboard: Res<ButtonInput<KeyCode>>,
    mut key_events: MessageReader<KeyboardInput>,
    mut control: ResMut<StreamControl>,
) {
    let Some(focused_input) = control.focused_input else {
        return;
    };

    if keyboard.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight])
        && keyboard.just_pressed(KeyCode::KeyV)
    {
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
            Ok(text) => {
                push_focused_text(&mut control, focused_input, text.trim());
                control.status = "Pasted".to_owned();
            }
            Err(err) => control.status = format!("Clipboard unavailable: {err}"),
        }
    }

    for event in key_events.read() {
        if !event.state.is_pressed() {
            continue;
        }

        match event.key_code {
            KeyCode::Backspace => {
                pop_focused_text(&mut control, focused_input);
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                control.focused_input = None;
            }
            _ => {
                if keyboard.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]) {
                    continue;
                }
                if let Some(text) = &event.text {
                    push_focused_text(&mut control, focused_input, text);
                }
            }
        }
    }
}

pub(crate) fn handle_stream_input_box_interactions(
    mut control: ResMut<StreamControl>,
    mut input_boxes: ParamSet<(
        Query<
            (&Interaction, &mut BackgroundColor),
            (Changed<Interaction>, With<CustomWidthInputBox>),
        >,
        Query<
            (&Interaction, &mut BackgroundColor),
            (Changed<Interaction>, With<CustomHeightInputBox>),
        >,
        Query<
            (&Interaction, &mut BackgroundColor),
            (Changed<Interaction>, With<CustomFpsInputBox>),
        >,
    )>,
) {
    handle_input_box_interactions(
        &mut control,
        &mut input_boxes.p0(),
        StreamControlInput::CustomWidth,
    );
    handle_input_box_interactions(
        &mut control,
        &mut input_boxes.p1(),
        StreamControlInput::CustomHeight,
    );
    handle_input_box_interactions(
        &mut control,
        &mut input_boxes.p2(),
        StreamControlInput::CustomFps,
    );
}

pub(crate) fn handle_stream_start_interactions(
    mut control: ResMut<StreamControl>,
    mut requests: MessageWriter<DirectStreamStartRequest>,
    mut start_buttons: Query<
        (&Interaction, &mut BackgroundColor),
        (Changed<Interaction>, With<StartStreamButton>),
    >,
) {
    for (interaction, mut color) in &mut start_buttons {
        if *interaction == Interaction::Pressed {
            match control.custom_dimensions() {
                Ok((width, height, fps)) => {
                    requests.write(DirectStreamStartRequest::custom_host(width, height, fps));
                }
                Err(()) => {
                    control.status = "Use an 8-aligned square size 64-256 and fps 1-60".to_owned();
                }
            }
        }
        *color = button_color(*interaction, Color::srgb(0.05, 0.20, 0.13));
    }
}

pub(crate) fn handle_stream_stop_interactions(
    mut requests: MessageWriter<DirectStreamStopRequest>,
    mut stop_buttons: Query<
        (&Interaction, &mut BackgroundColor),
        (Changed<Interaction>, With<StopStreamButton>),
    >,
) {
    for (interaction, mut color) in &mut stop_buttons {
        if *interaction == Interaction::Pressed {
            requests.write(DirectStreamStopRequest);
        }
        *color = button_color(*interaction, Color::srgb(0.21, 0.06, 0.07));
    }
}

pub(crate) fn drain_window_occluded_events(mut occluded_events: MessageReader<WindowOccluded>) {
    for _ in occluded_events.read() {}
}

pub(crate) fn handle_direct_stream_start_requests(
    mut control: ResMut<StreamControl>,
    mut requests: MessageReader<DirectStreamStartRequest>,
    mut results: MessageWriter<DirectStreamControlResult>,
    mut senders: ResMut<RawFrameSenders>,
    stats: Res<SharedStats>,
    mut readback: Option<ResMut<StreamReadback>>,
    mut assets: StreamControlAssets,
    mut target: ResMut<DirectStreamTarget>,
    mut direct_stream_state: ResMut<DirectStreamState>,
    mut gpu_palette: Option<ResMut<GpuPalettePipeline>>,
    frame_hub: Res<PaletteFrameHub>,
    audio_sync: Res<DirectStreamAudioSyncConfig>,
    mut camera_targets: Query<&mut RenderTarget>,
    mut quad_transforms: Query<&mut Transform>,
    config: Res<AppConfig>,
) {
    for request in requests.read() {
        let result = match request.mode {
            DirectStreamMode::CustomHost => {
                if let Some(readback) = readback.as_deref_mut() {
                    let gpu_palette = gpu_palette.as_deref_mut();
                    control.start_custom_host(
                        request.width,
                        request.height,
                        request.fps,
                        &mut senders,
                        &stats,
                        &mut assets.images,
                        &mut assets.palette_materials,
                        &mut assets.raw_copy_materials,
                        &mut assets.raw_snapshot_materials,
                        &mut target,
                        &mut direct_stream_state,
                        readback,
                        gpu_palette,
                        &frame_hub,
                        &audio_sync,
                        &mut camera_targets,
                        &mut quad_transforms,
                        &config,
                    )
                } else {
                    control.status = "Custom host unavailable".to_owned();
                    control.control_result(DirectStreamControlAction::Start, false)
                }
            }
            DirectStreamMode::Preview => {
                control.status =
                    "Preview mode cannot be started through custom host control".to_owned();
                control.control_result(DirectStreamControlAction::Start, false)
            }
        };
        results.write(result);
    }
}

pub(crate) fn handle_direct_stream_stop_requests(
    mut control: ResMut<StreamControl>,
    mut requests: MessageReader<DirectStreamStopRequest>,
    mut results: MessageWriter<DirectStreamControlResult>,
    mut senders: ResMut<RawFrameSenders>,
    stats: Res<SharedStats>,
    audio_target: Res<DirectStreamAudioTarget>,
    mut direct_stream_state: ResMut<DirectStreamState>,
    mut readback: Option<ResMut<StreamReadback>>,
) {
    for _ in requests.read() {
        let result = if let Some(readback) = readback.as_deref_mut() {
            control.stop_custom_host(
                &mut senders,
                &stats,
                &audio_target,
                &mut direct_stream_state,
                readback,
            )
        } else {
            control.status = "Custom host unavailable".to_owned();
            control.control_result(DirectStreamControlAction::Stop, false)
        };
        results.write(result);
    }
}

pub(crate) fn handle_stream_misc_button_interactions(
    mut control: ResMut<StreamControl>,
    local_chat: Option<Res<LocalChatHub>>,
    mut buttons: ParamSet<(
        Query<(&Interaction, &mut BackgroundColor), (Changed<Interaction>, With<OpenStreamButton>)>,
        Query<(&Interaction, &mut BackgroundColor), (Changed<Interaction>, With<PurgeChatButton>)>,
    )>,
) {
    for (interaction, mut color) in &mut buttons.p0() {
        if *interaction == Interaction::Pressed {
            control.open_custom_host();
        }
        *color = button_color(*interaction, Color::srgb(0.07, 0.10, 0.19));
    }

    for (interaction, mut color) in &mut buttons.p1() {
        if *interaction == Interaction::Pressed
            && let Some(chat) = &local_chat
        {
            chat.purge();
            control.status = "Purged local chat".to_owned();
        }
        *color = button_color(*interaction, Color::srgb(0.17, 0.10, 0.04));
    }
}

pub(crate) fn update_stream_control_ui(
    control: Res<StreamControl>,
    mut texts: ParamSet<(
        Query<&mut Text, With<StreamControlStatusText>>,
        Query<&mut Text, With<CustomWidthInputText>>,
        Query<&mut Text, With<CustomHeightInputText>>,
        Query<&mut Text, With<CustomFpsInputText>>,
    )>,
) {
    if !control.is_changed() {
        return;
    }

    if let Ok(mut text) = texts.p0().single_mut() {
        **text = format!(
            "stream control: {} - {}",
            if control.is_streaming() {
                "streaming"
            } else {
                "idle"
            },
            control.status
        );
    }
    if let Ok(mut text) = texts.p1().single_mut() {
        **text = control.custom_width.clone();
    }
    if let Ok(mut text) = texts.p2().single_mut() {
        **text = control.custom_height.clone();
    }
    if let Ok(mut text) = texts.p3().single_mut() {
        **text = control.custom_fps.clone();
    }
}

fn handle_input_box_interactions<T: Component>(
    control: &mut StreamControl,
    query: &mut Query<(&Interaction, &mut BackgroundColor), (Changed<Interaction>, With<T>)>,
    input: StreamControlInput,
) {
    for (interaction, mut color) in query {
        if *interaction == Interaction::Pressed {
            control.focused_input = Some(input);
        }
        *color = button_color(*interaction, Color::srgb(0.045, 0.055, 0.07));
    }
}

fn push_focused_text(control: &mut StreamControl, focused_input: StreamControlInput, text: &str) {
    for ch in text.chars().filter(|ch| ch.is_ascii_digit()) {
        match focused_input {
            StreamControlInput::CustomWidth => control.custom_width.push(ch),
            StreamControlInput::CustomHeight => control.custom_height.push(ch),
            StreamControlInput::CustomFps => control.custom_fps.push(ch),
        }
    }
}

fn pop_focused_text(control: &mut StreamControl, focused_input: StreamControlInput) {
    match focused_input {
        StreamControlInput::CustomWidth => {
            control.custom_width.pop();
        }
        StreamControlInput::CustomHeight => {
            control.custom_height.pop();
        }
        StreamControlInput::CustomFps => {
            control.custom_fps.pop();
        }
    }
}

fn button_color(interaction: Interaction, base: Color) -> BackgroundColor {
    match interaction {
        Interaction::Pressed => BackgroundColor(Color::srgb(0.24, 0.32, 0.46)),
        Interaction::Hovered => BackgroundColor(Color::srgb(0.13, 0.17, 0.24)),
        Interaction::None => BackgroundColor(base),
    }
}

fn open_url(url: &str) -> std::io::Result<()> {
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
        .map(|_| ())
}
