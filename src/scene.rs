//! Built-in PixelStream preview, stats, and setup UI scene.
//!
//! The code here is intentionally generic: downstream games expose settings,
//! but PixelStream owns the preview windows, palette debug views, status text,
//! and generic setup controls.

use crate::{
    config::{AppConfig, WindowMode, effective_custom_batch_size},
    constants::{
        INITIAL_RENDER_SETTLE_FRAMES, PREVIEW_EDITOR_HEIGHT, STREAM_FPS, STREAM_HEIGHT,
        STREAM_WIDTH, WEB_ADDR, preview_display_scale,
    },
    gpu_lookup::build_lookup_gpu_with_progress,
    gpu_palette::{
        GPU_PREVIEW_DISPLAY_LAYER, PaletteMaterial, PalettePreviewDisplayMaterial,
        PreviewPaletteThrottle, PreviewRawDisplay, RawPreviewCopyMaterial,
        RawPreviewDisplayMaterial, RawPreviewSnapshotMaterial, make_stream_source_image,
        spawn_custom_host_pipeline,
    },
    palette::load_palette_lookup_runtime,
    palette_lut::{
        PaletteConfig, PaletteLookup, PaletteMatching, build_lookup_with_progress, write_lookup,
    },
    public_types::{DirectStreamDitherSettings, DirectStreamTarget, DirectStreamWindowLayout},
    stats::{SharedStats, StatsText},
    stream_control::{
        CustomFpsInputBox, CustomFpsInputText, CustomHeightInputBox, CustomHeightInputText,
        CustomWidthInputBox, CustomWidthInputText, OpenStreamButton, PurgeChatButton,
        StartStreamButton, StopStreamButton, StreamControlStatusText,
    },
};
use bevy::{
    asset::RenderAssetUsages,
    camera::{RenderTarget, visibility::RenderLayers},
    picking::hover::Hovered,
    prelude::*,
    render::{
        gpu_readback::{Readback, ReadbackComplete},
        render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages},
    },
    ui::{Checked, RelativeCursorPosition},
    window::PrimaryWindow,
};
use bevy_ui_widgets::{Checkbox, Slider, SliderRange, SliderThumb, SliderValue, TrackClick};
use crossbeam_channel::Receiver;
use std::{
    collections::HashMap,
    fs,
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const PREVIEW_PALETTE_VALIDATION_FRAMES: u32 = 30;
const PREVIEW_PALETTE_MISMATCH_LOG: &str = "preview_palette_audit.txt";

#[derive(Resource)]
pub(crate) struct StatsWindowUpdateClock {
    timer: Timer,
}

impl Default for StatsWindowUpdateClock {
    fn default() -> Self {
        Self {
            timer: Timer::new(Duration::from_millis(200), TimerMode::Repeating),
        }
    }
}

pub(crate) struct PendingReadback {
    pub(crate) captured_at: Instant,
    pub(crate) output_index: usize,
}

pub(crate) struct RenderedBatchFrame {
    pub(crate) output_index: usize,
    pub(crate) captured_at: Instant,
}

#[derive(Resource)]
pub(crate) struct StreamReadback {
    pub(crate) images: Vec<Handle<Image>>,
    pub(crate) pixel_format: ReadbackPixelFormat,
    pub(crate) readback_entities: Vec<Entity>,
    pub(crate) next_readback_entity: usize,
    pub(crate) frame_interval: Duration,
    pub(crate) frame_accumulator: Duration,
    pub(crate) frame_due: bool,
    pub(crate) pending_requests: HashMap<Entity, PendingReadback>,
    pub(crate) batch_size: usize,
    pub(crate) batch_started_at: Option<Instant>,
    pub(crate) batch_in_progress: bool,
    pub(crate) textures_rendered_in_batch: usize,
    pub(crate) frame_waiting_for_render: Option<RenderedBatchFrame>,
    pub(crate) rendered_batch_frames: Vec<RenderedBatchFrame>,
    pub(crate) render_settle_frames_remaining: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadbackPixelFormat {
    Bgra,
    IndexedRgba8,
}

impl ReadbackPixelFormat {
    pub(crate) fn row_bytes(self, width: u32) -> usize {
        match self {
            Self::Bgra => width as usize * 4,
            Self::IndexedRgba8 => width as usize * 4,
        }
    }

    pub(crate) fn is_bgra(self) -> bool {
        self == Self::Bgra
    }
}

#[derive(Component)]
pub(crate) struct PreviewDisplayCamera;

#[derive(Component)]
pub(crate) struct PreviewPixelDebugText;

#[derive(Component)]
pub(crate) struct PreviewStreamVisual;

#[derive(Component)]
pub(crate) struct PreviewLoadingText;

#[derive(Component)]
pub(crate) struct PreviewRebakingOverlay;

#[derive(Component)]
pub(crate) struct PreviewRebakingOverlayText;

#[derive(Component)]
pub(crate) struct PreviewQuantizedDisplay;

#[derive(Component)]
pub(crate) struct PreviewPaletteEditorRoot;

#[derive(Component)]
pub(crate) struct PreviewPaletteCell(usize);

#[derive(Component)]
pub(crate) struct PreviewPaletteGenerateButton;

#[derive(Component)]
pub(crate) struct PreviewPaletteCheckbox(PreviewPaletteCheckboxKind);

#[derive(Component)]
pub(crate) struct PreviewPaletteCheckboxMark(PreviewPaletteCheckboxKind);

#[derive(Component)]
pub(crate) struct PreviewPaletteRebakeButton;

#[derive(Component)]
pub(crate) struct PreviewPaletteApplyPickerButton;

#[derive(Component)]
pub(crate) struct PreviewPaletteOklchCanvas(PreviewOklchCanvasKind);

#[derive(Component)]
pub(crate) struct PreviewPaletteOklchMarker {
    kind: PreviewOklchCanvasKind,
    axis: PreviewOklchMarkerAxis,
}

#[derive(Component)]
pub(crate) struct PreviewPaletteSlider(PreviewLabSlider);

#[derive(Component)]
pub(crate) struct PreviewPaletteSliderThumb;

#[derive(Component)]
pub(crate) struct PreviewPaletteSliderValueText(PreviewLabSlider);

#[derive(Component)]
pub(crate) struct PreviewPalettePickButton;

#[derive(Component)]
pub(crate) struct PreviewPaletteLoadButton;

#[derive(Component)]
pub(crate) struct PreviewPaletteSaveButton;

#[derive(Component)]
pub(crate) struct PreviewPaletteLabButton;

#[derive(Component)]
pub(crate) struct PreviewPaletteStatusText;

#[derive(Resource)]
pub(crate) struct PreviewPaletteEditor {
    colors: Vec<[u8; 4]>,
    selected: usize,
    pick_raw_next_click: bool,
    lab_settings: PreviewLabSettings,
    committed_lab_settings: PreviewLabSettings,
    picker: PreviewColorPicker,
    picker_images: PreviewColorPickerImages,
    dirty: bool,
    status: String,
}

#[derive(Resource)]
pub(crate) struct PreviewPaletteRebake {
    receiver: Option<Receiver<PreviewPaletteRebakeResult>>,
    progress: Arc<AtomicUsize>,
    mode: PreviewPaletteRebakeMode,
    frames_remaining: u8,
}

pub(crate) struct PreviewPaletteRebakeResult {
    lookup: Vec<u8>,
    mode: PreviewPaletteRebakeMode,
}

#[derive(Resource)]
pub(crate) struct PreviewPaletteSave {
    receiver: Receiver<Result<PathBuf, String>>,
    progress: Arc<AtomicUsize>,
}

struct PreviewPaletteLoad {
    config: PaletteConfig,
    lookup: Option<PaletteLookup>,
    path: PathBuf,
}

#[derive(Clone, Copy)]
pub(crate) enum PreviewPaletteRebakeMode {
    Gpu,
    Cpu,
}

#[derive(Clone, Copy)]
pub(crate) enum PreviewPaletteCheckboxKind {
    AddBlack,
    AddWhite,
}

#[derive(Clone, Copy)]
enum PreviewPaletteButtonKind {
    Generate,
    Rebake,
    ApplyPicker,
    PickRaw,
    Load,
    Save,
    Lab,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewLabSlider {
    ChromaMin,
    ChromaMax,
    ChromaDivisions,
    ValueMin,
    ValueMax,
    ValueDivisions,
    HueMin,
    HueMax,
    HueDivisions,
    HueOffset,
    BiasLightness,
    BiasChroma,
    BiasHue,
    OffsetLightnessMultiply,
    OffsetLightnessAdd,
    OffsetChromaMultiply,
    OffsetChromaAdd,
    OffsetHueAdd,
    GreyChromaThreshold,
}

#[derive(Clone, Copy)]
struct PreviewColorPicker {
    lightness: f32,
    chroma: f32,
    hue: f32,
}

const PREVIEW_OKLCH_CHROMA_MAX: f32 = 0.33;

impl Default for PreviewColorPicker {
    fn default() -> Self {
        Self {
            lightness: 0.6,
            chroma: 0.12,
            hue: 120.0,
        }
    }
}

#[derive(Clone)]
struct PreviewColorPickerImages {
    lightness_chroma: Handle<Image>,
    hue_chroma: Handle<Image>,
    lightness: Handle<Image>,
    chroma: Handle<Image>,
    hue: Handle<Image>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewOklchCanvasKind {
    LightnessChroma,
    HueChroma,
    Lightness,
    Chroma,
    Hue,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewOklchMarkerAxis {
    X,
    Y,
}

#[derive(Clone, Copy)]
struct PreviewLabSettings {
    chroma_min: f32,
    chroma_max: f32,
    chroma_divisions: usize,
    value_min: f32,
    value_max: f32,
    value_divisions: usize,
    add_black: bool,
    add_white: bool,
    hue_min: f32,
    hue_max: f32,
    hue_divisions: usize,
    hue_offset: f32,
    bias_lightness: f32,
    bias_chroma: f32,
    bias_hue: f32,
    offset_lightness_multiply: f32,
    offset_lightness_add: f32,
    offset_chroma_multiply: f32,
    offset_chroma_add: f32,
    offset_hue_add: f32,
    grey_chroma_threshold: f32,
}

impl Default for PreviewLabSettings {
    fn default() -> Self {
        Self {
            chroma_min: 0.0,
            chroma_max: 1.0,
            chroma_divisions: 4,
            value_min: 0.0,
            value_max: 1.0,
            value_divisions: 16,
            add_black: false,
            add_white: false,
            hue_min: 0.0,
            hue_max: 360.0,
            hue_divisions: 20,
            hue_offset: 0.0,
            bias_lightness: 0.333,
            bias_chroma: 0.333,
            bias_hue: 0.334,
            offset_lightness_multiply: 0.0,
            offset_lightness_add: 0.0,
            offset_chroma_multiply: 0.0,
            offset_chroma_add: 0.0,
            offset_hue_add: 0.0,
            grey_chroma_threshold: 0.001,
        }
    }
}

impl From<PreviewLabSettings> for PaletteMatching {
    fn from(settings: PreviewLabSettings) -> Self {
        Self {
            lightness: settings.bias_lightness,
            chroma: settings.bias_chroma,
            hue: settings.bias_hue,
            lightness_multiply: settings.offset_lightness_multiply,
            lightness_add: settings.offset_lightness_add,
            chroma_multiply: settings.offset_chroma_multiply,
            chroma_add: settings.offset_chroma_add,
            grey_chroma_threshold: settings.grey_chroma_threshold,
            hue_add: settings.offset_hue_add,
        }
    }
}

impl PreviewLabSettings {
    fn with_matching(self, matching: PaletteMatching) -> Self {
        Self {
            bias_lightness: matching.lightness,
            bias_chroma: matching.chroma,
            bias_hue: matching.hue,
            offset_lightness_multiply: matching.lightness_multiply,
            offset_lightness_add: matching.lightness_add,
            offset_chroma_multiply: matching.chroma_multiply,
            offset_chroma_add: matching.chroma_add,
            grey_chroma_threshold: matching.grey_chroma_threshold,
            offset_hue_add: matching.hue_add,
            ..self
        }
    }
}

#[derive(Component)]
struct PreviewPixelDebugReadback {
    request_id: u64,
    source: PreviewPixelSource,
    x: u32,
    y: u32,
    width: u32,
    dither: DirectStreamDitherSettings,
}

#[derive(Component)]
struct PreviewPaletteValidationReadback {
    source: PreviewPixelSource,
    generation: u64,
    frame_number: u32,
    width: u32,
    height: u32,
    dither: DirectStreamDitherSettings,
}

#[derive(Component)]
struct PreviewPalettePickReadback {
    x: u32,
    y: u32,
    width: u32,
}

#[derive(Component)]
struct PreviewLookupTextureAudit {
    expected: Arc<[u8]>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PreviewPixelSource {
    Raw,
    Quantized,
    Audit,
    OverlayMask,
}

#[derive(Resource)]
pub(crate) struct PreviewPixelDebugState {
    pub(crate) raw_image: Handle<Image>,
    pub(crate) quantized_image: Handle<Image>,
    pub(crate) audit_image: Handle<Image>,
    pub(crate) overlay_mask_image: Handle<Image>,
    pub(crate) palette_colors: Vec<[u8; 4]>,
    pub(crate) lookup_entries: Arc<[u8]>,
    matching: PaletteMatching,
    raw_center: Vec2,
    quantized_center: Vec2,
    display_size: Vec2,
    output: String,
    validation_frames_requested: u32,
    validation_frames_checked: u32,
    validation_total_mismatches: u64,
    validation_total_mapping_mismatches: u64,
    validation_total_drifts: u64,
    pub(crate) display_generation: u64,
    validation_last_requested_generation: Option<u64>,
    validation_raw_data: HashMap<u64, Vec<u8>>,
    validation_quantized_data: HashMap<u64, Vec<u8>>,
    validation_audit_data: HashMap<u64, Vec<u8>>,
    validation_overlay_mask_data: HashMap<u64, Vec<u8>>,
    pixel_debug_raw: Option<RawPixelDebug>,
    pixel_debug_quantized: Option<QuantizedPixelDebug>,
    pixel_debug_audit: Option<QuantizedPixelDebug>,
    pixel_debug_overlay_mask: Option<bool>,
    pixel_debug_clicked_source: Option<PreviewPixelSource>,
    next_pixel_debug_request_id: u64,
    active_pixel_debug_request_id: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreviewLookupRoute {
    ExactPalette,
    DirectTable,
    AlteredTable,
}

impl PreviewLookupRoute {
    fn label(self) -> &'static str {
        match self {
            Self::ExactPalette => "exact palette bypass",
            Self::DirectTable => "direct IPSMAP table",
            Self::AlteredTable => "altered/prequantized IPSMAP table",
        }
    }
}

#[derive(Clone)]
struct RawPixelDebug {
    x: u32,
    y: u32,
    b: u8,
    g: u8,
    r: u8,
    a: u8,
    lookup_rgb: [u8; 3],
    lookup_key: usize,
    lookup_route: PreviewLookupRoute,
}

#[derive(Clone)]
struct QuantizedPixelDebug {
    palette_index: u8,
    shader_palette_index: u8,
    color: [u8; 4],
    lookup_rgb: [u8; 3],
    direct_overlay: bool,
}

pub(crate) fn setup_direct_stream_scene(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut palette_materials: ResMut<Assets<PaletteMaterial>>,
    mut preview_display_materials: ResMut<Assets<PalettePreviewDisplayMaterial>>,
    mut raw_copy_materials: ResMut<Assets<RawPreviewCopyMaterial>>,
    mut raw_display_materials: ResMut<Assets<RawPreviewDisplayMaterial>>,
    mut raw_snapshot_materials: ResMut<Assets<RawPreviewSnapshotMaterial>>,
    config: Res<AppConfig>,
    window_layout: Res<DirectStreamWindowLayout>,
) {
    let stream_image = images.add(make_stream_source_image(
        config.stream_width,
        config.stream_height,
    ));

    let stream_camera = commands
        .spawn((
            Camera2d,
            Camera {
                order: -1,
                clear_color: ClearColorConfig::Custom(Color::srgb(0.04, 0.05, 0.07)),
                ..default()
            },
            RenderTarget::Image(stream_image.clone().into()),
            RenderLayers::layer(0),
        ))
        .id();

    match config.window_mode {
        WindowMode::Preview => {
            commands.spawn((
                Camera2d,
                RenderLayers::layer(GPU_PREVIEW_DISPLAY_LAYER),
                PreviewDisplayCamera,
            ));
        }
        WindowMode::Stats => {
            commands.spawn(Camera2d);
            spawn_stats_window(&mut commands, config.custom_host, &window_layout);
        }
    }

    let mut target = DirectStreamTarget {
        camera: stream_camera,
        overlay_camera: stream_camera,
        image: stream_image.clone(),
        output_image: stream_image.clone(),
        output_is_indexed: false,
        overlay_layer: 0,
        raw_overlay_layer: None,
        width: config.stream_width,
        height: config.stream_height,
        fps: config.stream_fps,
    };

    if config.custom_host || config.window_mode == WindowMode::Preview {
        let palette_lookup = load_palette_lookup_runtime(&config.palette_lookup_path);
        let palette_config = palette_lookup.config();
        let palette_colors = palette_config.colors.clone();
        let palette_bias = crate::palette::PaletteBias::from(palette_config.matching);
        let batch_size =
            effective_custom_batch_size(config.custom_host_batch_size, config.stream_fps);
        let overlay_enabled = std::env::var_os("DIRECT_STREAM_AUDIT_DISABLE_OVERLAYS").is_none();
        let pipeline = spawn_custom_host_pipeline(
            &mut commands,
            &mut images,
            &mut meshes,
            &mut palette_materials,
            &mut raw_copy_materials,
            &mut raw_snapshot_materials,
            config.stream_width,
            config.stream_height,
            stream_image.clone(),
            &palette_colors,
            palette_bias,
            &palette_lookup,
            &mut target,
            batch_size,
            overlay_enabled,
            config.window_mode == WindowMode::Preview,
        );
        if config.window_mode == WindowMode::Preview {
            let (display_material, raw_display_material, debug_state) = spawn_preview_comparison(
                &mut commands,
                &mut images,
                &mut meshes,
                &mut preview_display_materials,
                &mut raw_display_materials,
                &mut raw_copy_materials,
                &stream_image,
                &pipeline,
                Arc::<[u8]>::from(palette_lookup.entries().to_vec()),
                palette_config.matching,
                batch_size,
                &window_layout,
                config.stream_width,
                config.stream_height,
            );
            commands.insert_resource(debug_state);
            commands.insert_resource(PreviewPaletteThrottle::new(
                config.stream_fps,
                batch_size,
                display_material,
                raw_display_material,
            ));
        }
        let pipeline_clone = pipeline.clone();
        commands.insert_resource(pipeline);
        if config.custom_host {
            let readback_entities =
                spawn_readback_entities(&mut commands, pipeline_clone.output_images.len());
            commands.insert_resource(StreamReadback {
                images: pipeline_clone.output_images.clone(),
                pixel_format: ReadbackPixelFormat::IndexedRgba8,
                readback_entities,
                next_readback_entity: 0,
                frame_interval: Duration::from_secs_f64(1.0 / config.stream_fps as f64),
                frame_accumulator: Duration::ZERO,
                frame_due: false,
                pending_requests: HashMap::default(),
                batch_size,
                batch_started_at: None,
                batch_in_progress: false,
                textures_rendered_in_batch: 0,
                frame_waiting_for_render: None,
                rendered_batch_frames: Vec::with_capacity(batch_size),
                render_settle_frames_remaining: INITIAL_RENDER_SETTLE_FRAMES,
            });
        }
    }
    commands.insert_resource(target);
}

fn spawn_preview_comparison(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    preview_display_materials: &mut Assets<PalettePreviewDisplayMaterial>,
    raw_display_materials: &mut Assets<RawPreviewDisplayMaterial>,
    _raw_copy_materials: &mut Assets<RawPreviewCopyMaterial>,
    _stream_image: &Handle<Image>,
    pipeline: &crate::gpu_palette::GpuPalettePipeline,
    lookup_entries: Arc<[u8]>,
    matching: PaletteMatching,
    _batch_size: usize,
    window_layout: &DirectStreamWindowLayout,
    width: u32,
    height: u32,
) -> (
    Handle<PalettePreviewDisplayMaterial>,
    Handle<RawPreviewDisplayMaterial>,
    PreviewPixelDebugState,
) {
    reset_preview_palette_mismatch_log(width, height);
    commands
        .spawn(PreviewLookupTextureAudit {
            expected: lookup_entries.clone(),
        })
        .observe(handle_preview_lookup_texture_audit)
        .insert(Readback::texture(pipeline.lookup_texture.clone()));
    let raw_output_images = pipeline.source_images.clone();
    let first_raw_output = raw_output_images[0].clone();

    let x_offset = width as f32 * 0.5;
    let display_scale = preview_display_scale(width, height);
    let reserved_panel_offset = window_layout.right_panel_width * 0.5;
    let preview_y = PREVIEW_EDITOR_HEIGHT as f32 * 0.5;
    let raw_center = Vec2::new(-x_offset * display_scale - reserved_panel_offset, preview_y);
    let quantized_center = Vec2::new(x_offset * display_scale - reserved_panel_offset, preview_y);
    let display_size = Vec2::new(width as f32 * display_scale, height as f32 * display_scale);
    let raw_display_material = raw_display_materials.add(RawPreviewDisplayMaterial {
        source_image: first_raw_output,
    });
    commands.spawn((
        Mesh2d(meshes.add(Rectangle::default())),
        MeshMaterial2d(raw_display_material.clone()),
        Transform::from_xyz(raw_center.x, raw_center.y, 0.0).with_scale(Vec3::new(
            display_size.x,
            display_size.y,
            1.0,
        )),
        Visibility::Hidden,
        RenderLayers::layer(GPU_PREVIEW_DISPLAY_LAYER),
        PreviewRawDisplay,
        PreviewStreamVisual,
    ));

    let display_material = preview_display_materials.add(PalettePreviewDisplayMaterial {
        params: Vec4::new(0.333, 0.333, 0.334, pipeline.palette_count.max(1) as f32),
        source_image: pipeline.output_images[0].clone(),
        palette_texture: pipeline.palette_texture.clone(),
        lookup_texture: pipeline.lookup_texture.clone(),
        lookup_params: Vec4::new(1.0, 1.0, 0.0, 0.0),
        input_offset_a: Vec4::ZERO,
        input_offset_b: Vec4::new(0.0, 0.001, 0.0, 0.0),
        dither_a: Vec4::ZERO,
        dither_b: Vec4::ZERO,
    });
    commands.spawn((
        Mesh2d(meshes.add(Rectangle::default())),
        MeshMaterial2d(display_material.clone()),
        Transform::from_xyz(quantized_center.x, quantized_center.y, 0.0).with_scale(Vec3::new(
            display_size.x,
            display_size.y,
            1.0,
        )),
        Visibility::Hidden,
        RenderLayers::layer(GPU_PREVIEW_DISPLAY_LAYER),
        PreviewStreamVisual,
        PreviewQuantizedDisplay,
    ));
    spawn_preview_loading_ui(commands);
    spawn_preview_rebaking_overlay(commands, window_layout);
    spawn_preview_pixel_debug_ui(commands);
    spawn_preview_palette_editor_ui(
        commands,
        images,
        &pipeline.palette_colors,
        matching,
        window_layout,
    );
    (
        display_material,
        raw_display_material,
        PreviewPixelDebugState {
            raw_image: raw_output_images[0].clone(),
            quantized_image: pipeline.output_images[0].clone(),
            audit_image: pipeline.audit_images[0].clone(),
            overlay_mask_image: pipeline.overlay_mask_images[0].clone(),
            palette_colors: pipeline.palette_colors.clone(),
            lookup_entries,
            matching,
            raw_center,
            quantized_center,
            display_size,
            output:
                "Pixel debug: click either preview to compare the paired raw and quantized pixels"
                    .to_owned(),
            validation_frames_requested: 0,
            validation_frames_checked: 0,
            validation_total_mismatches: 0,
            validation_total_mapping_mismatches: 0,
            validation_total_drifts: 0,
            display_generation: 0,
            validation_last_requested_generation: None,
            validation_raw_data: HashMap::new(),
            validation_quantized_data: HashMap::new(),
            validation_audit_data: HashMap::new(),
            validation_overlay_mask_data: HashMap::new(),
            pixel_debug_raw: None,
            pixel_debug_quantized: None,
            pixel_debug_audit: None,
            pixel_debug_overlay_mask: None,
            pixel_debug_clicked_source: None,
            next_pixel_debug_request_id: 0,
            active_pixel_debug_request_id: None,
        },
    )
}

fn reset_preview_palette_mismatch_log(width: u32, height: u32) {
    let header = format!(
        "PixelStream preview palette audit\nsize={width}x{height}\nframes={}\n\n",
        PREVIEW_PALETTE_VALIDATION_FRAMES
    );
    if let Err(error) = fs::write(PREVIEW_PALETTE_MISMATCH_LOG, header) {
        eprintln!("Could not reset {PREVIEW_PALETTE_MISMATCH_LOG}: {error}");
    }
}

fn handle_preview_lookup_texture_audit(
    event: On<ReadbackComplete>,
    mut commands: Commands,
    audits: Query<&PreviewLookupTextureAudit>,
) {
    let Ok(audit) = audits.get(event.entity) else {
        commands.entity(event.entity).despawn();
        return;
    };
    let mut mismatch_count = 0usize;
    let mut examples = Vec::new();
    for index in 0..audit.expected.len().max(event.data.len()) {
        let expected = audit.expected.get(index).copied();
        let actual = event.data.get(index).copied();
        if expected == actual {
            continue;
        }
        mismatch_count += 1;
        if examples.len() < 32 {
            examples.push(format!(
                "LOOKUP_TEXTURE_MISMATCH index={index} expected={expected:?} actual={actual:?}"
            ));
        }
    }
    let result = OpenOptions::new()
        .create(true)
        .append(true)
        .open(PREVIEW_PALETTE_MISMATCH_LOG)
        .map(BufWriter::new)
        .and_then(|mut log| {
            for example in &examples {
                writeln!(log, "{example}")?;
            }
            writeln!(
                log,
                "LOOKUP_TEXTURE expected_bytes={} actual_bytes={} mismatches={mismatch_count}",
                audit.expected.len(),
                event.data.len(),
            )
        });
    if let Err(error) = result {
        eprintln!("Could not write lookup texture audit: {error}");
    }
    commands.entity(event.entity).despawn();
}

pub(crate) fn update_preview_layout(
    config: Res<AppConfig>,
    window_layout: Res<DirectStreamWindowLayout>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut raw_display: Query<
        &mut Transform,
        (With<PreviewRawDisplay>, Without<PreviewQuantizedDisplay>),
    >,
    mut quantized_display: Query<&mut Transform, With<PreviewQuantizedDisplay>>,
    mut debug_state: Option<ResMut<PreviewPixelDebugState>>,
) {
    if config.window_mode != WindowMode::Preview {
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };

    let left_margin = 12.0;
    let right_margin = 12.0 + window_layout.right_panel_width;
    let preview_width = (window.width() - left_margin - right_margin).max(1.0);
    let preview_height = (window.height() - PREVIEW_EDITOR_HEIGHT as f32).max(1.0);
    let gap = 24.0;
    let scale = ((preview_width - gap).max(1.0) / (config.stream_width as f32 * 2.0))
        .min(preview_height / config.stream_height as f32)
        .max(1.0);
    let display_size = Vec2::new(
        config.stream_width as f32 * scale,
        config.stream_height as f32 * scale,
    );
    let preview_y = PREVIEW_EDITOR_HEIGHT as f32 * 0.5;
    let content_left = -window.width() * 0.5 + left_margin;
    let raw_center = Vec2::new(content_left + display_size.x * 0.5, preview_y);
    let quantized_center = Vec2::new(content_left + display_size.x * 1.5 + gap, preview_y);

    for mut transform in &mut raw_display {
        transform.translation.x = raw_center.x;
        transform.translation.y = raw_center.y;
        transform.scale = Vec3::new(display_size.x, display_size.y, 1.0);
    }
    for mut transform in &mut quantized_display {
        transform.translation.x = quantized_center.x;
        transform.translation.y = quantized_center.y;
        transform.scale = Vec3::new(display_size.x, display_size.y, 1.0);
    }
    if let Some(debug_state) = debug_state.as_deref_mut() {
        debug_state.raw_center = raw_center;
        debug_state.quantized_center = quantized_center;
        debug_state.display_size = display_size;
    }
}

fn spawn_preview_loading_ui(commands: &mut Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: px(0),
            right: px(0),
            top: px(0),
            bottom: px(PREVIEW_EDITOR_HEIGHT as f32),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        children![(
            Text::new("Loading preview..."),
            TextFont {
                font_size: FontSize::Px(24.0),
                ..default()
            },
            TextColor(Color::srgb(0.86, 0.92, 0.98)),
            PreviewLoadingText,
        )],
    ));
}

fn spawn_preview_rebaking_overlay(
    commands: &mut Commands,
    window_layout: &DirectStreamWindowLayout,
) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: px(0),
            right: px(window_layout.right_panel_width),
            top: px(0),
            bottom: px(PREVIEW_EDITOR_HEIGHT as f32),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.42)),
        Visibility::Hidden,
        PreviewRebakingOverlay,
        children![(
            Text::new("Rebaking..."),
            TextFont {
                font_size: FontSize::Px(24.0),
                ..default()
            },
            TextColor(Color::srgb(0.92, 0.96, 1.0)),
            PreviewRebakingOverlayText,
        )],
    ));
}

fn spawn_preview_pixel_debug_ui(commands: &mut Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: px(10),
            bottom: px(PREVIEW_EDITOR_HEIGHT as f32 + 12.0),
            max_width: px(640),
            padding: UiRect::all(px(8)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.02, 0.025, 0.035, 0.86)),
        BorderColor::all(Color::srgb(0.22, 0.30, 0.42)),
        children![(
            Text::new(
                "Pixel debug: click either preview to compare the paired raw and quantized pixels"
            ),
            TextFont {
                font_size: FontSize::Px(11.0),
                ..default()
            },
            TextColor(Color::srgb(0.86, 0.92, 0.98)),
            PreviewPixelDebugText,
        )],
    ));
}

fn spawn_preview_palette_editor_ui(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    colors: &[[u8; 4]],
    matching: PaletteMatching,
    window_layout: &DirectStreamWindowLayout,
) {
    let picker = PreviewColorPicker::default();
    let picker_images = make_preview_oklch_picker_images(images, picker);
    commands.insert_resource(PreviewPaletteEditor {
        colors: colors.to_vec(),
        selected: 0,
        pick_raw_next_click: false,
        lab_settings: PreviewLabSettings::default().with_matching(matching),
        committed_lab_settings: PreviewLabSettings::default().with_matching(matching),
        picker,
        picker_images: picker_images.clone(),
        dirty: false,
        status: "Palette editor ready".to_owned(),
    });

    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(10),
                right: px(10.0 + window_layout.right_panel_width),
                bottom: px(10),
                padding: UiRect::all(px(8)),
                flex_direction: FlexDirection::Row,
                column_gap: px(10),
                ..default()
            },
            BackgroundColor(Color::srgba(0.02, 0.025, 0.035, 0.90)),
            BorderColor::all(Color::srgb(0.22, 0.30, 0.42)),
            PreviewPaletteEditorRoot,
        ))
        .with_children(|parent| {
            parent
                .spawn(Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: px(6),
                    ..default()
                })
                .with_children(|swatch_panel| {
                    swatch_panel
                        .spawn(Node {
                            display: Display::Grid,
                            grid_template_columns: RepeatedGridTrack::px(16, 14.0),
                            grid_template_rows: RepeatedGridTrack::px(16, 14.0),
                            column_gap: px(3),
                            row_gap: px(3),
                            ..default()
                        })
                        .with_children(|grid| {
                            for (index, color) in colors.iter().copied().enumerate().take(256) {
                                grid.spawn((
                                    Button,
                                    Node {
                                        width: px(14),
                                        height: px(14),
                                        border: UiRect::all(px(if index == 0 { 2 } else { 1 })),
                                        ..default()
                                    },
                                    BackgroundColor(rgba_color(color)),
                                    BorderColor::all(if index == 0 {
                                        Color::WHITE
                                    } else {
                                        Color::srgba(0.0, 0.0, 0.0, 0.65)
                                    }),
                                    PreviewPaletteCell(index),
                                ));
                            }
                        });
                    spawn_preview_palette_button_row(
                        swatch_panel,
                        &[
                            ("Load", PreviewPaletteButtonKind::Load),
                            ("Save", PreviewPaletteButtonKind::Save),
                            ("Lab", PreviewPaletteButtonKind::Lab),
                        ],
                    );
                });

            parent
                .spawn(Node {
                    flex_grow: 1.0,
                    min_width: px(0),
                    flex_direction: FlexDirection::Column,
                    row_gap: px(5),
                    ..default()
                })
                .with_children(|panel| {
                    panel
                        .spawn(Node {
                            width: percent(100),
                            display: Display::Grid,
                            grid_template_columns: vec![
                                GridTrack::px(158.0),
                                GridTrack::flex(1.0),
                                GridTrack::flex(1.0),
                                GridTrack::flex(1.0),
                            ],
                            column_gap: px(10),
                            ..default()
                        })
                        .with_children(|columns| {
                            columns
                                .spawn(Node {
                                    min_width: px(0),
                                    flex_direction: FlexDirection::Column,
                                    row_gap: px(4),
                                    ..default()
                                })
                                .with_children(|column| {
                                    spawn_oklch_picker_panel(column, &picker_images);
                                    spawn_preview_palette_button_row(
                                        column,
                                        &[
                                            ("Apply Pick", PreviewPaletteButtonKind::ApplyPicker),
                                            ("Pick Raw", PreviewPaletteButtonKind::PickRaw),
                                        ],
                                    );
                                });

                            for group in [
                                (
                                    "CVH Generation",
                                    &[
                                        ("C min", PreviewLabSlider::ChromaMin, 0.0, 1.0),
                                        ("C max", PreviewLabSlider::ChromaMax, 0.0, 1.0),
                                        (
                                            "C divisions",
                                            PreviewLabSlider::ChromaDivisions,
                                            1.0,
                                            16.0,
                                        ),
                                        ("V min", PreviewLabSlider::ValueMin, 0.0, 1.0),
                                        ("V max", PreviewLabSlider::ValueMax, 0.0, 1.0),
                                        (
                                            "V divisions",
                                            PreviewLabSlider::ValueDivisions,
                                            2.0,
                                            32.0,
                                        ),
                                        ("H min", PreviewLabSlider::HueMin, 0.0, 360.0),
                                        ("H max", PreviewLabSlider::HueMax, 0.0, 360.0),
                                        ("H divisions", PreviewLabSlider::HueDivisions, 1.0, 48.0),
                                        ("H offset", PreviewLabSlider::HueOffset, -180.0, 180.0),
                                    ][..],
                                ),
                                (
                                    "Pre-quantization",
                                    &[
                                        (
                                            "value mult",
                                            PreviewLabSlider::OffsetLightnessMultiply,
                                            -1.0,
                                            1.0,
                                        ),
                                        (
                                            "value add",
                                            PreviewLabSlider::OffsetLightnessAdd,
                                            -1.0,
                                            1.0,
                                        ),
                                        (
                                            "chroma mult",
                                            PreviewLabSlider::OffsetChromaMultiply,
                                            -1.0,
                                            1.0,
                                        ),
                                        (
                                            "chroma add",
                                            PreviewLabSlider::OffsetChromaAdd,
                                            -1.0,
                                            1.0,
                                        ),
                                        ("hue add", PreviewLabSlider::OffsetHueAdd, -1.0, 1.0),
                                        (
                                            "grey thresh",
                                            PreviewLabSlider::GreyChromaThreshold,
                                            0.0,
                                            1.0,
                                        ),
                                    ][..],
                                ),
                                (
                                    "Priorities",
                                    &[
                                        ("Oklab L", PreviewLabSlider::BiasLightness, 0.0, 1.0),
                                        ("Oklab a", PreviewLabSlider::BiasChroma, 0.0, 1.0),
                                        ("Oklab b", PreviewLabSlider::BiasHue, 0.0, 1.0),
                                    ][..],
                                ),
                            ] {
                                columns
                                    .spawn(Node {
                                        min_width: px(0),
                                        flex_direction: FlexDirection::Column,
                                        row_gap: px(4),
                                        ..default()
                                    })
                                    .with_children(|column| {
                                        column.spawn((
                                            Text::new(group.0),
                                            TextFont {
                                                font_size: FontSize::Px(10.0),
                                                ..default()
                                            },
                                            TextColor(Color::srgb(0.92, 0.96, 1.0)),
                                        ));
                                        for (label, slider, min, max) in group.1 {
                                            column.spawn(preview_palette_slider(
                                                label,
                                                *slider,
                                                preview_slider_value(
                                                    PreviewLabSettings::default(),
                                                    *slider,
                                                ),
                                                *min,
                                                *max,
                                            ));
                                        }
                                        match group.0 {
                                            "CVH Generation" => {
                                                column
                                                    .spawn(Node {
                                                        flex_direction: FlexDirection::Row,
                                                        flex_wrap: FlexWrap::Wrap,
                                                        column_gap: px(8),
                                                        row_gap: px(4),
                                                        ..default()
                                                    })
                                                    .with_children(|row| {
                                                        for (label, kind, checked) in [
                                                            (
                                                                "Add Black",
                                                                PreviewPaletteCheckboxKind::AddBlack,
                                                                false,
                                                            ),
                                                            (
                                                                "Add White",
                                                                PreviewPaletteCheckboxKind::AddWhite,
                                                                false,
                                                            ),
                                                        ] {
                                                            row.spawn(preview_palette_checkbox(
                                                                label, kind, checked,
                                                            ));
                                                        }
                                                    });
                                                spawn_preview_palette_button_row(
                                                    column,
                                                    &[("Generate", PreviewPaletteButtonKind::Generate)],
                                                );
                                            }
                                            "Pre-quantization" => {
                                                spawn_preview_palette_button_row(
                                                    column,
                                                    &[("Rebake", PreviewPaletteButtonKind::Rebake)],
                                                );
                                            }
                                            _ => {}
                                        }
                                    });
                            }
                        });

                    panel.spawn((
                        Text::new("Palette editor ready"),
                        TextFont {
                            font_size: FontSize::Px(11.0),
                            ..default()
                        },
                        TextColor(Color::srgb(0.78, 0.86, 0.94)),
                        PreviewPaletteStatusText,
                    ));
                });
        });
}

fn preview_palette_button(label: &'static str) -> impl Bundle {
    (
        Button,
        Node {
            min_width: px(38),
            height: px(22),
            padding: UiRect::horizontal(px(7)),
            border: UiRect::all(px(1)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::srgb(0.07, 0.10, 0.16)),
        BorderColor::all(Color::srgb(0.24, 0.31, 0.42)),
        children![(
            Text::new(label),
            TextFont {
                font_size: FontSize::Px(10.0),
                ..default()
            },
            TextColor(Color::srgb(0.90, 0.95, 1.0)),
        )],
    )
}

fn spawn_preview_palette_button_row(
    parent: &mut ChildSpawnerCommands,
    buttons: &[(&'static str, PreviewPaletteButtonKind)],
) {
    parent
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Wrap,
            column_gap: px(5),
            row_gap: px(5),
            align_items: AlignItems::Center,
            ..default()
        })
        .with_children(|row| {
            for (label, kind) in buttons {
                let mut entity = row.spawn(preview_palette_button(label));
                match kind {
                    PreviewPaletteButtonKind::Generate => {
                        entity.insert(PreviewPaletteGenerateButton);
                    }
                    PreviewPaletteButtonKind::Rebake => {
                        entity.insert(PreviewPaletteRebakeButton);
                    }
                    PreviewPaletteButtonKind::ApplyPicker => {
                        entity.insert(PreviewPaletteApplyPickerButton);
                    }
                    PreviewPaletteButtonKind::PickRaw => {
                        entity.insert(PreviewPalettePickButton);
                    }
                    PreviewPaletteButtonKind::Load => {
                        entity.insert(PreviewPaletteLoadButton);
                    }
                    PreviewPaletteButtonKind::Save => {
                        entity.insert(PreviewPaletteSaveButton);
                    }
                    PreviewPaletteButtonKind::Lab => {
                        entity.insert(PreviewPaletteLabButton);
                    }
                };
            }
        });
}

fn preview_palette_checkbox(
    label: &'static str,
    kind: PreviewPaletteCheckboxKind,
    checked: bool,
) -> impl Bundle {
    (
        Checkbox,
        Node {
            height: px(20),
            flex_direction: FlexDirection::Row,
            column_gap: px(4),
            align_items: AlignItems::Center,
            ..default()
        },
        PreviewPaletteCheckbox(kind),
        children![
            (
                Node {
                    width: px(10),
                    height: px(10),
                    border: UiRect::all(px(1)),
                    ..default()
                },
                BackgroundColor(if checked {
                    Color::srgb(0.68, 0.78, 0.92)
                } else {
                    Color::srgb(0.04, 0.06, 0.09)
                }),
                BorderColor::all(Color::srgb(0.34, 0.44, 0.60)),
                Pickable::IGNORE,
                PreviewPaletteCheckboxMark(kind),
            ),
            (
                Text::new(label),
                TextFont {
                    font_size: FontSize::Px(10.0),
                    ..default()
                },
                TextColor(Color::srgb(0.90, 0.95, 1.0)),
                Pickable::IGNORE,
            ),
        ],
    )
}

fn spawn_oklch_picker_panel(
    parent: &mut ChildSpawnerCommands,
    picker_images: &PreviewColorPickerImages,
) {
    parent.spawn((
        Text::new("OKLCH Picker"),
        TextFont {
            font_size: FontSize::Px(10.0),
            ..default()
        },
        TextColor(Color::srgb(0.92, 0.96, 1.0)),
    ));
    spawn_oklch_picker_canvas(
        parent,
        PreviewOklchCanvasKind::LightnessChroma,
        picker_images.lightness_chroma.clone(),
        150.0,
        64.0,
    );
    spawn_oklch_picker_canvas(
        parent,
        PreviewOklchCanvasKind::HueChroma,
        picker_images.hue_chroma.clone(),
        150.0,
        42.0,
    );
    spawn_oklch_picker_strip(
        parent,
        "Lr",
        PreviewOklchCanvasKind::Lightness,
        picker_images.lightness.clone(),
    );
    spawn_oklch_picker_strip(
        parent,
        "C",
        PreviewOklchCanvasKind::Chroma,
        picker_images.chroma.clone(),
    );
    spawn_oklch_picker_strip(
        parent,
        "H",
        PreviewOklchCanvasKind::Hue,
        picker_images.hue.clone(),
    );
}

fn spawn_oklch_picker_canvas(
    parent: &mut ChildSpawnerCommands,
    kind: PreviewOklchCanvasKind,
    image: Handle<Image>,
    width: f32,
    height: f32,
) {
    parent
        .spawn((
            Button,
            Node {
                width: px(width),
                height: px(height),
                border: UiRect::all(px(1)),
                overflow: Overflow::clip(),
                position_type: PositionType::Relative,
                ..default()
            },
            ImageNode::new(image),
            BackgroundColor(Color::srgb(0.18, 0.18, 0.18)),
            BorderColor::all(Color::srgb(0.32, 0.38, 0.48)),
            RelativeCursorPosition::default(),
            PreviewPaletteOklchCanvas(kind),
        ))
        .with_children(|canvas| {
            spawn_oklch_picker_marker(canvas, kind, PreviewOklchMarkerAxis::X);
            if matches!(
                kind,
                PreviewOklchCanvasKind::LightnessChroma | PreviewOklchCanvasKind::HueChroma
            ) {
                spawn_oklch_picker_marker(canvas, kind, PreviewOklchMarkerAxis::Y);
            }
        });
}

fn spawn_oklch_picker_strip(
    parent: &mut ChildSpawnerCommands,
    label: &'static str,
    kind: PreviewOklchCanvasKind,
    image: Handle<Image>,
) {
    parent
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            column_gap: px(4),
            align_items: AlignItems::Center,
            ..default()
        })
        .with_children(|row| {
            row.spawn((
                Button,
                Node {
                    width: px(132),
                    height: px(12),
                    border: UiRect::all(px(1)),
                    overflow: Overflow::clip(),
                    position_type: PositionType::Relative,
                    ..default()
                },
                ImageNode::new(image),
                BackgroundColor(Color::srgb(0.18, 0.18, 0.18)),
                BorderColor::all(Color::srgb(0.32, 0.38, 0.48)),
                RelativeCursorPosition::default(),
                PreviewPaletteOklchCanvas(kind),
                children![(
                    Node {
                        position_type: PositionType::Absolute,
                        left: percent(0),
                        top: px(-3),
                        width: px(5),
                        height: px(18),
                        border: UiRect::all(px(1)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.02, 0.025, 0.035, 0.60)),
                    BorderColor::all(Color::WHITE),
                    Pickable::IGNORE,
                    PreviewPaletteOklchMarker {
                        kind,
                        axis: PreviewOklchMarkerAxis::X,
                    },
                )],
            ));
            row.spawn((
                Text::new(label),
                TextFont {
                    font_size: FontSize::Px(9.0),
                    ..default()
                },
                TextColor(Color::srgb(0.86, 0.91, 0.98)),
            ));
        });
}

fn spawn_oklch_picker_marker(
    parent: &mut ChildSpawnerCommands,
    kind: PreviewOklchCanvasKind,
    axis: PreviewOklchMarkerAxis,
) {
    let vertical = axis == PreviewOklchMarkerAxis::X;
    parent.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: percent(0),
            top: percent(0),
            width: if vertical { px(1) } else { percent(100) },
            height: if vertical { percent(100) } else { px(1) },
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.82)),
        Pickable::IGNORE,
        PreviewPaletteOklchMarker { kind, axis },
    ));
}

fn preview_palette_slider(
    label: &'static str,
    slider: PreviewLabSlider,
    value: f32,
    min: f32,
    max: f32,
) -> impl Bundle {
    (
        Node {
            width: percent(100),
            height: px(18),
            display: Display::Grid,
            grid_template_columns: vec![
                GridTrack::px(72.0),
                GridTrack::flex(1.0),
                GridTrack::px(38.0),
            ],
            column_gap: px(4),
            align_items: AlignItems::Center,
            ..default()
        },
        children![
            (
                Text::new(label),
                TextFont {
                    font_size: FontSize::Px(9.0),
                    ..default()
                },
                TextColor(Color::srgb(0.78, 0.86, 0.94)),
            ),
            (
                Node {
                    height: px(14),
                    position_type: PositionType::Relative,
                    align_items: AlignItems::Center,
                    ..default()
                },
                Hovered::default(),
                Slider {
                    track_click: TrackClick::Snap,
                    ..default()
                },
                SliderValue(value),
                SliderRange::new(min, max),
                PreviewPaletteSlider(slider),
                children![
                    (
                        Node {
                            width: percent(100),
                            height: px(4),
                            border_radius: BorderRadius::all(px(2)),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.18, 0.24, 0.34)),
                    ),
                    (
                        Node {
                            position_type: PositionType::Absolute,
                            left: px(0),
                            right: px(10),
                            top: px(0),
                            bottom: px(0),
                            ..default()
                        },
                        children![(
                            SliderThumb,
                            PreviewPaletteSliderThumb,
                            Node {
                                width: px(10),
                                height: px(14),
                                position_type: PositionType::Absolute,
                                left: percent(0),
                                border_radius: BorderRadius::all(px(2)),
                                ..default()
                            },
                            BackgroundColor(Color::srgb(0.68, 0.78, 0.92)),
                        )],
                    ),
                ],
            ),
            (
                Text::new(format!("{value:.3}")),
                TextFont {
                    font_size: FontSize::Px(9.0),
                    ..default()
                },
                TextColor(Color::srgb(0.90, 0.95, 1.0)),
                PreviewPaletteSliderValueText(slider),
            ),
        ],
    )
}

fn spawn_readback_entities(commands: &mut Commands, count: usize) -> Vec<Entity> {
    (0..count)
        .map(|_| {
            commands
                .spawn_empty()
                .observe(crate::capture::queue_readback_frame)
                .id()
        })
        .collect()
}

pub(crate) fn handle_preview_pixel_debug_clicks(
    config: Res<AppConfig>,
    dither: Res<DirectStreamDitherSettings>,
    buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    camera_query: Query<(&Camera, &GlobalTransform), With<PreviewDisplayCamera>>,
    debug_state: Option<ResMut<PreviewPixelDebugState>>,
    palette_editor: Option<ResMut<PreviewPaletteEditor>>,
    mut commands: Commands,
) {
    if config.window_mode != WindowMode::Preview || !buttons.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(mut debug_state) = debug_state else {
        return;
    };
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor_position) = window.cursor_position() else {
        return;
    };
    let Ok((camera, camera_transform)) = camera_query.single() else {
        return;
    };
    let Ok(world_position) = camera.viewport_to_world_2d(camera_transform, cursor_position) else {
        return;
    };

    let raw_image = debug_state.raw_image.clone();
    let quantized_image = debug_state.quantized_image.clone();
    let audit_image = debug_state.audit_image.clone();
    let overlay_mask_image = debug_state.overlay_mask_image.clone();

    if let Some(mut palette_editor) = palette_editor
        && palette_editor.pick_raw_next_click
    {
        if let Some((_source, x, y)) = preview_sample_from_rect(
            world_position,
            debug_state.raw_center,
            debug_state.display_size,
            PreviewPixelSource::Raw,
            config.stream_width,
            config.stream_height,
        ) {
            palette_editor.pick_raw_next_click = false;
            commands
                .spawn(PreviewPalettePickReadback {
                    x,
                    y,
                    width: config.stream_width,
                })
                .observe(handle_preview_palette_pick_readback)
                .insert(Readback::texture(raw_image));
        } else {
            palette_editor.status = "Pick Raw is armed: click inside the raw preview".to_owned();
        }
        return;
    }

    let Some((source, x, y)) = preview_sample_from_world(
        world_position,
        &debug_state,
        config.stream_width,
        config.stream_height,
    ) else {
        return;
    };
    debug_state.pixel_debug_clicked_source = Some(source);
    debug_state.pixel_debug_raw = None;
    debug_state.pixel_debug_quantized = None;
    debug_state.pixel_debug_audit = None;
    debug_state.pixel_debug_overlay_mask = None;
    let request_id = debug_state.next_pixel_debug_request_id;
    debug_state.next_pixel_debug_request_id = request_id.wrapping_add(1);
    debug_state.active_pixel_debug_request_id = Some(request_id);

    commands
        .spawn(PreviewPixelDebugReadback {
            request_id,
            source: PreviewPixelSource::Raw,
            x,
            y,
            width: config.stream_width,
            dither: *dither,
        })
        .observe(handle_preview_pixel_debug_readback)
        .insert(Readback::texture(raw_image));
    commands
        .spawn(PreviewPixelDebugReadback {
            request_id,
            source: PreviewPixelSource::Quantized,
            x,
            y,
            width: config.stream_width,
            dither: *dither,
        })
        .observe(handle_preview_pixel_debug_readback)
        .insert(Readback::texture(quantized_image));
    commands
        .spawn(PreviewPixelDebugReadback {
            request_id,
            source: PreviewPixelSource::Audit,
            x,
            y,
            width: config.stream_width,
            dither: *dither,
        })
        .observe(handle_preview_pixel_debug_readback)
        .insert(Readback::texture(audit_image));
    commands
        .spawn(PreviewPixelDebugReadback {
            request_id,
            source: PreviewPixelSource::OverlayMask,
            x,
            y,
            width: config.stream_width,
            dither: *dither,
        })
        .observe(handle_preview_pixel_debug_readback)
        .insert(Readback::texture(overlay_mask_image));
}

fn handle_preview_palette_pick_readback(
    event: On<ReadbackComplete>,
    mut commands: Commands,
    readbacks: Query<&PreviewPalettePickReadback>,
    mut editor: Option<ResMut<PreviewPaletteEditor>>,
) {
    let Ok(readback) = readbacks.get(event.entity) else {
        commands.entity(event.entity).despawn();
        return;
    };
    let Some(editor) = editor.as_deref_mut() else {
        commands.entity(event.entity).despawn();
        return;
    };

    let row_bytes = readback.width as usize * 4;
    let aligned_row_bytes =
        bevy::render::renderer::RenderDevice::align_copy_bytes_per_row(row_bytes);
    let offset = readback.y as usize * aligned_row_bytes + readback.x as usize * 4;
    if offset + 3 >= event.data.len() {
        editor.status = format!("Raw pick out of range at {}, {}", readback.x, readback.y);
        commands.entity(event.entity).despawn();
        return;
    }

    let color = [
        event.data[offset + 2],
        event.data[offset + 1],
        event.data[offset],
        event.data[offset + 3],
    ];
    set_preview_palette_color(
        editor,
        color,
        format!(
            "Set cell {} from raw #{:02X}{:02X}{:02X}; rebake needed",
            editor.selected, color[0], color[1], color[2]
        ),
    );
    commands.entity(event.entity).despawn();
}

pub(crate) fn handle_preview_palette_editor_interactions(
    config: Res<AppConfig>,
    mut commands: Commands,
    mut editor: Option<ResMut<PreviewPaletteEditor>>,
    saving: Option<Res<PreviewPaletteSave>>,
    mut pipeline: Option<ResMut<crate::gpu_palette::GpuPalettePipeline>>,
    mut debug_state: Option<ResMut<PreviewPixelDebugState>>,
    mut images: ResMut<Assets<Image>>,
    mut cells: Query<
        (
            &Interaction,
            &PreviewPaletteCell,
            &mut BackgroundColor,
            &mut BorderColor,
            &mut Node,
        ),
        (Changed<Interaction>, With<Button>),
    >,
    mut generate_buttons: Query<
        &Interaction,
        (Changed<Interaction>, With<PreviewPaletteGenerateButton>),
    >,
    mut rebake_buttons: Query<
        &Interaction,
        (Changed<Interaction>, With<PreviewPaletteRebakeButton>),
    >,
    mut apply_picker_buttons: Query<
        &Interaction,
        (Changed<Interaction>, With<PreviewPaletteApplyPickerButton>),
    >,
    slider_changes: Query<(&PreviewPaletteSlider, &SliderValue), Changed<SliderValue>>,
    mut pick_buttons: Query<&Interaction, (Changed<Interaction>, With<PreviewPalettePickButton>)>,
    mut load_buttons: Query<&Interaction, (Changed<Interaction>, With<PreviewPaletteLoadButton>)>,
    mut save_buttons: Query<&Interaction, (Changed<Interaction>, With<PreviewPaletteSaveButton>)>,
    mut lab_buttons: Query<&Interaction, (Changed<Interaction>, With<PreviewPaletteLabButton>)>,
) {
    if config.window_mode != WindowMode::Preview {
        return;
    }
    let Some(editor) = editor.as_deref_mut() else {
        return;
    };

    for (interaction, cell, mut background, mut border, mut node) in &mut cells {
        if *interaction == Interaction::Pressed {
            editor.selected = cell.0.min(editor.colors.len().saturating_sub(1));
            editor.status = format!("Selected cell {}", editor.selected);
        }
        let selected = cell.0 == editor.selected;
        if let Some(color) = editor.colors.get(cell.0).copied() {
            *background = BackgroundColor(rgba_color(color));
        }
        *border = BorderColor::all(if selected {
            Color::WHITE
        } else if *interaction == Interaction::Hovered {
            Color::srgb(0.62, 0.76, 0.96)
        } else {
            Color::srgba(0.0, 0.0, 0.0, 0.65)
        });
        node.border = UiRect::all(px(if selected { 2 } else { 1 }));
    }

    for interaction in &mut generate_buttons {
        if *interaction == Interaction::Pressed {
            match generate_preview_lab_palette(editor.lab_settings) {
                Ok(colors) => {
                    let count = colors.len();
                    set_preview_palette_colors(
                        editor,
                        padded_preview_palette(colors),
                        format!("Generated palette ({count} colors); rebake needed"),
                    );
                }
                Err(err) => editor.status = format!("Generate failed: {err}"),
            }
        }
    }

    for interaction in &mut rebake_buttons {
        if *interaction == Interaction::Pressed {
            commit_preview_palette_to_pipeline(
                &mut commands,
                editor,
                pipeline.as_deref_mut(),
                debug_state.as_deref_mut(),
                &mut images,
            );
        }
    }

    for interaction in &mut apply_picker_buttons {
        if *interaction == Interaction::Pressed {
            if let Some(color) = checked_oklch_to_srgb(
                editor.picker.lightness,
                editor.picker.chroma,
                editor.picker.hue,
            ) {
                set_preview_palette_color(
                    editor,
                    color,
                    format!(
                        "Set cell {} from picker #{:02X}{:02X}{:02X}; rebake needed",
                        editor.selected, color[0], color[1], color[2]
                    ),
                );
            } else {
                editor.status = "Picker color is outside sRGB gamut".to_owned();
            }
        }
    }

    let changed_sliders = slider_changes
        .iter()
        .map(|(slider, value)| (slider.0, value.0))
        .collect::<Vec<_>>();
    for (slider, value) in changed_sliders {
        set_preview_slider_value(&mut editor.lab_settings, slider, value);
        if is_preview_priority_slider(slider) {
            normalize_preview_priorities(&mut editor.lab_settings, slider);
        }
        match slider {
            PreviewLabSlider::BiasLightness
            | PreviewLabSlider::BiasChroma
            | PreviewLabSlider::BiasHue
            | PreviewLabSlider::OffsetLightnessMultiply
            | PreviewLabSlider::OffsetLightnessAdd
            | PreviewLabSlider::OffsetChromaMultiply
            | PreviewLabSlider::OffsetChromaAdd
            | PreviewLabSlider::OffsetHueAdd
            | PreviewLabSlider::GreyChromaThreshold => {
                editor.dirty = true;
                editor.status = format!(
                    "{}; rebake needed",
                    preview_lab_settings_text(editor.lab_settings)
                );
            }
            _ => {
                editor.status = preview_lab_settings_text(editor.lab_settings);
            }
        }
    }

    for interaction in &mut pick_buttons {
        if *interaction == Interaction::Pressed {
            editor.pick_raw_next_click = true;
            editor.status = "Click the raw preview to sample a source color".to_owned();
        }
    }

    for interaction in &mut load_buttons {
        if *interaction == Interaction::Pressed {
            match load_preview_palette_file() {
                Ok(Some(load)) => {
                    let path = load.path.clone();
                    editor.lab_settings = editor.lab_settings.with_matching(load.config.matching);
                    if let Some(lookup) = load.lookup {
                        commit_loaded_preview_lookup_to_pipeline(
                            editor,
                            &load.config,
                            lookup,
                            pipeline.as_deref_mut(),
                            debug_state.as_deref_mut(),
                            &mut images,
                        );
                        editor.status = format!("Loaded {}", path.display());
                    } else {
                        set_preview_palette_colors(
                            editor,
                            padded_preview_palette(load.config.colors),
                            format!("Loaded {}", path.display()),
                        );
                    }
                }
                Ok(None) => editor.status = "Load cancelled".to_owned(),
                Err(err) => editor.status = format!("Load failed: {err}"),
            }
        }
    }

    for interaction in &mut save_buttons {
        if *interaction == Interaction::Pressed {
            if saving.is_some() {
                editor.status = "Palette save already in progress".to_owned();
                continue;
            }
            match queue_preview_palette_save(&mut commands, &editor.colors, editor.lab_settings) {
                Ok(Some(path)) => editor.status = format!("Saving {}... 0%", path.display()),
                Ok(None) => editor.status = "Save cancelled".to_owned(),
                Err(err) => editor.status = format!("Save failed: {err}"),
            }
        }
    }

    for interaction in &mut lab_buttons {
        if *interaction == Interaction::Pressed {
            match open_preview_palette_lab() {
                Ok(()) => editor.status = "Opened palette lab".to_owned(),
                Err(err) => editor.status = format!("Could not open lab: {err}"),
            }
        }
    }
}

pub(crate) fn handle_preview_palette_checkbox_changes(
    config: Res<AppConfig>,
    mut editor: Option<ResMut<PreviewPaletteEditor>>,
    checkbox_states: Query<(&PreviewPaletteCheckbox, Has<Checked>)>,
    mut checkbox_marks: Query<(&PreviewPaletteCheckboxMark, &mut BackgroundColor)>,
) {
    if config.window_mode != WindowMode::Preview {
        return;
    }
    let Some(editor) = editor.as_deref_mut() else {
        return;
    };

    let mut changed = false;
    for (checkbox, checked) in &checkbox_states {
        match checkbox.0 {
            PreviewPaletteCheckboxKind::AddBlack => {
                if editor.lab_settings.add_black != checked {
                    editor.lab_settings.add_black = checked;
                    changed = true;
                }
            }
            PreviewPaletteCheckboxKind::AddWhite => {
                if editor.lab_settings.add_white != checked {
                    editor.lab_settings.add_white = checked;
                    changed = true;
                }
            }
        }
    }
    if changed {
        editor.status = preview_lab_settings_text(editor.lab_settings);
    }
    for (mark, mut background) in &mut checkbox_marks {
        let checked = match mark.0 {
            PreviewPaletteCheckboxKind::AddBlack => editor.lab_settings.add_black,
            PreviewPaletteCheckboxKind::AddWhite => editor.lab_settings.add_white,
        };
        *background = BackgroundColor(if checked {
            Color::srgb(0.68, 0.78, 0.92)
        } else {
            Color::srgb(0.04, 0.06, 0.09)
        });
    }
}

pub(crate) fn handle_preview_oklch_picker_interactions(
    config: Res<AppConfig>,
    mut editor: Option<ResMut<PreviewPaletteEditor>>,
    picker_canvases: Query<
        (
            &Interaction,
            &RelativeCursorPosition,
            &PreviewPaletteOklchCanvas,
        ),
        With<Button>,
    >,
) {
    if config.window_mode != WindowMode::Preview {
        return;
    }
    let Some(editor) = editor.as_deref_mut() else {
        return;
    };

    for (interaction, cursor, canvas) in &picker_canvases {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Some(position) = cursor.normalized else {
            continue;
        };
        update_preview_picker_from_canvas(&mut editor.picker, canvas.0, position);
        match checked_oklch_to_srgb(
            editor.picker.lightness,
            editor.picker.chroma,
            editor.picker.hue,
        ) {
            Some(color) => {
                set_preview_palette_color(
                    editor,
                    color,
                    format!(
                        "Set cell {} from {}; rebake needed",
                        editor.selected,
                        preview_picker_settings_text(editor.picker)
                    ),
                );
            }
            None => {
                editor.status = "OKLCH picker cell is outside sRGB gamut".to_owned();
            }
        }
    }
}

pub(crate) fn sync_preview_palette_editor_ui(
    editor: Option<Res<PreviewPaletteEditor>>,
    mut images: ResMut<Assets<Image>>,
    mut cells: Query<
        (
            &PreviewPaletteCell,
            &mut BackgroundColor,
            &mut BorderColor,
            &mut Node,
        ),
        Without<PreviewPaletteOklchMarker>,
    >,
    mut text_query: Query<
        &mut Text,
        (
            With<PreviewPaletteStatusText>,
            Without<PreviewPaletteSliderValueText>,
        ),
    >,
    slider_ranges: Query<(Entity, &PreviewPaletteSlider, &SliderRange), With<PreviewPaletteSlider>>,
    children: Query<&Children>,
    mut slider_thumbs: Query<
        &mut Node,
        (
            With<PreviewPaletteSliderThumb>,
            Without<PreviewPaletteSlider>,
            Without<PreviewPaletteCell>,
            Without<PreviewPaletteOklchMarker>,
        ),
    >,
    mut slider_value_texts: Query<
        (&PreviewPaletteSliderValueText, &mut Text),
        Without<PreviewPaletteStatusText>,
    >,
    mut oklch_markers: Query<
        (&PreviewPaletteOklchMarker, &mut Node),
        (
            Without<PreviewPaletteCell>,
            Without<PreviewPaletteSliderThumb>,
        ),
    >,
) {
    let Some(editor) = editor else {
        return;
    };
    if !editor.is_changed() {
        return;
    }
    update_preview_oklch_picker_images(&editor, &mut images);
    for (cell, mut background, mut border, mut node) in &mut cells {
        if let Some(color) = editor.colors.get(cell.0).copied() {
            *background = BackgroundColor(rgba_color(color));
        }
        let selected = cell.0 == editor.selected;
        *border = BorderColor::all(if selected {
            Color::WHITE
        } else {
            Color::srgba(0.0, 0.0, 0.0, 0.65)
        });
        node.border = UiRect::all(px(if selected { 2 } else { 1 }));
    }
    for (slider_entity, slider, range) in &slider_ranges {
        let value = preview_slider_value(editor.lab_settings, slider.0);
        for child in children.iter_descendants(slider_entity) {
            if let Ok(mut node) = slider_thumbs.get_mut(child) {
                node.left = percent(range.thumb_position(value) * 100.0);
            }
        }
    }
    for (slider_text, mut text) in &mut slider_value_texts {
        text.0 = format!(
            "{:.3}",
            preview_slider_value(editor.lab_settings, slider_text.0)
        );
    }
    for (marker, mut node) in &mut oklch_markers {
        let position = preview_picker_marker_position(editor.picker, marker.kind, marker.axis);
        match marker.axis {
            PreviewOklchMarkerAxis::X => node.left = percent(position * 100.0),
            PreviewOklchMarkerAxis::Y => node.top = percent((1.0 - position) * 100.0),
        }
    }
    let Ok(mut text) = text_query.single_mut() else {
        return;
    };
    let color = editor
        .colors
        .get(editor.selected)
        .copied()
        .unwrap_or([0, 0, 0, 255]);
    text.0 = format!(
        "Cell {} #{:02X}{:02X}{:02X}  {}",
        editor.selected, color[0], color[1], color[2], editor.status
    );
}

fn set_preview_palette_color(editor: &mut PreviewPaletteEditor, color: [u8; 4], status: String) {
    if let Some(slot) = editor.colors.get_mut(editor.selected) {
        *slot = color;
    }
    editor.dirty = true;
    editor.status = status;
}

fn set_preview_palette_colors(
    editor: &mut PreviewPaletteEditor,
    mut colors: Vec<[u8; 4]>,
    status: String,
) {
    if colors.is_empty() {
        editor.status = "Loaded palette was empty".to_owned();
        return;
    }
    colors.truncate(editor.colors.len().max(1));
    editor.colors = colors;
    editor.selected = editor.selected.min(editor.colors.len().saturating_sub(1));
    editor.dirty = true;
    editor.status = format!("{status}; rebake needed");
}

fn commit_preview_palette_to_pipeline(
    commands: &mut Commands,
    editor: &mut PreviewPaletteEditor,
    pipeline: Option<&mut crate::gpu_palette::GpuPalettePipeline>,
    debug_state: Option<&mut PreviewPixelDebugState>,
    images: &mut Assets<Image>,
) {
    let Some(pipeline) = pipeline else {
        return;
    };
    pipeline.palette_colors.clone_from(&editor.colors);
    pipeline.palette_count = editor.colors.len();
    if let Some(mut image) = images.get_mut(&pipeline.palette_texture)
        && let Some(data) = image.data.as_mut()
    {
        data.clear();
        for color in &editor.colors {
            data.extend_from_slice(color);
        }
    }
    if let Some(debug_state) = debug_state {
        debug_state.palette_colors.clone_from(&editor.colors);
    }
    editor.committed_lab_settings = editor.lab_settings;
    editor.dirty = false;
    start_preview_palette_rebake(commands, &editor.colors, editor.lab_settings);
    editor.status = "IPSMAP6 rebake queued".to_owned();
}

fn commit_loaded_preview_lookup_to_pipeline(
    editor: &mut PreviewPaletteEditor,
    config: &PaletteConfig,
    lookup: PaletteLookup,
    pipeline: Option<&mut crate::gpu_palette::GpuPalettePipeline>,
    debug_state: Option<&mut PreviewPixelDebugState>,
    images: &mut Assets<Image>,
) {
    let colors = padded_preview_palette(config.colors.clone());
    editor.colors = colors.clone();
    editor.selected = editor.selected.min(editor.colors.len().saturating_sub(1));
    editor.committed_lab_settings = editor.lab_settings;
    editor.dirty = false;

    let lookup_entries = Arc::<[u8]>::from(lookup.entries().to_vec().into_boxed_slice());
    if let Some(pipeline) = pipeline {
        pipeline.palette_colors.clone_from(&colors);
        pipeline.palette_count = colors.len();
        pipeline.lookup_entries = lookup_entries.clone();
        if let Some(mut image) = images.get_mut(&pipeline.palette_texture) {
            *image = crate::gpu_palette::make_palette_texture(&colors);
        }
        if let Some(mut image) = images.get_mut(&pipeline.lookup_texture) {
            *image = crate::gpu_palette::make_lookup_texture(&lookup);
        }
    }
    if let Some(debug_state) = debug_state {
        debug_state.palette_colors = colors;
        debug_state.lookup_entries = lookup_entries;
        debug_state.matching = config.matching;
        debug_state.validation_frames_requested = 0;
        debug_state.validation_frames_checked = 0;
        debug_state.validation_total_mismatches = 0;
        debug_state.validation_total_mapping_mismatches = 0;
        debug_state.validation_total_drifts = 0;
        debug_state.validation_last_requested_generation = None;
        debug_state.validation_raw_data.clear();
        debug_state.validation_quantized_data.clear();
        debug_state.validation_audit_data.clear();
        debug_state.validation_overlay_mask_data.clear();
        debug_state.pixel_debug_raw = None;
        debug_state.pixel_debug_quantized = None;
        debug_state.pixel_debug_audit = None;
        debug_state.pixel_debug_overlay_mask = None;
        debug_state.pixel_debug_clicked_source = None;
    }
}

fn start_preview_palette_rebake(
    commands: &mut Commands,
    colors: &[[u8; 4]],
    lab_settings: PreviewLabSettings,
) {
    let progress = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = crossbeam_channel::bounded(1);
    let worker_progress = progress.clone();
    let config = PaletteConfig {
        colors: colors.to_vec(),
        matching: PaletteMatching::from(lab_settings),
    };
    std::thread::spawn(move || {
        let (lookup, mode) = match build_lookup_gpu_with_progress(&config, |percent| {
            worker_progress.store(percent, Ordering::Relaxed);
        }) {
            Ok(lookup) => (lookup, PreviewPaletteRebakeMode::Gpu),
            Err(error) => {
                eprintln!("{error}; falling back to CPU IPSMAP6 rebake");
                let lookup = build_lookup_with_progress(&config, |percent| {
                    worker_progress.store(percent, Ordering::Relaxed);
                });
                (lookup, PreviewPaletteRebakeMode::Cpu)
            }
        };
        worker_progress.store(100, Ordering::Relaxed);
        let _ = sender.send(PreviewPaletteRebakeResult { lookup, mode });
    });
    commands.insert_resource(PreviewPaletteRebake {
        receiver: Some(receiver),
        progress,
        mode: PreviewPaletteRebakeMode::Gpu,
        frames_remaining: 12,
    });
}

pub(crate) fn process_preview_palette_rebake(
    rebake: Option<ResMut<PreviewPaletteRebake>>,
    mut commands: Commands,
    mut editor: Option<ResMut<PreviewPaletteEditor>>,
    mut pipeline: Option<ResMut<crate::gpu_palette::GpuPalettePipeline>>,
    throttle: Option<Res<PreviewPaletteThrottle>>,
    mut debug_state: Option<ResMut<PreviewPixelDebugState>>,
    mut images: ResMut<Assets<Image>>,
    mut palette_materials: ResMut<Assets<PaletteMaterial>>,
    mut display_materials: ResMut<Assets<PalettePreviewDisplayMaterial>>,
    mut overlays: Query<&mut Visibility, With<PreviewRebakingOverlay>>,
    mut overlay_text: Query<&mut Text, With<PreviewRebakingOverlayText>>,
) {
    let Some(mut rebake) = rebake else {
        for mut visibility in &mut overlays {
            *visibility = Visibility::Hidden;
        }
        return;
    };

    for mut visibility in &mut overlays {
        *visibility = Visibility::Visible;
    }

    let progress = rebake.progress.load(Ordering::Relaxed).min(100);
    for mut text in &mut overlay_text {
        text.0 = format!("Rebaking... {progress}%");
    }

    let completed_rebake = if let Some(receiver) = &rebake.receiver {
        let Ok(result) = receiver.try_recv() else {
            return;
        };
        Some(result)
    } else {
        None
    };
    let mut completed_mode = rebake.mode;

    if let (Some(pipeline), Some(editor)) = (pipeline.as_deref_mut(), editor.as_deref()) {
        if let Some(result) = completed_rebake
            && let Some(mut image) = images.get_mut(&pipeline.lookup_texture)
        {
            completed_mode = result.mode;
            rebake.mode = result.mode;
            rebake.receiver = None;
            let lookup = result.lookup;
            let lookup_entries = Arc::<[u8]>::from(lookup.clone().into_boxed_slice());
            if image
                .data
                .as_ref()
                .is_some_and(|data| data.len() == lookup.len())
            {
                if let Some(data) = image.data.as_mut() {
                    *data = lookup;
                }
            } else {
                *image = crate::gpu_palette::make_lookup_texture_from_entries(&lookup);
            }
            pipeline.lookup_entries = lookup_entries.clone();
            if let Some(debug_state) = debug_state.as_deref_mut() {
                debug_state.lookup_entries = lookup_entries;
                debug_state.matching = PaletteMatching::from(editor.committed_lab_settings);
                debug_state.validation_frames_requested = 0;
                debug_state.validation_frames_checked = 0;
                debug_state.validation_total_mismatches = 0;
                debug_state.validation_total_mapping_mismatches = 0;
                debug_state.validation_total_drifts = 0;
                debug_state.validation_last_requested_generation = None;
                debug_state.validation_raw_data.clear();
                debug_state.validation_quantized_data.clear();
                debug_state.validation_audit_data.clear();
                debug_state.validation_overlay_mask_data.clear();
                debug_state.pixel_debug_raw = None;
                debug_state.pixel_debug_quantized = None;
                debug_state.pixel_debug_audit = None;
                debug_state.pixel_debug_overlay_mask = None;
                debug_state.pixel_debug_clicked_source = None;
            }
        }
        update_preview_palette_materials_for_gpu(
            pipeline,
            throttle.as_deref(),
            &mut palette_materials,
            &mut display_materials,
            editor.committed_lab_settings,
            rebake.mode,
        );
    }

    if let Some(editor) = editor.as_deref_mut() {
        editor.status = match completed_mode {
            PreviewPaletteRebakeMode::Gpu => "GPU IPSMAP6 rebake complete".to_owned(),
            PreviewPaletteRebakeMode::Cpu => "IPSMAP6 rebake complete".to_owned(),
        };
    }

    if rebake.receiver.is_none() && rebake.frames_remaining > 0 {
        rebake.frames_remaining -= 1;
    }
    let remove = rebake.receiver.is_none() && rebake.frames_remaining == 0;
    if remove {
        commands.queue(|world: &mut bevy::prelude::World| {
            world.remove_resource::<PreviewPaletteRebake>();
        });
    }
}

fn update_preview_palette_materials_for_gpu(
    pipeline: &crate::gpu_palette::GpuPalettePipeline,
    throttle: Option<&PreviewPaletteThrottle>,
    palette_materials: &mut Assets<PaletteMaterial>,
    display_materials: &mut Assets<PalettePreviewDisplayMaterial>,
    settings: PreviewLabSettings,
    _mode: PreviewPaletteRebakeMode,
) {
    let matching = PaletteMatching::from(settings);
    let params = Vec4::new(
        matching.lightness,
        matching.chroma,
        matching.hue,
        pipeline.palette_count.max(1) as f32,
    );
    let lookup_params = Vec4::new(1.0, 0.0, 0.0, 0.0);
    let display_lookup_params = Vec4::new(1.0, 1.0, 0.0, 0.0);
    let input_offset_a = Vec4::new(
        matching.lightness_multiply,
        matching.lightness_add,
        matching.chroma_multiply,
        matching.chroma_add,
    );
    let input_offset_b = Vec4::new(matching.hue_add, matching.grey_chroma_threshold, 0.0, 0.0);

    if let Some(mut material) = palette_materials.get_mut(&pipeline.material) {
        material.params = params;
        material.lookup_params = lookup_params;
        material.input_offset_a = input_offset_a;
        material.input_offset_b = input_offset_b;
    }
    if let Some(throttle) = throttle
        && let Some(mut material) = display_materials.get_mut(&throttle.display_material)
    {
        material.params = params;
        material.lookup_params = display_lookup_params;
        material.input_offset_a = input_offset_a;
        material.input_offset_b = input_offset_b;
    }
}

pub(crate) fn sync_preview_palette_pipeline_materials(
    config: Res<AppConfig>,
    editor: Option<Res<PreviewPaletteEditor>>,
    pipeline: Option<Res<crate::gpu_palette::GpuPalettePipeline>>,
    throttle: Option<Res<PreviewPaletteThrottle>>,
    mut palette_materials: ResMut<Assets<PaletteMaterial>>,
    mut display_materials: ResMut<Assets<PalettePreviewDisplayMaterial>>,
) {
    if config.window_mode != WindowMode::Preview {
        return;
    }
    let (Some(editor), Some(pipeline)) = (editor.as_deref(), pipeline.as_deref()) else {
        return;
    };
    update_preview_palette_materials_for_gpu(
        pipeline,
        throttle.as_deref(),
        &mut palette_materials,
        &mut display_materials,
        editor.committed_lab_settings,
        PreviewPaletteRebakeMode::Gpu,
    );
}

fn padded_preview_palette(mut colors: Vec<[u8; 4]>) -> Vec<[u8; 4]> {
    colors.truncate(256);
    while colors.len() < 256 {
        colors.push([0, 0, 0, 255]);
    }
    colors
}

fn set_preview_slider_value(
    settings: &mut PreviewLabSettings,
    slider: PreviewLabSlider,
    value: f32,
) {
    let value = value.clamp(preview_slider_min(slider), preview_slider_max(slider));
    match slider {
        PreviewLabSlider::ChromaMin => {
            settings.chroma_min = value.clamp(0.0, settings.chroma_max - 0.001);
        }
        PreviewLabSlider::ChromaMax => {
            settings.chroma_max = value.clamp(settings.chroma_min + 0.001, 1.0);
        }
        PreviewLabSlider::ChromaDivisions => {
            settings.chroma_divisions = value.round().clamp(1.0, 16.0) as usize;
        }
        PreviewLabSlider::ValueMin => {
            settings.value_min = value.clamp(0.0, settings.value_max - 0.001);
        }
        PreviewLabSlider::ValueMax => {
            settings.value_max = value.clamp(settings.value_min + 0.001, 1.0);
        }
        PreviewLabSlider::ValueDivisions => {
            settings.value_divisions = value.round().clamp(2.0, 32.0) as usize;
        }
        PreviewLabSlider::HueMin => settings.hue_min = value,
        PreviewLabSlider::HueMax => settings.hue_max = value,
        PreviewLabSlider::HueDivisions => {
            settings.hue_divisions = value.round().clamp(1.0, 48.0) as usize;
        }
        PreviewLabSlider::HueOffset => settings.hue_offset = value,
        PreviewLabSlider::BiasLightness => settings.bias_lightness = value,
        PreviewLabSlider::BiasChroma => settings.bias_chroma = value,
        PreviewLabSlider::BiasHue => settings.bias_hue = value,
        PreviewLabSlider::OffsetLightnessMultiply => settings.offset_lightness_multiply = value,
        PreviewLabSlider::OffsetLightnessAdd => settings.offset_lightness_add = value,
        PreviewLabSlider::OffsetChromaMultiply => settings.offset_chroma_multiply = value,
        PreviewLabSlider::OffsetChromaAdd => settings.offset_chroma_add = value,
        PreviewLabSlider::OffsetHueAdd => settings.offset_hue_add = value,
        PreviewLabSlider::GreyChromaThreshold => settings.grey_chroma_threshold = value,
    }
}

fn is_preview_priority_slider(slider: PreviewLabSlider) -> bool {
    matches!(
        slider,
        PreviewLabSlider::BiasLightness | PreviewLabSlider::BiasChroma | PreviewLabSlider::BiasHue
    )
}

fn normalize_preview_priorities(settings: &mut PreviewLabSettings, changed: PreviewLabSlider) {
    let changed_value = preview_slider_value(*settings, changed).clamp(0.0, 1.0);
    set_preview_priority_value(settings, changed, changed_value);

    let others = match changed {
        PreviewLabSlider::BiasLightness => {
            [PreviewLabSlider::BiasChroma, PreviewLabSlider::BiasHue]
        }
        PreviewLabSlider::BiasChroma => {
            [PreviewLabSlider::BiasLightness, PreviewLabSlider::BiasHue]
        }
        PreviewLabSlider::BiasHue => [
            PreviewLabSlider::BiasLightness,
            PreviewLabSlider::BiasChroma,
        ],
        _ => return,
    };
    let remaining = (1.0 - changed_value).max(0.0);
    let other_total =
        preview_slider_value(*settings, others[0]) + preview_slider_value(*settings, others[1]);
    if other_total <= f32::EPSILON {
        let split = remaining * 0.5;
        set_preview_priority_value(settings, others[0], split);
        set_preview_priority_value(settings, others[1], split);
    } else {
        for slider in others {
            let value = preview_slider_value(*settings, slider) / other_total * remaining;
            set_preview_priority_value(settings, slider, value);
        }
    }

    let total = settings.bias_lightness + settings.bias_chroma + settings.bias_hue;
    let correction = 1.0 - total;
    let corrected = (preview_slider_value(*settings, others[1]) + correction).clamp(0.0, 1.0);
    set_preview_priority_value(settings, others[1], corrected);
}

fn set_preview_priority_value(
    settings: &mut PreviewLabSettings,
    slider: PreviewLabSlider,
    value: f32,
) {
    let value = value.clamp(0.0, 1.0);
    match slider {
        PreviewLabSlider::BiasLightness => settings.bias_lightness = value,
        PreviewLabSlider::BiasChroma => settings.bias_chroma = value,
        PreviewLabSlider::BiasHue => settings.bias_hue = value,
        _ => {}
    }
}

fn preview_slider_value(settings: PreviewLabSettings, slider: PreviewLabSlider) -> f32 {
    match slider {
        PreviewLabSlider::ChromaMin => settings.chroma_min,
        PreviewLabSlider::ChromaMax => settings.chroma_max,
        PreviewLabSlider::ChromaDivisions => settings.chroma_divisions as f32,
        PreviewLabSlider::ValueMin => settings.value_min,
        PreviewLabSlider::ValueMax => settings.value_max,
        PreviewLabSlider::ValueDivisions => settings.value_divisions as f32,
        PreviewLabSlider::HueMin => settings.hue_min,
        PreviewLabSlider::HueMax => settings.hue_max,
        PreviewLabSlider::HueDivisions => settings.hue_divisions as f32,
        PreviewLabSlider::HueOffset => settings.hue_offset,
        PreviewLabSlider::BiasLightness => settings.bias_lightness,
        PreviewLabSlider::BiasChroma => settings.bias_chroma,
        PreviewLabSlider::BiasHue => settings.bias_hue,
        PreviewLabSlider::OffsetLightnessMultiply => settings.offset_lightness_multiply,
        PreviewLabSlider::OffsetLightnessAdd => settings.offset_lightness_add,
        PreviewLabSlider::OffsetChromaMultiply => settings.offset_chroma_multiply,
        PreviewLabSlider::OffsetChromaAdd => settings.offset_chroma_add,
        PreviewLabSlider::OffsetHueAdd => settings.offset_hue_add,
        PreviewLabSlider::GreyChromaThreshold => settings.grey_chroma_threshold,
    }
}

fn preview_slider_min(slider: PreviewLabSlider) -> f32 {
    match slider {
        PreviewLabSlider::ChromaMin
        | PreviewLabSlider::ChromaMax
        | PreviewLabSlider::ValueMin
        | PreviewLabSlider::ValueMax
        | PreviewLabSlider::BiasLightness
        | PreviewLabSlider::BiasChroma
        | PreviewLabSlider::BiasHue
        | PreviewLabSlider::GreyChromaThreshold => 0.0,
        PreviewLabSlider::ChromaDivisions | PreviewLabSlider::HueDivisions => 1.0,
        PreviewLabSlider::ValueDivisions => 2.0,
        PreviewLabSlider::HueMin | PreviewLabSlider::HueMax => 0.0,
        PreviewLabSlider::HueOffset => -180.0,
        _ => -1.0,
    }
}

fn preview_slider_max(slider: PreviewLabSlider) -> f32 {
    match slider {
        PreviewLabSlider::ChromaDivisions => 16.0,
        PreviewLabSlider::ValueDivisions => 32.0,
        PreviewLabSlider::HueMin | PreviewLabSlider::HueMax => 360.0,
        PreviewLabSlider::HueDivisions => 48.0,
        PreviewLabSlider::HueOffset => 180.0,
        _ => 1.0,
    }
}

fn preview_lab_settings_text(settings: PreviewLabSettings) -> String {
    format!(
        "Lab C {:.3}-{:.3}/{} V {:.3}-{:.3}/{} H {:.1}-{:.1}/{} offset {:.1} B{} W{} match L/a/b {:.3}/{:.3}/{:.3}",
        settings.chroma_min,
        settings.chroma_max,
        settings.chroma_divisions,
        settings.value_min,
        settings.value_max,
        settings.value_divisions,
        settings.hue_min,
        settings.hue_max,
        settings.hue_divisions,
        settings.hue_offset,
        if settings.add_black { "+" } else { "-" },
        if settings.add_white { "+" } else { "-" },
        settings.bias_lightness,
        settings.bias_chroma,
        settings.bias_hue,
    )
}

fn preview_picker_settings_text(picker: PreviewColorPicker) -> String {
    format!(
        "Picker OKLCH L {:.3} Lr {:.3} C {:.3} H {:.1}",
        picker.lightness,
        toe(picker.lightness),
        picker.chroma,
        picker.hue
    )
}

fn make_preview_oklch_picker_images(
    images: &mut Assets<Image>,
    picker: PreviewColorPicker,
) -> PreviewColorPickerImages {
    PreviewColorPickerImages {
        lightness_chroma: images.add(make_preview_oklch_picker_image(
            PreviewOklchCanvasKind::LightnessChroma,
            picker,
        )),
        hue_chroma: images.add(make_preview_oklch_picker_image(
            PreviewOklchCanvasKind::HueChroma,
            picker,
        )),
        lightness: images.add(make_preview_oklch_picker_image(
            PreviewOklchCanvasKind::Lightness,
            picker,
        )),
        chroma: images.add(make_preview_oklch_picker_image(
            PreviewOklchCanvasKind::Chroma,
            picker,
        )),
        hue: images.add(make_preview_oklch_picker_image(
            PreviewOklchCanvasKind::Hue,
            picker,
        )),
    }
}

fn update_preview_oklch_picker_images(editor: &PreviewPaletteEditor, images: &mut Assets<Image>) {
    for (handle, kind) in [
        (
            &editor.picker_images.lightness_chroma,
            PreviewOklchCanvasKind::LightnessChroma,
        ),
        (
            &editor.picker_images.hue_chroma,
            PreviewOklchCanvasKind::HueChroma,
        ),
        (
            &editor.picker_images.lightness,
            PreviewOklchCanvasKind::Lightness,
        ),
        (&editor.picker_images.chroma, PreviewOklchCanvasKind::Chroma),
        (&editor.picker_images.hue, PreviewOklchCanvasKind::Hue),
    ] {
        if let Some(mut image) = images.get_mut(handle) {
            image.data = Some(render_preview_oklch_picker_pixels(kind, editor.picker));
        }
    }
}

fn make_preview_oklch_picker_image(
    kind: PreviewOklchCanvasKind,
    picker: PreviewColorPicker,
) -> Image {
    let (width, height) = preview_oklch_picker_texture_size(kind);
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        render_preview_oklch_picker_pixels(kind, picker),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;
    image
}

fn preview_oklch_picker_texture_size(kind: PreviewOklchCanvasKind) -> (u32, u32) {
    match kind {
        PreviewOklchCanvasKind::LightnessChroma => (180, 76),
        PreviewOklchCanvasKind::HueChroma => (180, 52),
        PreviewOklchCanvasKind::Lightness
        | PreviewOklchCanvasKind::Chroma
        | PreviewOklchCanvasKind::Hue => (180, 12),
    }
}

fn render_preview_oklch_picker_pixels(
    kind: PreviewOklchCanvasKind,
    picker: PreviewColorPicker,
) -> Vec<u8> {
    const CHROMA_MAX: f32 = PREVIEW_OKLCH_CHROMA_MAX;
    let (width, height) = preview_oklch_picker_texture_size(kind);
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        let ny = if height <= 1 {
            0.0
        } else {
            y as f32 / (height - 1) as f32
        };
        for x in 0..width {
            let nx = if width <= 1 {
                0.0
            } else {
                x as f32 / (width - 1) as f32
            };
            let (lightness, chroma, hue) = match kind {
                PreviewOklchCanvasKind::LightnessChroma => {
                    (toe_inv(nx), (1.0 - ny) * CHROMA_MAX, picker.hue)
                }
                PreviewOklchCanvasKind::HueChroma => {
                    (picker.lightness, (1.0 - ny) * CHROMA_MAX, nx * 360.0)
                }
                PreviewOklchCanvasKind::Lightness => (toe_inv(nx), picker.chroma, picker.hue),
                PreviewOklchCanvasKind::Chroma => (picker.lightness, nx * CHROMA_MAX, picker.hue),
                PreviewOklchCanvasKind::Hue => (0.75, 0.125, nx * 360.0),
            };
            match checked_oklch_to_srgb(lightness, chroma, hue) {
                Some([r, g, b, _]) => data.extend_from_slice(&[r, g, b, 255]),
                None => data.extend_from_slice(&[0, 0, 0, 0]),
            }
        }
    }
    data
}

fn update_preview_picker_from_canvas(
    picker: &mut PreviewColorPicker,
    kind: PreviewOklchCanvasKind,
    position: Vec2,
) {
    let x = (position.x + 0.5).clamp(0.0, 1.0);
    let y = (0.5 - position.y).clamp(0.0, 1.0);
    match kind {
        PreviewOklchCanvasKind::LightnessChroma => {
            picker.lightness = toe_inv(x);
            picker.chroma = y * PREVIEW_OKLCH_CHROMA_MAX;
        }
        PreviewOklchCanvasKind::HueChroma => {
            picker.hue = x * 360.0;
            picker.chroma = y * PREVIEW_OKLCH_CHROMA_MAX;
        }
        PreviewOklchCanvasKind::Lightness => picker.lightness = toe_inv(x),
        PreviewOklchCanvasKind::Chroma => picker.chroma = x * PREVIEW_OKLCH_CHROMA_MAX,
        PreviewOklchCanvasKind::Hue => picker.hue = x * 360.0,
    }
}

fn preview_picker_marker_position(
    picker: PreviewColorPicker,
    kind: PreviewOklchCanvasKind,
    axis: PreviewOklchMarkerAxis,
) -> f32 {
    match (kind, axis) {
        (PreviewOklchCanvasKind::LightnessChroma, PreviewOklchMarkerAxis::X)
        | (PreviewOklchCanvasKind::Lightness, PreviewOklchMarkerAxis::X) => toe(picker.lightness),
        (PreviewOklchCanvasKind::LightnessChroma, PreviewOklchMarkerAxis::Y)
        | (PreviewOklchCanvasKind::HueChroma, PreviewOklchMarkerAxis::Y)
        | (PreviewOklchCanvasKind::Chroma, PreviewOklchMarkerAxis::X) => {
            (picker.chroma / PREVIEW_OKLCH_CHROMA_MAX).clamp(0.0, 1.0)
        }
        (PreviewOklchCanvasKind::HueChroma, PreviewOklchMarkerAxis::X)
        | (PreviewOklchCanvasKind::Hue, PreviewOklchMarkerAxis::X) => {
            (picker.hue / 360.0).rem_euclid(1.0)
        }
        (_, PreviewOklchMarkerAxis::Y) => 0.0,
    }
}

fn generate_preview_lab_palette(settings: PreviewLabSettings) -> Result<Vec<[u8; 4]>, String> {
    const OKLCH_MAX_CHROMA: f32 = 0.2576833;
    const HUE_OFFSET_ZERO_SHIFT: f32 = -11.0;

    if settings.chroma_max > 1.0 || settings.chroma_max <= settings.chroma_min {
        return Err("chroma max must be <= 1 and greater than chroma min".to_owned());
    }
    if settings.value_min < 0.0
        || settings.value_max > 1.0
        || settings.value_max <= settings.value_min
    {
        return Err("value min/max must be a valid 0..1 range".to_owned());
    }

    let chroma_samples = lab_range(
        settings.chroma_min,
        settings.chroma_max,
        settings.chroma_divisions,
    )
    .into_iter()
    .filter(|value| *value > 0.0)
    .collect::<Vec<_>>();
    let chromas = chroma_samples
        .iter()
        .map(|value| value.clamp(0.0, 1.0) * OKLCH_MAX_CHROMA)
        .collect::<Vec<_>>();
    let values = lab_range(
        settings.value_min,
        settings.value_max,
        settings.value_divisions,
    );
    let mut grey_values = Vec::new();
    if settings.add_black {
        grey_values.push(0.0);
    }
    grey_values.extend(values.iter().copied());
    if settings.add_white {
        grey_values.push(1.0);
    }
    grey_values.sort_by(|a, b| a.total_cmp(b));
    grey_values.dedup_by(|a, b| (*a - *b).abs() <= 0.000_001);

    let mut colors = grey_values
        .iter()
        .filter_map(|value| lab_grey_color(*value))
        .collect::<Vec<_>>();
    let hue_span = settings.hue_max - settings.hue_min;
    let shifted_hue_offset = settings.hue_offset + HUE_OFFSET_ZERO_SHIFT;
    let oklch_start = srgb_hue_to_oklch_hue(shifted_hue_offset + settings.hue_min);
    let hues = (0..settings.hue_divisions)
        .map(|i| oklch_start + hue_span * i as f32 / settings.hue_divisions as f32)
        .collect::<Vec<_>>();

    for hue in hues {
        for chroma in &chromas {
            for value in &values {
                if *value <= 0.0 || *value >= 1.0 {
                    continue;
                }
                if let Some(color) = checked_oklch_to_srgb(*value, *chroma, hue) {
                    colors.push(color);
                }
            }
        }
    }

    if colors.len() > 256 {
        return Err(format!(
            "generated {} colors; reduce C/V/H divisions",
            colors.len()
        ));
    }
    Ok(colors)
}

fn lab_range(min: f32, max: f32, divisions: usize) -> Vec<f32> {
    if divisions <= 1 {
        return vec![(min + max) * 0.5];
    }
    (0..divisions)
        .map(|i| min + (max - min) * i as f32 / (divisions - 1) as f32)
        .collect()
}

fn lab_grey_color(value: f32) -> Option<[u8; 4]> {
    if value <= 0.0 {
        Some([0, 0, 0, 255])
    } else if value >= 1.0 {
        Some([255, 255, 255, 255])
    } else {
        checked_oklch_to_srgb(value, 0.0, 0.0)
    }
}

fn checked_oklch_to_srgb(lightness: f32, chroma: f32, hue_degrees: f32) -> Option<[u8; 4]> {
    let (r, g, b) = oklch_to_linear_srgb(Oklch {
        l: lightness,
        c: chroma,
        h: hue_degrees.to_radians(),
    });
    in_srgb_gamut(r, g, b).then(|| [srgb_byte(r), srgb_byte(g), srgb_byte(b), 255])
}

fn toe_inv(lr: f32) -> f32 {
    let k1 = 0.206;
    let k2 = 0.03;
    let k3 = (1.0 + k1) / (1.0 + k2);
    (lr * (lr + k1)) / (k3 * (lr + k2))
}

fn toe(lightness: f32) -> f32 {
    let k1 = 0.206;
    let k2 = 0.03;
    let k3 = (1.0 + k1) / (1.0 + k2);
    0.5 * (k3 * lightness - k1
        + ((k3 * lightness - k1) * (k3 * lightness - k1) + 4.0 * k2 * k3 * lightness).sqrt())
}

fn srgb_hue_to_oklch_hue(hue_degrees: f32) -> f32 {
    let [r, g, b, _] = hsv_to_rgb_degrees(hue_degrees, 1.0, 1.0);
    let lab = rgb_to_oklab(r, g, b);
    Oklch::from(lab).h.to_degrees()
}

fn queue_preview_palette_save(
    commands: &mut Commands,
    colors: &[[u8; 4]],
    lab_settings: PreviewLabSettings,
) -> Result<Option<PathBuf>, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("Save preview palette")
        .add_filter("IPSMAP lookup", &["ipsmap"])
        .set_file_name("preview_palette.ipsmap")
        .save_file()
    else {
        return Ok(None);
    };
    let path = path.with_extension("ipsmap");
    let config = PaletteConfig {
        colors: colors.to_vec(),
        matching: PaletteMatching::from(lab_settings),
    };
    let worker_path = path.clone();
    let progress = Arc::new(AtomicUsize::new(0));
    let worker_progress = progress.clone();
    let (sender, receiver) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let result = (|| {
            let lookup = build_lookup_with_progress(&config, |percent| {
                worker_progress.store(percent, Ordering::Relaxed);
            });
            worker_progress.store(100, Ordering::Relaxed);
            write_lookup(&worker_path, &config, &lookup)?;
            Ok(worker_path)
        })();
        let _ = sender.send(result);
    });
    commands.insert_resource(PreviewPaletteSave { receiver, progress });
    Ok(Some(path))
}

pub(crate) fn process_preview_palette_save(
    saving: Option<Res<PreviewPaletteSave>>,
    mut commands: Commands,
    mut editor: Option<ResMut<PreviewPaletteEditor>>,
) {
    let Some(saving) = saving else {
        return;
    };
    let progress = saving.progress.load(Ordering::Relaxed).min(100);
    let completed = saving.receiver.try_recv().ok();
    if let Some(editor) = editor.as_deref_mut() {
        match &completed {
            Some(Ok(path)) => editor.status = format!("Saved {}", path.display()),
            Some(Err(err)) => editor.status = format!("Save failed: {err}"),
            None => editor.status = format!("Saving palette... {progress}%"),
        }
    }
    if completed.is_some() {
        commands.queue(|world: &mut World| {
            world.remove_resource::<PreviewPaletteSave>();
        });
    }
}

fn load_preview_palette_file() -> Result<Option<PreviewPaletteLoad>, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("Load preview palette")
        .add_filter("Palette files", &["txt", "toml", "ipsmap"])
        .pick_file()
    else {
        return Ok(None);
    };
    let load = if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ipsmap"))
    {
        let lookup = crate::palette_lut::load_lookup_bundle(&path)?;
        PreviewPaletteLoad {
            config: lookup.config().clone(),
            lookup: Some(lookup),
            path,
        }
    } else {
        PreviewPaletteLoad {
            config: PaletteConfig {
                colors: load_preview_palette_text_file(&path)?,
                matching: PaletteMatching::default(),
            },
            lookup: None,
            path,
        }
    };
    Ok(Some(load))
}

fn load_preview_palette_text_file(path: &PathBuf) -> Result<Vec<[u8; 4]>, String> {
    let contents = fs::read_to_string(path).map_err(|err| err.to_string())?;
    let mut colors = Vec::new();
    for line in contents.lines() {
        let Some((_, color)) = line.split_once('#') else {
            continue;
        };
        colors.push(parse_preview_palette_color(color)?);
    }
    if colors.is_empty() {
        Err("no #RRGGBB colors found".to_owned())
    } else {
        Ok(colors)
    }
}

fn parse_preview_palette_color(value: &str) -> Result<[u8; 4], String> {
    let value = value
        .trim()
        .chars()
        .take_while(|ch| ch.is_ascii_hexdigit())
        .take(8)
        .collect::<String>();
    if value.len() != 6 && value.len() != 8 {
        return Err(format!("invalid color #{value}"));
    }
    let r = u8::from_str_radix(&value[0..2], 16).map_err(|err| err.to_string())?;
    let g = u8::from_str_radix(&value[2..4], 16).map_err(|err| err.to_string())?;
    let b = u8::from_str_radix(&value[4..6], 16).map_err(|err| err.to_string())?;
    let a = if value.len() == 8 {
        u8::from_str_radix(&value[6..8], 16).map_err(|err| err.to_string())?
    } else {
        255
    };
    Ok([r, g, b, a])
}

fn open_preview_palette_lab() -> Result<(), String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("dist/ipsc_lab/palette.html");
    if path.exists() {
        open::that(path).map_err(|err| err.to_string())
    } else {
        open::that("http://127.0.0.1:8092").map_err(|err| err.to_string())
    }
}

fn rgba_color([r, g, b, a]: [u8; 4]) -> Color {
    Color::srgba_u8(r, g, b, a)
}

#[derive(Clone, Copy)]
struct Oklab {
    l: f32,
    a: f32,
    b: f32,
}

#[derive(Clone, Copy)]
struct Oklch {
    l: f32,
    c: f32,
    h: f32,
}

impl From<Oklab> for Oklch {
    fn from(color: Oklab) -> Self {
        let c = color.a.hypot(color.b);
        let h = if c <= 0.000_001 {
            0.0
        } else {
            color.b.atan2(color.a)
        };
        Self { l: color.l, c, h }
    }
}

fn oklch_to_linear_srgb(color: Oklch) -> (f32, f32, f32) {
    let a = color.h.cos() * color.c;
    let b = color.h.sin() * color.c;

    let l_ = color.l + 0.39633778 * a + 0.21580376 * b;
    let m_ = color.l - 0.105561346 * a - 0.06385417 * b;
    let s_ = color.l - 0.08948418 * a - 1.2914855 * b;

    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    (
        4.0767417 * l - 3.3077116 * m + 0.23096994 * s,
        -1.268438 * l + 2.6097574 * m - 0.34131938 * s,
        -0.0041960863 * l - 0.7034186 * m + 1.7076147 * s,
    )
}

fn in_srgb_gamut(r: f32, g: f32, b: f32) -> bool {
    const EPSILON: f32 = 0.000_001;
    let range = -EPSILON..=1.0 + EPSILON;
    r.is_finite()
        && g.is_finite()
        && b.is_finite()
        && range.contains(&r)
        && range.contains(&g)
        && range.contains(&b)
}

fn srgb_byte(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let srgb = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    stable_srgb_byte(srgb)
}

fn stable_srgb_byte(srgb: f32) -> u8 {
    let scaled = srgb.clamp(0.0, 1.0) * 255.0;
    let lower = scaled.floor();
    let fraction = scaled - lower;
    let quantized = if (fraction - 0.5).abs() <= 0.25 {
        lower
    } else {
        (scaled + 0.5).floor()
    };
    quantized.clamp(0.0, 255.0) as u8
}

fn rgb_to_oklab(r: u8, g: u8, b: u8) -> Oklab {
    let r = srgb_to_linear(r as f32 / 255.0);
    let g = srgb_to_linear(g as f32 / 255.0);
    let b = srgb_to_linear(b as f32 / 255.0);

    let l = 0.41222146 * r + 0.53633255 * g + 0.051445995 * b;
    let m = 0.2119035 * r + 0.6806995 * g + 0.10739696 * b;
    let s = 0.08830246 * r + 0.28171884 * g + 0.6299787 * b;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    Oklab {
        l: 0.21045426 * l_ + 0.7936178 * m_ - 0.004072047 * s_,
        a: 1.9779985 * l_ - 2.4285922 * m_ + 0.4505937 * s_,
        b: 0.025904037 * l_ + 0.78277177 * m_ - 0.80867577 * s_,
    }
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn hsv_to_rgb_degrees(hue_degrees: f32, s: f32, v: f32) -> [u8; 4] {
    let h = hue_degrees.rem_euclid(360.0) / 60.0;
    let i = h.floor();
    let f = h - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match i as u8 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
        255,
    ]
}

fn preview_sample_from_world(
    world_position: Vec2,
    debug_state: &PreviewPixelDebugState,
    width: u32,
    height: u32,
) -> Option<(PreviewPixelSource, u32, u32)> {
    preview_sample_from_rect(
        world_position,
        debug_state.raw_center,
        debug_state.display_size,
        PreviewPixelSource::Raw,
        width,
        height,
    )
    .or_else(|| {
        preview_sample_from_rect(
            world_position,
            debug_state.quantized_center,
            debug_state.display_size,
            PreviewPixelSource::Quantized,
            width,
            height,
        )
    })
}

fn preview_sample_from_rect(
    world_position: Vec2,
    center: Vec2,
    size: Vec2,
    source: PreviewPixelSource,
    width: u32,
    height: u32,
) -> Option<(PreviewPixelSource, u32, u32)> {
    let left = center.x - size.x * 0.5;
    let right = center.x + size.x * 0.5;
    let bottom = center.y - size.y * 0.5;
    let top = center.y + size.y * 0.5;
    if world_position.x < left
        || world_position.x >= right
        || world_position.y < bottom
        || world_position.y >= top
    {
        return None;
    }

    let u = ((world_position.x - left) / size.x).clamp(0.0, 0.999_999);
    let v = ((top - world_position.y) / size.y).clamp(0.0, 0.999_999);
    let x = (u * width as f32).floor() as u32;
    let y = (v * height as f32).floor() as u32;
    Some((source, x, y))
}

fn handle_preview_pixel_debug_readback(
    event: On<ReadbackComplete>,
    mut commands: Commands,
    readbacks: Query<&PreviewPixelDebugReadback>,
    mut debug_state: Option<ResMut<PreviewPixelDebugState>>,
) {
    let Ok(readback) = readbacks.get(event.entity) else {
        commands.entity(event.entity).despawn();
        return;
    };
    let Some(debug_state) = debug_state.as_deref_mut() else {
        commands.entity(event.entity).despawn();
        return;
    };
    if debug_state.active_pixel_debug_request_id != Some(readback.request_id) {
        commands.entity(event.entity).despawn();
        return;
    }

    match readback.source {
        PreviewPixelSource::Raw => {
            debug_state.pixel_debug_raw = Some(preview_raw_pixel_debug(
                readback,
                &event.data,
                &debug_state.lookup_entries,
                &debug_state.palette_colors,
                readback.dither,
            ));
        }
        PreviewPixelSource::Quantized => {
            debug_state.pixel_debug_quantized = Some(preview_quantized_pixel_debug(
                readback,
                &event.data,
                &debug_state.palette_colors,
            ));
        }
        PreviewPixelSource::Audit => {
            debug_state.pixel_debug_audit = Some(preview_quantized_pixel_debug(
                readback,
                &event.data,
                &debug_state.palette_colors,
            ));
        }
        PreviewPixelSource::OverlayMask => {
            debug_state.pixel_debug_overlay_mask =
                Some(preview_overlay_mask_pixel(readback, &event.data));
        }
    }
    if debug_state.pixel_debug_raw.is_some()
        && debug_state.pixel_debug_quantized.is_some()
        && debug_state.pixel_debug_audit.is_some()
        && debug_state.pixel_debug_overlay_mask.is_some()
    {
        let raw = debug_state
            .pixel_debug_raw
            .take()
            .expect("raw pixel debug checked above");
        let mut quantized = debug_state
            .pixel_debug_quantized
            .take()
            .expect("quantized pixel debug checked above");
        let audit = debug_state
            .pixel_debug_audit
            .take()
            .expect("audit pixel debug checked above");
        quantized.lookup_rgb = audit.lookup_rgb;
        quantized.shader_palette_index = audit.palette_index;
        quantized.direct_overlay = debug_state
            .pixel_debug_overlay_mask
            .take()
            .expect("overlay mask pixel debug checked above");
        let clicked_source = debug_state
            .pixel_debug_clicked_source
            .take()
            .unwrap_or(PreviewPixelSource::Raw);
        debug_state.output = preview_pixel_pair_text(
            &raw,
            &quantized,
            clicked_source,
            debug_state.matching,
            &debug_state.lookup_entries,
            &debug_state.palette_colors,
        );
        debug_state.active_pixel_debug_request_id = None;
    }
    commands.entity(event.entity).despawn();
}

pub(crate) fn request_preview_palette_validation(
    config: Res<AppConfig>,
    dither: Res<DirectStreamDitherSettings>,
    debug_state: Option<ResMut<PreviewPixelDebugState>>,
    mut commands: Commands,
) {
    if config.window_mode != WindowMode::Preview {
        return;
    }
    let Some(mut debug_state) = debug_state else {
        return;
    };
    if debug_state.validation_frames_requested >= PREVIEW_PALETTE_VALIDATION_FRAMES {
        return;
    }
    if debug_state.display_generation == 0 {
        return;
    }
    if debug_state.validation_last_requested_generation == Some(debug_state.display_generation) {
        return;
    }

    let generation = debug_state.display_generation;
    debug_state.validation_last_requested_generation = Some(generation);
    debug_state.validation_frames_requested += 1;
    let frame_number = debug_state.validation_frames_requested;
    let raw_image = debug_state.raw_image.clone();
    let quantized_image = debug_state.quantized_image.clone();
    let audit_image = debug_state.audit_image.clone();
    let overlay_mask_image = debug_state.overlay_mask_image.clone();
    commands
        .spawn(PreviewPaletteValidationReadback {
            source: PreviewPixelSource::Raw,
            generation,
            frame_number,
            width: config.stream_width,
            height: config.stream_height,
            dither: *dither,
        })
        .observe(handle_preview_palette_validation_readback)
        .insert(Readback::texture(raw_image));
    commands
        .spawn(PreviewPaletteValidationReadback {
            source: PreviewPixelSource::Quantized,
            generation,
            frame_number,
            width: config.stream_width,
            height: config.stream_height,
            dither: *dither,
        })
        .observe(handle_preview_palette_validation_readback)
        .insert(Readback::texture(quantized_image));
    commands
        .spawn(PreviewPaletteValidationReadback {
            source: PreviewPixelSource::Audit,
            generation,
            frame_number,
            width: config.stream_width,
            height: config.stream_height,
            dither: *dither,
        })
        .observe(handle_preview_palette_validation_readback)
        .insert(Readback::texture(audit_image));
    commands
        .spawn(PreviewPaletteValidationReadback {
            source: PreviewPixelSource::OverlayMask,
            generation,
            frame_number,
            width: config.stream_width,
            height: config.stream_height,
            dither: *dither,
        })
        .observe(handle_preview_palette_validation_readback)
        .insert(Readback::texture(overlay_mask_image));
}

fn handle_preview_palette_validation_readback(
    event: On<ReadbackComplete>,
    mut commands: Commands,
    readbacks: Query<&PreviewPaletteValidationReadback>,
    mut debug_state: Option<ResMut<PreviewPixelDebugState>>,
) {
    let Ok(readback) = readbacks.get(event.entity) else {
        commands.entity(event.entity).despawn();
        return;
    };
    let Some(debug_state) = debug_state.as_deref_mut() else {
        commands.entity(event.entity).despawn();
        return;
    };

    match readback.source {
        PreviewPixelSource::Raw => {
            debug_state
                .validation_raw_data
                .insert(readback.generation, event.data.clone());
        }
        PreviewPixelSource::Quantized => {
            debug_state
                .validation_quantized_data
                .insert(readback.generation, event.data.clone());
        }
        PreviewPixelSource::Audit => {
            debug_state
                .validation_audit_data
                .insert(readback.generation, event.data.clone());
        }
        PreviewPixelSource::OverlayMask => {
            debug_state
                .validation_overlay_mask_data
                .insert(readback.generation, event.data.clone());
        }
    }

    if debug_state
        .validation_raw_data
        .contains_key(&readback.generation)
        && debug_state
            .validation_quantized_data
            .contains_key(&readback.generation)
        && debug_state
            .validation_audit_data
            .contains_key(&readback.generation)
        && debug_state
            .validation_overlay_mask_data
            .contains_key(&readback.generation)
    {
        let raw = debug_state
            .validation_raw_data
            .remove(&readback.generation)
            .expect("raw validation data checked above");
        let quantized = debug_state
            .validation_quantized_data
            .remove(&readback.generation)
            .expect("quantized validation data checked above");
        let audit = debug_state
            .validation_audit_data
            .remove(&readback.generation)
            .expect("audit validation data checked above");
        let overlay_mask = debug_state
            .validation_overlay_mask_data
            .remove(&readback.generation)
            .expect("overlay mask validation data checked above");
        debug_state.validation_frames_checked =
            debug_state.validation_frames_checked.saturating_add(1);
        let frame_number = readback.frame_number;
        let log_result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(PREVIEW_PALETTE_MISMATCH_LOG)
            .map(BufWriter::new)
            .and_then(|mut log| {
                let summary = validate_preview_palette_frame(
                    &raw,
                    &quantized,
                    &audit,
                    &overlay_mask,
                    readback.width,
                    readback.height,
                    &debug_state.lookup_entries,
                    &debug_state.palette_colors,
                    readback.dither,
                    debug_state.matching,
                    frame_number,
                    readback.generation,
                    &mut log,
                )?;
                writeln!(
                    log,
                    "FRAME {frame_number} generation={} mismatches={} mapping_mismatches={} drifts={}",
                    readback.generation,
                    summary.mismatches,
                    summary.mapping_mismatches,
                    summary.drifts,
                )?;
                Ok(summary)
            });
        match log_result {
            Ok(summary) => {
                debug_state.validation_total_mismatches += summary.mismatches;
                debug_state.validation_total_mapping_mismatches += summary.mapping_mismatches;
                debug_state.validation_total_drifts += summary.drifts;
                debug_state.output = format!(
                    "Palette audit {}/{}: {} mismatches (see {})",
                    debug_state.validation_frames_checked,
                    PREVIEW_PALETTE_VALIDATION_FRAMES,
                    summary.mismatches,
                    PREVIEW_PALETTE_MISMATCH_LOG,
                );
            }
            Err(error) => {
                debug_state.output =
                    format!("Could not write {}: {error}", PREVIEW_PALETTE_MISMATCH_LOG);
                eprintln!("{}", debug_state.output);
            }
        }
        if debug_state.validation_frames_checked == PREVIEW_PALETTE_VALIDATION_FRAMES {
            let report = format!(
                "automatic preview palette validation: {} frames, {} mismatches (mapping {}), {} drifts",
                PREVIEW_PALETTE_VALIDATION_FRAMES,
                debug_state.validation_total_mismatches,
                debug_state.validation_total_mapping_mismatches,
                debug_state.validation_total_drifts,
            );
            if let Ok(mut log) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(PREVIEW_PALETTE_MISMATCH_LOG)
            {
                let _ = writeln!(log, "FINAL {report}");
            }
            eprintln!("{report}");
            debug_state.output = report;
        }
    }
    commands.entity(event.entity).despawn();
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PreviewFrameValidationSummary {
    mismatches: u64,
    mapping_mismatches: u64,
    drifts: u64,
}

fn validate_preview_palette_frame(
    raw: &[u8],
    quantized: &[u8],
    audit: &[u8],
    overlay_mask: &[u8],
    width: u32,
    height: u32,
    lookup_entries: &[u8],
    palette_colors: &[[u8; 4]],
    dither: DirectStreamDitherSettings,
    matching: PaletteMatching,
    frame_number: u32,
    generation: u64,
    log: &mut impl Write,
) -> std::io::Result<PreviewFrameValidationSummary> {
    let mut summary = PreviewFrameValidationSummary::default();

    for y in 0..height as usize {
        for x in 0..width as usize {
            let readback = PreviewPixelDebugReadback {
                request_id: 0,
                source: PreviewPixelSource::Raw,
                x: x as u32,
                y: y as u32,
                width,
                dither,
            };
            let raw_debug =
                preview_raw_pixel_debug(&readback, raw, lookup_entries, palette_colors, dither);
            let mut quantized_debug =
                preview_quantized_pixel_debug(&readback, quantized, palette_colors);
            let audit_debug = preview_quantized_pixel_debug(&readback, audit, palette_colors);
            quantized_debug.lookup_rgb = audit_debug.lookup_rgb;
            quantized_debug.shader_palette_index = audit_debug.palette_index;
            quantized_debug.direct_overlay = preview_overlay_mask_pixel(&readback, overlay_mask);
            if is_direct_output_overlay(&raw_debug, &quantized_debug, PreviewPixelSource::Raw) {
                continue;
            }
            let gpu_expected_index = expected_gpu_lookup_index(
                &raw_debug,
                &quantized_debug,
                lookup_entries,
                palette_colors,
            );
            let mapping_mismatch = gpu_expected_index != Some(quantized_debug.palette_index)
                || quantized_debug.shader_palette_index != quantized_debug.palette_index;
            let drift = raw_debug.lookup_rgb != quantized_debug.lookup_rgb;
            if !mapping_mismatch && !drift {
                continue;
            }
            summary.mismatches += u64::from(mapping_mismatch);
            summary.mapping_mismatches += u64::from(mapping_mismatch);
            summary.drifts += u64::from(drift);
            writeln!(
                log,
                "FRAME {frame_number} generation={generation} pixel=({x},{y}) mapping_mismatch={mapping_mismatch} drift={drift}\n{}\n---",
                preview_pixel_pair_text(
                    &raw_debug,
                    &quantized_debug,
                    PreviewPixelSource::Raw,
                    matching,
                    lookup_entries,
                    palette_colors,
                ),
            )?;
        }
    }

    Ok(summary)
}

fn expected_gpu_lookup_index(
    raw: &RawPixelDebug,
    quantized: &QuantizedPixelDebug,
    lookup_entries: &[u8],
    palette_colors: &[[u8; 4]],
) -> Option<u8> {
    if raw.lookup_route == PreviewLookupRoute::ExactPalette {
        return exact_palette_index(raw.r, raw.g, raw.b, palette_colors);
    }

    let mut lookup_key = rgb_key(quantized.lookup_rgb);
    if raw.lookup_route == PreviewLookupRoute::DirectTable {
        lookup_key = lookup_key.checked_add(crate::palette_lut::LUT_ENTRY_COUNT)?;
    }
    lookup_entries.get(lookup_key).copied()
}

fn expected_preview_index(
    r: u8,
    g: u8,
    b: u8,
    alpha: u8,
    x: u32,
    y: u32,
    dither: DirectStreamDitherSettings,
    lookup_entries: &[u8],
    palette_colors: &[[u8; 4]],
) -> Option<(PreviewLookupRoute, u8, [u8; 3])> {
    let direct = alpha == 254 && lookup_entries.len() >= crate::palette_lut::LUT_ENTRY_COUNT * 2;
    if !direct && let Some(index) = exact_palette_index(r, g, b, palette_colors) {
        return Some((PreviewLookupRoute::ExactPalette, index, [r, g, b]));
    }

    let lookup_rgb = if direct {
        [r, g, b]
    } else {
        apply_preview_dither([r, g, b], x, y, dither)
    };
    let lookup_key = rgb_key(lookup_rgb);
    let expected_lookup_key = if direct {
        lookup_key + crate::palette_lut::LUT_ENTRY_COUNT
    } else {
        lookup_key
    };
    lookup_entries
        .get(expected_lookup_key)
        .copied()
        .map(|index| {
            (
                if direct {
                    PreviewLookupRoute::DirectTable
                } else {
                    PreviewLookupRoute::AlteredTable
                },
                index,
                lookup_rgb,
            )
        })
}

fn rgb_key([r, g, b]: [u8; 3]) -> usize {
    (usize::from(r) << 16) | (usize::from(g) << 8) | usize::from(b)
}

fn apply_preview_dither(
    [r, g, b]: [u8; 3],
    x: u32,
    y: u32,
    dither: DirectStreamDitherSettings,
) -> [u8; 3] {
    let intensity = dither.intensity.max(0.0);
    if intensity <= 0.0 {
        return [r, g, b];
    }

    let scale = dither.scale.max(0.125);
    let cell_x = ((x as f32 + 0.5) / scale).floor() as i32;
    let cell_y = ((y as f32 + 0.5) / scale).floor() as i32;
    let value_noise = ordered_dither(cell_x, cell_y, 0, 0);
    let chroma_noise = ordered_dither(cell_x, cell_y, 3, 5);
    let hue_noise = ordered_dither(cell_x, cell_y, 6, 2);

    let mut oklch = Oklch::from(rgb_to_oklab(r, g, b));
    oklch.l = (oklch.l + value_noise * dither.value_strength * intensity).clamp(0.0, 1.0);
    oklch.c = (oklch.c + chroma_noise * dither.chroma_strength * intensity).max(0.0);
    oklch.h += hue_noise * dither.hue_strength * intensity * std::f32::consts::TAU;
    oklch.c = clamp_preview_chroma_to_srgb_gamut(oklch);

    let (r, g, b) = oklch_to_linear_srgb(oklch);
    [srgb_byte(r), srgb_byte(g), srgb_byte(b)]
}

fn ordered_dither(cell_x: i32, cell_y: i32, offset_x: i32, offset_y: i32) -> f32 {
    let x = (cell_x + offset_x) & 7;
    let y = (cell_y + offset_y) & 7;
    let mut index = 0u32;
    for bit in 0..3 {
        let x_bit = ((x >> bit) & 1) as u32;
        let y_bit = ((y >> bit) & 1) as u32;
        let pair = match (x_bit, y_bit) {
            (0, 0) => 0,
            (1, 0) => 2,
            (0, 1) => 3,
            _ => 1,
        };
        index += pair << (bit * 2);
    }
    ((index as f32 + 0.5) / 64.0) * 2.0 - 1.0
}

fn clamp_preview_chroma_to_srgb_gamut(color: Oklch) -> f32 {
    if color.c <= 0.0 {
        return 0.0;
    }
    let (r, g, b) = oklch_to_linear_srgb(color);
    if in_srgb_gamut(r, g, b) {
        return color.c;
    }

    let mut low = 0.0;
    let mut high = color.c;
    for _ in 0..16 {
        let mid = (low + high) * 0.5;
        let candidate = Oklch { c: mid, ..color };
        let (r, g, b) = oklch_to_linear_srgb(candidate);
        if in_srgb_gamut(r, g, b) {
            low = mid;
        } else {
            high = mid;
        }
    }
    low
}

fn exact_palette_index(r: u8, g: u8, b: u8, palette_colors: &[[u8; 4]]) -> Option<u8> {
    palette_colors
        .iter()
        .take(256)
        .position(|color| color[0] == r && color[1] == g && color[2] == b)
        .map(|index| index as u8)
}

fn preview_raw_pixel_debug(
    readback: &PreviewPixelDebugReadback,
    data: &[u8],
    lookup_entries: &[u8],
    palette_colors: &[[u8; 4]],
    dither: DirectStreamDitherSettings,
) -> RawPixelDebug {
    let row_bytes = readback.width as usize * 4;
    let aligned_row_bytes =
        bevy::render::renderer::RenderDevice::align_copy_bytes_per_row(row_bytes);
    let offset = readback.y as usize * aligned_row_bytes + readback.x as usize * 4;
    if offset + 3 >= data.len() {
        return RawPixelDebug::out_of_range(readback.x, readback.y);
    }
    let b = data[offset];
    let g = data[offset + 1];
    let r = data[offset + 2];
    let a = data[offset + 3];
    let expected = expected_preview_index(
        r,
        g,
        b,
        a,
        readback.x,
        readback.y,
        dither,
        lookup_entries,
        palette_colors,
    );
    let lookup_route = expected
        .map(|(route, _, _)| route)
        .unwrap_or(PreviewLookupRoute::AlteredTable);
    let lookup_rgb = expected
        .map(|(_, _, lookup_rgb)| lookup_rgb)
        .unwrap_or([r, g, b]);
    RawPixelDebug {
        x: readback.x,
        y: readback.y,
        b,
        g,
        r,
        a,
        lookup_rgb,
        lookup_key: rgb_key(lookup_rgb),
        lookup_route,
    }
}

impl RawPixelDebug {
    fn out_of_range(x: u32, y: u32) -> Self {
        Self {
            x,
            y,
            b: 0,
            g: 0,
            r: 0,
            a: 0,
            lookup_rgb: [0, 0, 0],
            lookup_key: 0,
            lookup_route: PreviewLookupRoute::AlteredTable,
        }
    }
}

fn preview_quantized_pixel_debug(
    readback: &PreviewPixelDebugReadback,
    data: &[u8],
    palette_colors: &[[u8; 4]],
) -> QuantizedPixelDebug {
    let row_bytes = readback.width as usize * 4;
    let aligned_row_bytes =
        bevy::render::renderer::RenderDevice::align_copy_bytes_per_row(row_bytes);
    let offset = readback.y as usize * aligned_row_bytes + readback.x as usize * 4;
    let palette_index = data.get(offset).copied().unwrap_or(0);
    let metadata_r = data.get(offset + 1).copied().unwrap_or(0);
    let metadata_g = data.get(offset + 2).copied().unwrap_or(0);
    let metadata_b = data.get(offset + 3).copied().unwrap_or(0);
    let color = palette_colors
        .get(palette_index as usize)
        .copied()
        .unwrap_or([0, 0, 0, 255]);
    QuantizedPixelDebug {
        palette_index,
        shader_palette_index: palette_index,
        color,
        lookup_rgb: [metadata_r, metadata_g, metadata_b],
        direct_overlay: false,
    }
}

fn preview_overlay_mask_pixel(readback: &PreviewPixelDebugReadback, data: &[u8]) -> bool {
    let row_bytes = readback.width as usize * 4;
    let aligned_row_bytes =
        bevy::render::renderer::RenderDevice::align_copy_bytes_per_row(row_bytes);
    let offset = readback.y as usize * aligned_row_bytes + readback.x as usize * 4;
    data.get(offset + 3).copied().unwrap_or(0) > 0
}

fn preview_pixel_pair_text(
    raw: &RawPixelDebug,
    quantized: &QuantizedPixelDebug,
    clicked_source: PreviewPixelSource,
    matching: PaletteMatching,
    lookup_entries: &[u8],
    palette_colors: &[[u8; 4]],
) -> String {
    let lookup_input = if raw.lookup_rgb != [raw.r, raw.g, raw.b] {
        format!(
            "\nlookup input after dither: #{:02X}{:02X}{:02X}",
            raw.lookup_rgb[0], raw.lookup_rgb[1], raw.lookup_rgb[2]
        )
    } else {
        String::new()
    };
    if is_direct_output_overlay(raw, quantized, clicked_source) {
        return format!(
            "Preview sample ({}, {})\nunderlay raw: #{:02X}{:02X}{:02X}  BGRA {}, {}, {}, {}{}\noverlay output: index {} #{:02X}{:02X}{:02X}\nexpected: direct output overlay index {} #{:02X}{:02X}{:02X}\nDelta E OK: n/a (overlay replaces underlay)\nunderlay lookup RGB key: {}",
            raw.x,
            raw.y,
            raw.r,
            raw.g,
            raw.b,
            raw.b,
            raw.g,
            raw.r,
            raw.a,
            lookup_input,
            quantized.palette_index,
            quantized.color[0],
            quantized.color[1],
            quantized.color[2],
            quantized.palette_index,
            quantized.color[0],
            quantized.color[1],
            quantized.color[2],
            raw.lookup_key,
        );
    }

    let (lookup_route, expected_index, expected_color) = preview_expected_readout(
        raw,
        quantized,
        clicked_source,
        lookup_entries,
        palette_colors,
    );
    let raw_oklab = rgb_to_oklab(raw.r, raw.g, raw.b);
    let quantized_oklab = rgb_to_oklab(quantized.color[0], quantized.color[1], quantized.color[2]);
    let delta_l = raw_oklab.l - quantized_oklab.l;
    let delta_a = raw_oklab.a - quantized_oklab.a;
    let delta_b = raw_oklab.b - quantized_oklab.b;
    let delta_e_ok = (delta_l * delta_l + delta_a * delta_a + delta_b * delta_b).sqrt();
    let raw_oklch = Oklch::from(raw_oklab);
    let quantized_oklch = Oklch::from(quantized_oklab);
    let dl = raw_oklch.l - quantized_oklch.l;
    let dc = raw_oklch.c - quantized_oklch.c;
    let dh_degrees = (raw_oklch.h.to_degrees() - quantized_oklch.h.to_degrees() + 180.0)
        .rem_euclid(360.0)
        - 180.0;
    let prequantization = if raw.lookup_route == PreviewLookupRoute::AlteredTable {
        format!(
            "\nprequantization: L x{:.3} {:+.3}, C x{:.3} {:+.3}, H {:+.3} turns",
            1.0 + matching.lightness_multiply,
            matching.lightness_add,
            1.0 + matching.chroma_multiply,
            matching.chroma_add,
            matching.hue_add,
        )
    } else {
        String::new()
    };
    let gpu_expected_index =
        expected_gpu_lookup_index(raw, quantized, lookup_entries, palette_colors);
    let mapping_status = if gpu_expected_index == Some(quantized.palette_index)
        && quantized.shader_palette_index == quantized.palette_index
    {
        "MATCH"
    } else {
        "MISMATCH"
    };
    let lookup_input_status = if raw.lookup_rgb == quantized.lookup_rgb {
        "MATCH"
    } else {
        "DRIFT"
    };
    format!(
        "Preview sample ({}, {})\nbefore raw: #{:02X}{:02X}{:02X}  BGRA {}, {}, {}, {}{}\nafter actual: index {} #{:02X}{:02X}{:02X}\nshader pre-overlay index: {}\nmapping: [{}]\nlookup input CPU/GPU: #{:02X}{:02X}{:02X}/#{:02X}{:02X}{:02X} [{}]\nexpected: {} index {} #{:02X}{:02X}{:02X}{}\nDelta E OK: {:.5}  OKLab dL/da/db: {:.4} / {:.4} / {:.4}\nOKLCH delta L/C/H: {:.4} / {:.4} / {:.1}deg\nlookup RGB key: {}",
        raw.x,
        raw.y,
        raw.r,
        raw.g,
        raw.b,
        raw.b,
        raw.g,
        raw.r,
        raw.a,
        lookup_input,
        quantized.palette_index,
        quantized.color[0],
        quantized.color[1],
        quantized.color[2],
        quantized.shader_palette_index,
        mapping_status,
        raw.lookup_rgb[0],
        raw.lookup_rgb[1],
        raw.lookup_rgb[2],
        quantized.lookup_rgb[0],
        quantized.lookup_rgb[1],
        quantized.lookup_rgb[2],
        lookup_input_status,
        lookup_route,
        expected_index,
        expected_color[0],
        expected_color[1],
        expected_color[2],
        prequantization,
        delta_e_ok,
        delta_l.abs(),
        delta_a.abs(),
        delta_b.abs(),
        dl.abs(),
        dc.abs(),
        dh_degrees.abs(),
        rgb_key(quantized.lookup_rgb),
    )
}

fn preview_expected_readout(
    raw: &RawPixelDebug,
    quantized: &QuantizedPixelDebug,
    clicked_source: PreviewPixelSource,
    lookup_entries: &[u8],
    palette_colors: &[[u8; 4]],
) -> (&'static str, String, [u8; 4]) {
    if is_direct_output_overlay(raw, quantized, clicked_source) {
        return (
            "direct output overlay",
            quantized.palette_index.to_string(),
            quantized.color,
        );
    }

    let expected_index = expected_gpu_lookup_index(raw, quantized, lookup_entries, palette_colors);
    let expected_color = expected_index
        .and_then(|index| palette_colors.get(index as usize).copied())
        .unwrap_or([0, 0, 0, 255]);
    (
        raw.lookup_route.label(),
        expected_index
            .map(|index| index.to_string())
            .unwrap_or_else(|| "out of range".to_owned()),
        expected_color,
    )
}

fn is_direct_output_overlay(
    _raw: &RawPixelDebug,
    quantized: &QuantizedPixelDebug,
    _clicked_source: PreviewPixelSource,
) -> bool {
    quantized.direct_overlay
}

#[cfg(test)]
mod preview_pixel_debug_tests {
    use super::*;

    fn raw_debug(route: PreviewLookupRoute, _expected_index: Option<u8>) -> RawPixelDebug {
        RawPixelDebug {
            x: 0,
            y: 0,
            b: 0,
            g: 0,
            r: 0,
            a: 255,
            lookup_rgb: [0, 0, 0],
            lookup_key: 0,
            lookup_route: route,
        }
    }

    fn quantized_debug(index: u8) -> QuantizedPixelDebug {
        QuantizedPixelDebug {
            palette_index: index,
            shader_palette_index: index,
            color: [4, 5, 6, 255],
            lookup_rgb: [0, 0, 0],
            direct_overlay: false,
        }
    }

    fn altered_fixtures(index: u8) -> (Vec<u8>, Vec<[u8; 4]>) {
        let lookup = vec![index];
        let mut palette = vec![[0, 0, 0, 255]; usize::from(index) + 1];
        palette[usize::from(index)] = [1, 2, 3, 255];
        (lookup, palette)
    }

    #[test]
    fn after_click_keeps_altered_label_when_after_matches_altered_expectation() {
        let raw = raw_debug(PreviewLookupRoute::AlteredTable, Some(9));
        let quantized = quantized_debug(9);
        let (lookup, palette) = altered_fixtures(9);

        let (label, index, color) = preview_expected_readout(
            &raw,
            &quantized,
            PreviewPixelSource::Quantized,
            &lookup,
            &palette,
        );

        assert_eq!(label, "altered/prequantized IPSMAP table");
        assert_eq!(index, "9");
        assert_eq!(color, [1, 2, 3, 255]);
    }

    #[test]
    fn after_click_reports_mismatch_when_unmarked_output_differs_from_expectation() {
        let raw = raw_debug(PreviewLookupRoute::AlteredTable, Some(9));
        let quantized = quantized_debug(42);
        let (lookup, palette) = altered_fixtures(9);

        let (label, index, color) = preview_expected_readout(
            &raw,
            &quantized,
            PreviewPixelSource::Quantized,
            &lookup,
            &palette,
        );

        assert_eq!(label, "altered/prequantized IPSMAP table");
        assert_eq!(index, "9");
        assert_eq!(color, [1, 2, 3, 255]);

        let text = preview_pixel_pair_text(
            &raw,
            &quantized,
            PreviewPixelSource::Quantized,
            PaletteMatching::default(),
            &lookup,
            &palette,
        );
        assert!(text.contains("mapping: [MISMATCH]"));
        assert!(!text.contains("direct output overlay"));
    }

    #[test]
    fn overlay_marker_identifies_direct_output_even_when_indices_match() {
        let raw = raw_debug(PreviewLookupRoute::AlteredTable, Some(9));
        let mut quantized = quantized_debug(9);
        quantized.direct_overlay = true;

        let (label, index, color) =
            preview_expected_readout(&raw, &quantized, PreviewPixelSource::Raw, &[], &[]);

        assert_eq!(label, "direct output overlay");
        assert_eq!(index, "9");
        assert_eq!(color, [4, 5, 6, 255]);
    }

    #[test]
    fn direct_overlay_readout_does_not_compare_overlay_to_underlay() {
        let mut raw = raw_debug(PreviewLookupRoute::AlteredTable, Some(9));
        raw.r = 0x14;
        raw.g = 0x07;
        raw.b = 0x2A;
        raw.lookup_rgb = [0x14, 0x07, 0x2A];
        raw.lookup_key = 1_312_554;
        let mut quantized = quantized_debug(169);
        quantized.color = [0x52, 0x45, 0xFC, 255];
        quantized.direct_overlay = true;

        let text = preview_pixel_pair_text(
            &raw,
            &quantized,
            PreviewPixelSource::Quantized,
            PaletteMatching::default(),
            &[],
            &[],
        );

        assert!(text.contains("underlay raw: #14072A"));
        assert!(text.contains("overlay output: index 169 #5245FC"));
        assert!(text.contains("Delta E OK: n/a (overlay replaces underlay)"));
        assert!(text.contains("underlay lookup RGB key: 1312554"));
        assert!(!text.contains("OKLab dL/da/db"));
        assert!(!text.contains("OKLCH delta"));
    }

    #[test]
    fn altered_lookup_prediction_uses_the_same_ordered_dither_input_as_the_shader() {
        let dither = DirectStreamDitherSettings {
            scale: 1.0,
            intensity: 1.0,
            value_strength: 0.2,
            chroma_strength: 0.05,
            hue_strength: 0.01,
        };
        let raw_rgb = [0x14, 0x07, 0x2A];
        let lookup_rgb = apply_preview_dither(raw_rgb, 116, 33, dither);
        assert_ne!(lookup_rgb, raw_rgb);

        let lookup_key = rgb_key(lookup_rgb);
        let mut lookup_entries = vec![0; lookup_key + 1];
        lookup_entries[lookup_key] = 42;
        let palette = [[1, 2, 3, 255]];

        let expected = expected_preview_index(
            raw_rgb[0],
            raw_rgb[1],
            raw_rgb[2],
            255,
            116,
            33,
            dither,
            &lookup_entries,
            &palette,
        );

        assert_eq!(
            expected,
            Some((PreviewLookupRoute::AlteredTable, 42, lookup_rgb))
        );
    }

    #[test]
    fn gpu_lookup_metadata_is_the_authority_for_audited_mapping() {
        let mut raw = raw_debug(PreviewLookupRoute::AlteredTable, None);
        raw.lookup_rgb = [62, 68, 18];
        let mut quantized = quantized_debug(73);
        quantized.lookup_rgb = [63, 68, 18];
        let gpu_key = rgb_key(quantized.lookup_rgb);
        let mut lookup = vec![0; gpu_key + 1];
        lookup[gpu_key] = 73;

        assert_eq!(
            expected_gpu_lookup_index(&raw, &quantized, &lookup, &[]),
            Some(73)
        );
    }

    #[test]
    fn quantized_readback_contains_exact_gpu_lookup_rgb() {
        let readback = PreviewPixelDebugReadback {
            request_id: 0,
            source: PreviewPixelSource::Quantized,
            x: 0,
            y: 0,
            width: 1,
            dither: DirectStreamDitherSettings::default(),
        };
        let mut data = vec![0; 256];
        data[..4].copy_from_slice(&[73, 63, 68, 18]);
        let palette = vec![[0, 0, 0, 255]; 74];

        let debug = preview_quantized_pixel_debug(&readback, &data, &palette);

        assert_eq!(debug.palette_index, 73);
        assert_eq!(debug.lookup_rgb, [63, 68, 18]);
    }

    #[test]
    fn stable_srgb_quantization_resolves_half_step_drift_consistently() {
        assert_eq!(stable_srgb_byte(10.49 / 255.0), 10);
        assert_eq!(stable_srgb_byte(10.51 / 255.0), 10);
        assert_eq!(stable_srgb_byte(10.80 / 255.0), 11);
    }

    #[test]
    fn palette_shader_anchors_dither_to_framebuffer_coordinates() {
        let shader = include_str!("../assets/shaders/palette_material_2d.wgsl");

        assert!(shader.contains("floor(mesh.position.xy)"));
        assert!(shader.contains("textureLoad(source_image, source_coord, 0)"));
        assert!(shader.contains("apply_dither(raw_source, framebuffer_coord)"));
        assert!(!shader.contains("source_uv"));
    }

    #[test]
    fn after_click_preserves_direct_table_label_for_raw_direct_text() {
        let raw = raw_debug(PreviewLookupRoute::DirectTable, Some(12));
        let quantized = quantized_debug(12);
        let mut lookup = vec![0; crate::palette_lut::LUT_ENTRY_COUNT + 1];
        lookup[crate::palette_lut::LUT_ENTRY_COUNT] = 12;
        let mut palette = vec![[0, 0, 0, 255]; 13];
        palette[12] = [1, 2, 3, 255];

        let (label, index, color) = preview_expected_readout(
            &raw,
            &quantized,
            PreviewPixelSource::Quantized,
            &lookup,
            &palette,
        );

        assert_eq!(label, "direct IPSMAP table");
        assert_eq!(index, "12");
        assert_eq!(color, [1, 2, 3, 255]);
    }

    #[test]
    fn priority_normalization_preserves_changed_value_and_sums_to_one() {
        let mut settings = PreviewLabSettings {
            bias_lightness: 0.8,
            bias_chroma: 0.3,
            bias_hue: 0.3,
            ..default()
        };

        normalize_preview_priorities(&mut settings, PreviewLabSlider::BiasLightness);

        assert!((settings.bias_lightness - 0.8).abs() < 0.000_001);
        assert!((settings.bias_chroma - 0.1).abs() < 0.000_001);
        assert!((settings.bias_hue - 0.1).abs() < 0.000_001);
        assert!(
            (settings.bias_lightness + settings.bias_chroma + settings.bias_hue - 1.0).abs()
                < 0.000_001
        );
    }

    #[test]
    fn priority_normalization_splits_remaining_when_other_priorities_are_zero() {
        let mut settings = PreviewLabSettings {
            bias_lightness: 0.0,
            bias_chroma: 0.4,
            bias_hue: 0.0,
            ..default()
        };

        normalize_preview_priorities(&mut settings, PreviewLabSlider::BiasChroma);

        assert!((settings.bias_chroma - 0.4).abs() < 0.000_001);
        assert!((settings.bias_lightness - 0.3).abs() < 0.000_001);
        assert!((settings.bias_hue - 0.3).abs() < 0.000_001);
    }
}

pub(crate) fn update_preview_pixel_debug_text(
    debug_state: Option<Res<PreviewPixelDebugState>>,
    mut text_query: Query<&mut Text, With<PreviewPixelDebugText>>,
) {
    let Some(debug_state) = debug_state else {
        return;
    };
    let Ok(mut text) = text_query.single_mut() else {
        return;
    };
    if debug_state.is_changed() {
        text.0.clone_from(&debug_state.output);
    }
}

fn spawn_stats_window(
    commands: &mut Commands,
    custom_host: bool,
    window_layout: &DirectStreamWindowLayout,
) {
    commands
        .spawn((
            Node {
                width: percent(100),
                height: percent(100),
                padding: UiRect {
                    left: px(10),
                    right: px(10.0 + window_layout.right_panel_width),
                    top: px(10),
                    bottom: px(10),
                },
                flex_direction: FlexDirection::Column,
                row_gap: px(6),
                justify_content: JustifyContent::FlexStart,
                align_items: AlignItems::FlexStart,
                ..default()
            },
            BackgroundColor(Color::srgb(0.02, 0.025, 0.035)),
        ))
        .with_child((
            Text::new(initial_stats_text(custom_host)),
            TextFont {
                font_size: FontSize::Px(10.0),
                ..default()
            },
            TextColor(Color::srgb(0.86, 0.92, 0.98)),
            StatsText,
        ))
        .with_children(|parent| {
            parent.spawn((
                Text::new("custom host"),
                TextFont {
                    font_size: FontSize::Px(10.0),
                    ..default()
                },
                TextColor(Color::srgb(0.64, 0.72, 0.80)),
            ));
            parent
                .spawn((Node {
                    width: percent(100),
                    height: px(20),
                    column_gap: px(6),
                    ..default()
                },))
                .with_children(|row| {
                    row.spawn(compact_input_box(
                        "width",
                        CustomWidthInputBox,
                        CustomWidthInputText,
                    ));
                    row.spawn(compact_input_box(
                        "height",
                        CustomHeightInputBox,
                        CustomHeightInputText,
                    ));
                    row.spawn(compact_input_box(
                        "fps",
                        CustomFpsInputBox,
                        CustomFpsInputText,
                    ));
                });
        })
        .with_child((
            Node {
                width: percent(100),
                height: px(24),
                column_gap: px(6),
                ..default()
            },
            children![
                stream_button("Start", StartStreamButton, Color::srgb(0.05, 0.20, 0.13)),
                stream_button("End", StopStreamButton, Color::srgb(0.21, 0.06, 0.07)),
                stream_button("Open", OpenStreamButton, Color::srgb(0.07, 0.10, 0.19)),
                stream_button("Purge Chat", PurgeChatButton, Color::srgb(0.17, 0.10, 0.04)),
            ],
        ))
        .with_child((
            Text::new("stream control: idle - Ready"),
            TextFont {
                font_size: FontSize::Px(10.0),
                ..default()
            },
            TextColor(Color::srgb(0.70, 0.78, 0.86)),
            StreamControlStatusText,
        ));
}

fn initial_stats_text(custom_host: bool) -> String {
    let mode = if custom_host {
        "custom host stats"
    } else {
        "stats"
    };
    let endpoint = if custom_host {
        "http://127.0.0.1:8080".to_owned()
    } else {
        format!("http://{WEB_ADDR}")
    };
    format!(
        "PixelStream\n{}\n{}\n{}",
        stat_line("mode", mode),
        stat_line(
            "stream",
            &format!("{STREAM_WIDTH}x{STREAM_HEIGHT} @ {STREAM_FPS} fps")
        ),
        stat_line("browser", &endpoint),
    )
}

fn compact_input_box<T: Component, U: Component>(
    placeholder: &'static str,
    box_marker: T,
    text_marker: U,
) -> impl Bundle {
    (
        Button,
        Node {
            flex_grow: 1.0,
            flex_basis: px(0),
            height: px(20),
            padding: UiRect::horizontal(px(6)),
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::srgb(0.045, 0.055, 0.07)),
        BorderColor::all(Color::srgb(0.16, 0.22, 0.30)),
        box_marker,
        children![(
            Text::new(placeholder),
            TextFont {
                font_size: FontSize::Px(10.0),
                ..default()
            },
            TextColor(Color::srgb(0.86, 0.92, 0.98)),
            text_marker,
        )],
    )
}

fn stream_button<T: Component>(label: &'static str, marker: T, color: Color) -> impl Bundle {
    (
        Button,
        Node {
            flex_grow: 1.0,
            flex_basis: px(0),
            height: px(24),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
        BackgroundColor(color),
        marker,
        children![(
            Text::new(label),
            TextFont {
                font_size: FontSize::Px(11.0),
                ..default()
            },
            TextColor(Color::srgb(0.92, 0.96, 1.0)),
        )],
    )
}

pub(crate) fn update_stats_window(
    config: Res<AppConfig>,
    target: Res<DirectStreamTarget>,
    stats: Res<SharedStats>,
    time: Res<Time>,
    mut clock: ResMut<StatsWindowUpdateClock>,
    mut query: Query<&mut Text, With<StatsText>>,
) {
    if !clock.timer.tick(time.delta()).just_finished() {
        return;
    }
    let Ok(mut text) = query.single_mut() else {
        return;
    };

    if let Ok(stats) = stats.0.lock() {
        text.0 = if config.custom_host {
            custom_host_stats_text(&stats, &target)
        } else {
            preview_stats_text(&stats, &target)
        };
    }
}

fn custom_host_stats_text(
    stats: &crate::stats::StreamStats,
    target: &DirectStreamTarget,
) -> String {
    [
        "PixelStream".to_owned(),
        stat_line("mode", "custom host stats"),
        stat_line(
            "stream",
            &format!("{}x{} @ {} fps", target.width, target.height, target.fps),
        ),
        stat_line("browser", &format!("http://{WEB_ADDR}")),
        String::new(),
        "capture".to_owned(),
        stat_line("frames captured", &stats.frames_captured.to_string()),
        stat_line("frames read", &stats.frames_read.to_string()),
        stat_line("frames encoded", &stats.frames_encoded.to_string()),
        stat_line("frames dropped", &stats.frames_dropped.to_string()),
        stat_line("sent fps", &format!("{:.2} fps", stats.custom_actual_fps)),
        stat_line("app fps", &format!("{:.2} fps", stats.custom_app_fps)),
        stat_line("batch size", &stats.custom_batch_size.to_string()),
        stat_line(
            "batch buffered",
            &stats.custom_batch_buffered_frames.to_string(),
        ),
        stat_line(
            "pending readbacks",
            &stats.custom_pending_readbacks.to_string(),
        ),
        stat_line(
            "batch latency",
            &format!("{:.2} ms", stats.custom_batch_latency_ms),
        ),
        stat_line(
            "http batch",
            &format!(
                "{} last / {:.1} avg",
                stats.custom_http_batch_last_frames, stats.custom_http_batch_avg_frames
            ),
        ),
        stat_line(
            "readback wait",
            &format!(
                "{:.2} ms last / {:.2} ms avg",
                stats.custom_readback_wait_last_ms, stats.custom_readback_wait_avg_ms
            ),
        ),
        stat_line(
            "readback cpu",
            &format!(
                "{:.2} ms last / {:.2} ms avg",
                stats.custom_readback_cpu_last_ms, stats.custom_readback_cpu_avg_ms
            ),
        ),
        stat_line(
            "encode",
            &format!(
                "{:.2} ms last / {:.2} ms avg",
                stats.custom_encode_last_ms, stats.custom_encode_avg_ms
            ),
        ),
        stat_line(
            "record write",
            &format!(
                "{:.2} ms last / {:.2} ms avg",
                stats.custom_record_last_ms, stats.custom_record_avg_ms
            ),
        ),
        stat_line(
            "publish",
            &format!(
                "{:.2} ms last / {:.2} ms avg",
                stats.custom_publish_last_ms, stats.custom_publish_avg_ms
            ),
        ),
        stat_line(
            "pipeline total",
            &format!(
                "{:.2} ms last / {:.2} ms avg",
                stats.custom_pipeline_last_ms, stats.custom_pipeline_avg_ms
            ),
        ),
        String::new(),
        "custom host".to_owned(),
        stat_line("palette mode", "ipsmap LUT"),
        stat_line("stage", stats.custom_stage),
        stat_line("error", &stats.custom_last_error),
        stat_line("packets sent", &stats.custom_frames_sent.to_string()),
        stat_line(
            "packet types",
            &format!(
                "key {} / delta {}",
                stats.custom_keyframes_sent, stats.custom_delta_frames_sent
            ),
        ),
        stat_line(
            "tile modes",
            &format!(
                "raw {} solid {} rle {} span {} xor {} cached {} skipped {}",
                stats.custom_raw_tiles_sent,
                stats.custom_solid_tiles_sent,
                stats.custom_rle_tiles_sent,
                stats.custom_span_tiles_sent,
                stats.custom_xor_tiles_sent,
                stats.custom_cached_tiles_sent,
                stats.custom_skipped_tiles
            ),
        ),
        stat_line("packet drops", &stats.custom_frames_dropped.to_string()),
        stat_line(
            "queue full drops",
            &stats.custom_queue_full_drops.to_string(),
        ),
        stat_line(
            "sender waits",
            &format!(
                "{} timeouts / {} wakeups",
                stats.custom_sender_wait_timeouts, stats.custom_sender_wait_wakeups
            ),
        ),
        stat_line("bytes sent", &stats.custom_bytes_sent.to_string()),
        stat_line(
            "audio packets",
            &stats.custom_audio_packets_sent.to_string(),
        ),
        stat_line("audio bytes", &stats.custom_audio_bytes_sent.to_string()),
        stat_line(
            "audio delay",
            &format!("{} ms", stats.custom_audio_delay_ms),
        ),
        stat_line(
            "latest packet",
            &format!("{} bytes", stats.latest_frame_bytes),
        ),
        stat_line("recording", &stats.custom_recording_path),
        stat_line("clients", &stats.stream_clients.to_string()),
        stat_line("page requests", &stats.preview_requests.to_string()),
    ]
    .join("\n")
}

fn preview_stats_text(stats: &crate::stats::StreamStats, target: &DirectStreamTarget) -> String {
    [
        "PixelStream".to_owned(),
        stat_line("mode", "preview stats"),
        stat_line(
            "stream",
            &format!("{}x{} @ {} fps", target.width, target.height, target.fps),
        ),
        stat_line("local preview", &format!("http://{WEB_ADDR}")),
        String::new(),
        "capture".to_owned(),
        stat_line("frames captured", &stats.frames_captured.to_string()),
        stat_line("frames read", &stats.frames_read.to_string()),
        stat_line("frames encoded", &stats.frames_encoded.to_string()),
        stat_line("frames dropped", &stats.frames_dropped.to_string()),
        String::new(),
        "preview".to_owned(),
        stat_line("preview drops", &stats.preview_frames_dropped.to_string()),
        stat_line("clients", &stats.stream_clients.to_string()),
        stat_line("requests", &stats.preview_requests.to_string()),
        stat_line(
            "latest frame",
            &format!("{} bytes", stats.latest_frame_bytes),
        ),
    ]
    .join("\n")
}

fn stat_line(label: &str, value: &str) -> String {
    format!("{label:>16}: {value}")
}
