//! Bevy render graph integration for palette conversion.
//!
//! This module routes the scene render target through the IPSMAP lookup shader,
//! handles preview/custom-host target swapping, and applies ordered dithering to
//! captured scene pixels before quantization.

use crate::{
    config::{AppConfig, WindowMode},
    constants::INITIAL_RENDER_SETTLE_FRAMES,
    palette::SharedPaletteBias,
    palette_lut::PaletteLookup,
    public_types::{DirectStreamDitherSettings, DirectStreamTarget},
    scene::{
        PendingReadback, PreviewLoadingText, PreviewPixelDebugState, PreviewStreamVisual,
        RenderedBatchFrame, StreamReadback,
    },
    stream_control::StreamControl,
};
use bevy::{
    asset::{RenderAssetUsages, load_internal_asset, uuid_handle},
    camera::{RenderTarget, visibility::RenderLayers},
    prelude::*,
    reflect::TypePath,
    render::gpu_readback::Readback,
    render::render_resource::{
        AsBindGroup, Extent3d, TextureDimension, TextureFormat, TextureUsages,
    },
    shader::{Shader, ShaderRef},
    sprite_render::{Material2d, Material2dPlugin},
};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) const GPU_PALETTE_LAYER: usize = 1;
pub(crate) const GPU_DIRECT_TEXT_LAYER: usize = 2;
pub(crate) const GPU_PREVIEW_DISPLAY_LAYER: usize = 3;
pub(crate) const GPU_PREVIEW_RAW_CAPTURE_LAYER: usize = 4;
pub(crate) const GPU_DIRECT_RAW_TEXT_LAYER: usize = 5;
pub(crate) const GPU_PREVIEW_RAW_SNAPSHOT_LAYER: usize = 6;
pub(crate) const GPU_PREVIEW_PALETTE_AUDIT_LAYER: usize = 7;
pub(crate) const INDEXED_DIRECT_OVERLAY_MARKER: f32 = 1.0;
pub(crate) fn indexed_unorm_byte(value: u8) -> f32 {
    let encoded = ((f32::from(value) + 0.25) / 255.0).min(1.0);
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}
const PALETTE_SHADER_HANDLE: Handle<Shader> = uuid_handle!("b69538c2-4fa1-4a12-89a5-32986e423f4d");
const PALETTE_PREVIEW_DISPLAY_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("8ac093df-eddb-44f3-96c7-435d6c7e00a9");
const RAW_PREVIEW_COPY_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("2fe46520-bd33-4420-8c37-3961ce605e77");
const RAW_PREVIEW_DISPLAY_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("52f15af4-c6dd-4ed7-bb24-113c21f862e7");
const RAW_PREVIEW_SNAPSHOT_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("68f56d29-7d23-475f-a3f0-fd0420c60de5");

pub(crate) struct GpuPalettePlugin;

impl Plugin for GpuPalettePlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(
            app,
            PALETTE_SHADER_HANDLE,
            "../assets/shaders/palette_material_2d.wgsl",
            Shader::from_wgsl
        );
        load_internal_asset!(
            app,
            RAW_PREVIEW_SNAPSHOT_SHADER_HANDLE,
            "../assets/shaders/raw_preview_snapshot_2d.wgsl",
            Shader::from_wgsl
        );
        load_internal_asset!(
            app,
            PALETTE_PREVIEW_DISPLAY_SHADER_HANDLE,
            "../assets/shaders/palette_preview_display_2d.wgsl",
            Shader::from_wgsl
        );
        load_internal_asset!(
            app,
            RAW_PREVIEW_COPY_SHADER_HANDLE,
            "../assets/shaders/raw_preview_copy_2d.wgsl",
            Shader::from_wgsl
        );
        load_internal_asset!(
            app,
            RAW_PREVIEW_DISPLAY_SHADER_HANDLE,
            "../assets/shaders/raw_preview_display_2d.wgsl",
            Shader::from_wgsl
        );

        app.init_resource::<DirectStreamDitherSettings>()
            .add_plugins((
                Material2dPlugin::<PaletteMaterial>::default(),
                Material2dPlugin::<PalettePreviewDisplayMaterial>::default(),
                Material2dPlugin::<RawPreviewCopyMaterial>::default(),
                Material2dPlugin::<RawPreviewDisplayMaterial>::default(),
                Material2dPlugin::<RawPreviewSnapshotMaterial>::default(),
            ))
            .add_systems(Update, sync_palette_material_bias)
            .add_systems(Update, sync_palette_material_dither)
            .add_systems(Update, throttle_preview_palette_cameras)
            .add_systems(Update, cycle_camera_render_targets);
    }
}

#[derive(AsBindGroup, Debug, Clone, Asset, TypePath)]
pub(crate) struct PaletteMaterial {
    #[uniform(0)]
    pub(crate) params: Vec4,
    #[texture(1)]
    #[sampler(2)]
    pub(crate) source_image: Handle<Image>,
    #[texture(3)]
    pub(crate) palette_texture: Handle<Image>,
    #[uniform(4)]
    pub(crate) lookup_params: Vec4,
    #[texture(5, sample_type = "u_int")]
    pub(crate) lookup_texture: Handle<Image>,
    #[uniform(6)]
    pub(crate) input_offset_a: Vec4,
    #[uniform(7)]
    pub(crate) input_offset_b: Vec4,
    #[uniform(8)]
    pub(crate) dither_a: Vec4,
    #[uniform(9)]
    pub(crate) dither_b: Vec4,
}

impl Material2d for PaletteMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(PALETTE_SHADER_HANDLE)
    }
}

#[derive(AsBindGroup, Debug, Clone, Asset, TypePath)]
pub(crate) struct PalettePreviewDisplayMaterial {
    #[uniform(0)]
    pub(crate) params: Vec4,
    #[texture(1)]
    #[sampler(2)]
    pub(crate) source_image: Handle<Image>,
    #[texture(3)]
    pub(crate) palette_texture: Handle<Image>,
    #[texture(5, sample_type = "u_int")]
    pub(crate) lookup_texture: Handle<Image>,
    #[uniform(4)]
    pub(crate) lookup_params: Vec4,
    #[uniform(6)]
    pub(crate) input_offset_a: Vec4,
    #[uniform(7)]
    pub(crate) input_offset_b: Vec4,
    #[uniform(8)]
    pub(crate) dither_a: Vec4,
    #[uniform(9)]
    pub(crate) dither_b: Vec4,
}

impl Material2d for PalettePreviewDisplayMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(PALETTE_PREVIEW_DISPLAY_SHADER_HANDLE)
    }
}

#[derive(AsBindGroup, Debug, Clone, Asset, TypePath)]
pub(crate) struct RawPreviewCopyMaterial {
    #[texture(0)]
    #[sampler(1)]
    pub(crate) source_image: Handle<Image>,
    #[uniform(2)]
    pub(crate) dither_a: Vec4,
    #[uniform(3)]
    pub(crate) dither_b: Vec4,
}

impl Material2d for RawPreviewCopyMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(RAW_PREVIEW_COPY_SHADER_HANDLE)
    }
}

#[derive(AsBindGroup, Debug, Clone, Asset, TypePath)]
pub(crate) struct RawPreviewDisplayMaterial {
    #[texture(0)]
    #[sampler(1)]
    pub(crate) source_image: Handle<Image>,
}

#[derive(AsBindGroup, Debug, Clone, Asset, TypePath)]
pub(crate) struct RawPreviewSnapshotMaterial {
    #[texture(0)]
    #[sampler(1)]
    pub(crate) source_image: Handle<Image>,
}

impl Material2d for RawPreviewSnapshotMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(RAW_PREVIEW_SNAPSHOT_SHADER_HANDLE)
    }
}

impl Material2d for RawPreviewDisplayMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(RAW_PREVIEW_DISPLAY_SHADER_HANDLE)
    }
}

#[derive(Component)]
pub(crate) struct PreviewRawDisplay;

#[derive(Resource, Clone)]
pub(crate) struct GpuPalettePipeline {
    pub(crate) material: Handle<PaletteMaterial>,
    pub(crate) source_copy_material: Handle<RawPreviewCopyMaterial>,
    pub(crate) raw_snapshot_material: Handle<RawPreviewSnapshotMaterial>,
    pub(crate) palette_texture: Handle<Image>,
    pub(crate) lookup_texture: Handle<Image>,
    pub(crate) capture_image: Handle<Image>,
    pub(crate) source_copy_camera: Entity,
    pub(crate) palette_camera: Entity,
    pub(crate) raw_overlay_camera: Entity,
    pub(crate) overlay_camera: Entity,
    pub(crate) raw_snapshot_camera: Entity,
    pub(crate) audit_camera: Option<Entity>,
    pub(crate) overlay_mask_camera: Option<Entity>,
    pub(crate) source_copy_quad_entity: Entity,
    pub(crate) quad_entity: Entity,
    pub(crate) raw_snapshot_quad_entity: Entity,
    pub(crate) audit_quad_entity: Option<Entity>,
    pub(crate) source_images: Vec<Handle<Image>>,
    pub(crate) output_images: Vec<Handle<Image>>,
    pub(crate) audit_images: Vec<Handle<Image>>,
    pub(crate) overlay_mask_images: Vec<Handle<Image>>,
    pub(crate) current_output_index: usize,
    pub(crate) palette_count: usize,
    pub(crate) palette_colors: Vec<[u8; 4]>,
    pub(crate) lookup_entries: Arc<[u8]>,
    pub(crate) overlay_enabled: bool,
}

#[derive(Resource)]
pub(crate) struct PreviewPaletteThrottle {
    frame_interval: Duration,
    frame_accumulator: Duration,
    batch_size: usize,
    queued_output_indices: VecDeque<usize>,
    pub(crate) display_material: Handle<PalettePreviewDisplayMaterial>,
    pub(crate) raw_display_material: Handle<RawPreviewDisplayMaterial>,
    warmup_frames_remaining: usize,
    delay_filled: bool,
}

impl PreviewPaletteThrottle {
    pub(crate) fn new(
        fps: u32,
        batch_size: usize,
        display_material: Handle<PalettePreviewDisplayMaterial>,
        raw_display_material: Handle<RawPreviewDisplayMaterial>,
    ) -> Self {
        let frame_interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
        Self {
            frame_interval,
            frame_accumulator: frame_interval,
            batch_size: batch_size.max(1),
            queued_output_indices: VecDeque::with_capacity(batch_size.max(1)),
            display_material,
            raw_display_material,
            warmup_frames_remaining: batch_size.max(1),
            delay_filled: false,
        }
    }
}

pub(crate) fn make_stream_source_image(width: u32, height: u32) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Bgra8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_DST
        | TextureUsages::COPY_SRC
        | TextureUsages::RENDER_ATTACHMENT;
    image
}

pub(crate) fn make_stream_output_image(width: u32, height: u32) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_DST
        | TextureUsages::COPY_SRC
        | TextureUsages::RENDER_ATTACHMENT;
    image
}

fn make_overlay_mask_image(width: u32, height: u32) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 0],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage = TextureUsages::COPY_SRC | TextureUsages::RENDER_ATTACHMENT;
    image
}

pub(crate) fn make_palette_source_image(width: u32, height: u32) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Bgra8Unorm,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_DST
        | TextureUsages::COPY_SRC
        | TextureUsages::RENDER_ATTACHMENT;
    image
}

pub(crate) fn make_palette_texture(colors: &[[u8; 4]]) -> Image {
    let width = colors.len().max(1) as u32;
    let mut data = Vec::with_capacity(width as usize * 4);
    if colors.is_empty() {
        data.extend_from_slice(&[0, 0, 0, 255]);
    } else {
        for color in colors {
            data.extend_from_slice(color);
        }
    }

    let mut image = Image::new_fill(
        Extent3d {
            width,
            height: 1,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &data,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;
    image
}

pub(crate) fn make_lookup_texture(lookup: &PaletteLookup) -> Image {
    make_lookup_texture_from_entries(lookup.entries())
}

pub(crate) fn lookup_entries_arc(lookup: &PaletteLookup) -> Arc<[u8]> {
    Arc::from(lookup.entries().to_vec().into_boxed_slice())
}

pub(crate) fn make_lookup_texture_from_entries(entries: &[u8]) -> Image {
    let data = entries.to_vec();
    let height = (data.len() / 4096).max(4096) as u32;
    let mut image = Image::new_fill(
        Extent3d {
            width: 4096,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &data,
        TextureFormat::R8Uint,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage =
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::COPY_SRC;
    image
}

pub(crate) fn spawn_custom_host_pipeline(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<PaletteMaterial>,
    raw_copy_materials: &mut Assets<RawPreviewCopyMaterial>,
    raw_snapshot_materials: &mut Assets<RawPreviewSnapshotMaterial>,
    width: u32,
    height: u32,
    source_image: Handle<Image>,
    palette_colors: &[[u8; 4]],
    palette_bias: crate::palette::PaletteBias,
    palette_lookup: &PaletteLookup,
    target: &mut DirectStreamTarget,
    batch_size: usize,
    overlay_enabled: bool,
    audit_enabled: bool,
) -> GpuPalettePipeline {
    let output_images: Vec<Handle<Image>> = (0..output_image_count(batch_size))
        .map(|_| images.add(make_stream_output_image(width, height)))
        .collect();
    let source_images: Vec<Handle<Image>> = (0..output_image_count(batch_size))
        .map(|_| images.add(make_palette_source_image(width, height)))
        .collect();
    let audit_images: Vec<Handle<Image>> = if audit_enabled {
        (0..output_image_count(batch_size))
            .map(|_| images.add(make_stream_output_image(width, height)))
            .collect()
    } else {
        Vec::new()
    };
    let overlay_mask_images: Vec<Handle<Image>> = if audit_enabled {
        (0..output_image_count(batch_size))
            .map(|_| images.add(make_overlay_mask_image(width, height)))
            .collect()
    } else {
        Vec::new()
    };
    let first_output = output_images.first().cloned().unwrap();
    let first_source = source_images.first().cloned().unwrap();
    let first_audit = audit_images.first().cloned();
    let first_overlay_mask = overlay_mask_images.first().cloned();
    let capture_image = images.add(make_palette_source_image(width, height));
    let palette_texture = images.add(make_palette_texture(palette_colors));
    let lookup_texture = images.add(make_lookup_texture(palette_lookup));
    let source_copy_material = raw_copy_materials.add(RawPreviewCopyMaterial {
        source_image: source_image.clone(),
        dither_a: Vec4::ZERO,
        dither_b: Vec4::ZERO,
    });
    let raw_snapshot_material = raw_snapshot_materials.add(RawPreviewSnapshotMaterial {
        source_image: capture_image.clone(),
    });
    let material = materials.add(PaletteMaterial {
        params: palette_material_params(&palette_bias, palette_colors.len()),
        source_image: capture_image.clone(),
        palette_texture: palette_texture.clone(),
        lookup_params: palette_lookup_params(),
        lookup_texture: lookup_texture.clone(),
        input_offset_a: palette_input_offset_a(&palette_bias),
        input_offset_b: palette_input_offset_b(&palette_bias),
        dither_a: Vec4::ZERO,
        dither_b: Vec4::ZERO,
    });

    let source_copy_camera = commands
        .spawn((
            Camera2d,
            Msaa::Off,
            Camera {
                order: 0,
                clear_color: ClearColorConfig::Custom(Color::BLACK),
                ..default()
            },
            RenderTarget::Image(capture_image.clone().into()),
            RenderLayers::layer(GPU_PREVIEW_RAW_CAPTURE_LAYER),
        ))
        .id();

    let palette_camera = commands
        .spawn((
            Camera2d,
            Msaa::Off,
            Camera {
                order: 2,
                clear_color: ClearColorConfig::Custom(Color::BLACK),
                ..default()
            },
            RenderTarget::Image(first_output.clone().into()),
            RenderLayers::layer(GPU_PALETTE_LAYER),
        ))
        .id();

    let raw_overlay_camera = commands
        .spawn((
            Camera2d,
            Msaa::Off,
            Camera {
                order: 1,
                clear_color: ClearColorConfig::None,
                is_active: overlay_enabled,
                ..default()
            },
            RenderTarget::Image(capture_image.clone().into()),
            RenderLayers::layer(GPU_DIRECT_RAW_TEXT_LAYER),
        ))
        .id();

    let overlay_camera = commands
        .spawn((
            Camera2d,
            Msaa::Off,
            Camera {
                order: 3,
                clear_color: ClearColorConfig::None,
                is_active: overlay_enabled,
                ..default()
            },
            RenderTarget::Image(first_output.clone().into()),
            RenderLayers::layer(GPU_DIRECT_TEXT_LAYER),
        ))
        .id();

    let raw_snapshot_camera = commands
        .spawn((
            Camera2d,
            Msaa::Off,
            Camera {
                order: 4,
                clear_color: ClearColorConfig::Custom(Color::BLACK),
                is_active: false,
                ..default()
            },
            RenderTarget::Image(first_source.clone().into()),
            RenderLayers::layer(GPU_PREVIEW_RAW_SNAPSHOT_LAYER),
        ))
        .id();

    let audit_camera = first_audit.as_ref().map(|first_audit| {
        commands
            .spawn((
                Camera2d,
                Msaa::Off,
                Camera {
                    order: 2,
                    clear_color: ClearColorConfig::Custom(Color::BLACK),
                    ..default()
                },
                RenderTarget::Image(first_audit.clone().into()),
                RenderLayers::layer(GPU_PREVIEW_PALETTE_AUDIT_LAYER),
            ))
            .id()
    });
    let overlay_mask_camera = first_overlay_mask.as_ref().map(|first_overlay_mask| {
        commands
            .spawn((
                Camera2d,
                Msaa::Off,
                Camera {
                    order: 3,
                    clear_color: ClearColorConfig::Custom(Color::NONE),
                    ..default()
                },
                RenderTarget::Image(first_overlay_mask.clone().into()),
                RenderLayers::layer(GPU_DIRECT_TEXT_LAYER),
            ))
            .id()
    });

    let source_copy_quad_entity = commands
        .spawn((
            Mesh2d(meshes.add(Rectangle::default())),
            MeshMaterial2d(source_copy_material.clone()),
            Transform::from_scale(Vec3::new(width as f32, height as f32, 1.0)),
            RenderLayers::layer(GPU_PREVIEW_RAW_CAPTURE_LAYER),
        ))
        .id();

    let quad_entity = commands
        .spawn((
            Mesh2d(meshes.add(Rectangle::default())),
            MeshMaterial2d(material.clone()),
            Transform::from_scale(Vec3::new(width as f32, height as f32, 1.0)),
            RenderLayers::layer(GPU_PALETTE_LAYER),
        ))
        .id();

    let raw_snapshot_quad_entity = commands
        .spawn((
            Mesh2d(meshes.add(Rectangle::default())),
            MeshMaterial2d(raw_snapshot_material.clone()),
            Transform::from_scale(Vec3::new(width as f32, height as f32, 1.0)),
            RenderLayers::layer(GPU_PREVIEW_RAW_SNAPSHOT_LAYER),
        ))
        .id();
    let audit_quad_entity = audit_camera.map(|_| {
        commands
            .spawn((
                Mesh2d(meshes.add(Rectangle::default())),
                MeshMaterial2d(material.clone()),
                Transform::from_scale(Vec3::new(width as f32, height as f32, 1.0)),
                RenderLayers::layer(GPU_PREVIEW_PALETTE_AUDIT_LAYER),
            ))
            .id()
    });

    target.output_image = first_output.clone();
    target.output_is_indexed = true;
    target.overlay_camera = overlay_camera;
    target.overlay_layer = GPU_DIRECT_TEXT_LAYER;
    target.raw_overlay_layer = Some(GPU_DIRECT_RAW_TEXT_LAYER);

    GpuPalettePipeline {
        material,
        source_copy_material,
        raw_snapshot_material,
        palette_texture,
        lookup_texture,
        capture_image,
        source_copy_camera,
        palette_camera,
        raw_overlay_camera,
        overlay_camera,
        raw_snapshot_camera,
        audit_camera,
        overlay_mask_camera,
        source_copy_quad_entity,
        quad_entity,
        raw_snapshot_quad_entity,
        audit_quad_entity,
        source_images,
        output_images,
        audit_images,
        overlay_mask_images,
        current_output_index: 0,
        palette_count: palette_colors.len(),
        palette_colors: palette_colors.to_vec(),
        lookup_entries: lookup_entries_arc(palette_lookup),
        overlay_enabled,
    }
}

pub(crate) fn retarget_custom_host_pipeline(
    pipeline: &mut GpuPalettePipeline,
    images: &mut Assets<Image>,
    materials: &mut Assets<PaletteMaterial>,
    raw_copy_materials: &mut Assets<RawPreviewCopyMaterial>,
    raw_snapshot_materials: &mut Assets<RawPreviewSnapshotMaterial>,
    camera_targets: &mut Query<&mut RenderTarget>,
    quad_transforms: &mut Query<&mut Transform>,
    width: u32,
    height: u32,
    source_image: Handle<Image>,
    palette_lookup: &PaletteLookup,
    target: &mut DirectStreamTarget,
    batch_size: usize,
) -> Result<(), ()> {
    let output_images: Vec<Handle<Image>> = (0..output_image_count(batch_size))
        .map(|_| images.add(make_stream_output_image(width, height)))
        .collect();
    let source_images: Vec<Handle<Image>> = (0..output_image_count(batch_size))
        .map(|_| images.add(make_palette_source_image(width, height)))
        .collect();
    let audit_images: Vec<Handle<Image>> = if pipeline.audit_camera.is_some() {
        (0..output_image_count(batch_size))
            .map(|_| images.add(make_stream_output_image(width, height)))
            .collect()
    } else {
        Vec::new()
    };
    let overlay_mask_images: Vec<Handle<Image>> = if pipeline.overlay_mask_camera.is_some() {
        (0..output_image_count(batch_size))
            .map(|_| images.add(make_overlay_mask_image(width, height)))
            .collect()
    } else {
        Vec::new()
    };
    let first_output = output_images.first().cloned().unwrap();
    let first_source = source_images.first().cloned().unwrap();
    let first_audit = audit_images.first().cloned();
    let first_overlay_mask = overlay_mask_images.first().cloned();
    let capture_image = images.add(make_palette_source_image(width, height));

    if let Ok(mut camera_target) = camera_targets.get_mut(pipeline.source_copy_camera) {
        *camera_target = RenderTarget::Image(capture_image.clone().into());
    } else {
        return Err(());
    }

    if let Ok(mut camera_target) = camera_targets.get_mut(pipeline.palette_camera) {
        *camera_target = RenderTarget::Image(first_output.clone().into());
    } else {
        return Err(());
    }

    if let Ok(mut camera_target) = camera_targets.get_mut(pipeline.raw_overlay_camera) {
        *camera_target = RenderTarget::Image(capture_image.clone().into());
    } else {
        return Err(());
    }

    if let Ok(mut camera_target) = camera_targets.get_mut(pipeline.overlay_camera) {
        *camera_target = RenderTarget::Image(first_output.clone().into());
    } else {
        return Err(());
    }

    if let Ok(mut camera_target) = camera_targets.get_mut(pipeline.raw_snapshot_camera) {
        *camera_target = RenderTarget::Image(first_source.clone().into());
    } else {
        return Err(());
    }

    if let (Some(audit_camera), Some(first_audit)) = (pipeline.audit_camera, first_audit)
        && let Ok(mut camera_target) = camera_targets.get_mut(audit_camera)
    {
        *camera_target = RenderTarget::Image(first_audit.into());
    }

    if let (Some(mask_camera), Some(first_overlay_mask)) =
        (pipeline.overlay_mask_camera, first_overlay_mask)
        && let Ok(mut camera_target) = camera_targets.get_mut(mask_camera)
    {
        *camera_target = RenderTarget::Image(first_overlay_mask.into());
    }

    if let Some(mut material) = raw_copy_materials.get_mut(&pipeline.source_copy_material) {
        material.source_image = source_image;
    } else {
        return Err(());
    }

    if let Some(mut material) = materials.get_mut(&pipeline.material) {
        material.source_image = capture_image.clone();
        let lookup_texture = images.add(make_lookup_texture(palette_lookup));
        material.lookup_texture = lookup_texture.clone();
        material.lookup_params = palette_lookup_params();
        pipeline.lookup_texture = lookup_texture;
        pipeline.lookup_entries = lookup_entries_arc(palette_lookup);
    } else {
        return Err(());
    }

    if let Some(mut material) = raw_snapshot_materials.get_mut(&pipeline.raw_snapshot_material) {
        material.source_image = capture_image.clone();
    } else {
        return Err(());
    }

    if let Ok(mut transform) = quad_transforms.get_mut(pipeline.source_copy_quad_entity) {
        transform.scale = Vec3::new(width as f32, height as f32, 1.0);
    } else {
        return Err(());
    }

    if let Ok(mut transform) = quad_transforms.get_mut(pipeline.quad_entity) {
        transform.scale = Vec3::new(width as f32, height as f32, 1.0);
    } else {
        return Err(());
    }

    if let Ok(mut transform) = quad_transforms.get_mut(pipeline.raw_snapshot_quad_entity) {
        transform.scale = Vec3::new(width as f32, height as f32, 1.0);
    } else {
        return Err(());
    }

    if let Some(audit_quad_entity) = pipeline.audit_quad_entity
        && let Ok(mut transform) = quad_transforms.get_mut(audit_quad_entity)
    {
        transform.scale = Vec3::new(width as f32, height as f32, 1.0);
    }

    pipeline.capture_image = capture_image;
    pipeline.source_images = source_images;
    pipeline.output_images = output_images;
    pipeline.audit_images = audit_images;
    pipeline.overlay_mask_images = overlay_mask_images;
    pipeline.current_output_index = 0;
    target.output_image = first_output;
    target.output_is_indexed = true;
    target.overlay_camera = pipeline.overlay_camera;
    target.overlay_layer = GPU_DIRECT_TEXT_LAYER;
    target.raw_overlay_layer = Some(GPU_DIRECT_RAW_TEXT_LAYER);

    Ok(())
}

fn sync_palette_material_bias(
    config: Res<AppConfig>,
    palette_bias: Option<Res<SharedPaletteBias>>,
    pipeline: Option<Res<GpuPalettePipeline>>,
    mut materials: ResMut<Assets<PaletteMaterial>>,
) {
    if config.window_mode == WindowMode::Preview {
        return;
    }
    let (Some(palette_bias), Some(pipeline)) = (palette_bias, pipeline) else {
        return;
    };
    if !palette_bias.is_changed() {
        return;
    }

    if let Some(mut material) = materials.get_mut(&pipeline.material) {
        let bias = palette_bias.get();
        material.params = palette_material_params(&bias, pipeline.palette_count);
        material.input_offset_a = palette_input_offset_a(&bias);
        material.input_offset_b = palette_input_offset_b(&bias);
    }
}

fn sync_palette_material_dither(
    dither: Option<Res<DirectStreamDitherSettings>>,
    pipeline: Option<Res<GpuPalettePipeline>>,
    throttle: Option<Res<PreviewPaletteThrottle>>,
    mut materials: ResMut<Assets<PaletteMaterial>>,
    mut display_materials: ResMut<Assets<PalettePreviewDisplayMaterial>>,
) {
    let (Some(dither), Some(pipeline)) = (dither, pipeline) else {
        return;
    };
    if !dither.is_changed() {
        return;
    }

    let dither_a = Vec4::new(
        dither.scale.max(0.125),
        dither.intensity.max(0.0),
        dither.value_strength,
        dither.chroma_strength,
    );
    let dither_b = Vec4::new(dither.hue_strength, 0.0, 0.0, 0.0);
    if let Some(mut material) = materials.get_mut(&pipeline.material)
        && (material.dither_a != dither_a || material.dither_b != dither_b)
    {
        material.dither_a = dither_a;
        material.dither_b = dither_b;
    }
    if let Some(throttle) = throttle
        && let Some(mut material) = display_materials.get_mut(&throttle.display_material)
        && (material.dither_a != dither_a || material.dither_b != dither_b)
    {
        material.dither_a = dither_a;
        material.dither_b = dither_b;
    }
}

fn throttle_preview_palette_cameras(
    time: Res<Time>,
    config: Res<AppConfig>,
    pipeline: Option<ResMut<GpuPalettePipeline>>,
    throttle: Option<ResMut<PreviewPaletteThrottle>>,
    mut display_materials: ResMut<Assets<PalettePreviewDisplayMaterial>>,
    mut raw_display_materials: ResMut<Assets<RawPreviewDisplayMaterial>>,
    mut debug_state: Option<ResMut<PreviewPixelDebugState>>,
    mut camera_targets: Query<&mut RenderTarget>,
    mut cameras: Query<&mut Camera>,
    mut preview_visuals: Query<&mut Visibility, With<PreviewStreamVisual>>,
    mut loading_text: Query<
        &mut Visibility,
        (With<PreviewLoadingText>, Without<PreviewStreamVisual>),
    >,
) {
    if config.window_mode != WindowMode::Preview {
        return;
    }

    let (Some(mut pipeline), Some(mut throttle)) = (pipeline, throttle) else {
        return;
    };

    let frame_interval = throttle.frame_interval;
    throttle.frame_accumulator += time.delta();
    let should_render = throttle.frame_accumulator >= frame_interval;
    if should_render {
        while throttle.frame_accumulator >= frame_interval {
            throttle.frame_accumulator -= frame_interval;
        }
    }

    if should_render && !pipeline.output_images.is_empty() && !pipeline.source_images.is_empty() {
        let output_index = pipeline.current_output_index;
        let output_image = pipeline.output_images[output_index].clone();
        let source_image = pipeline.source_images[output_index].clone();
        if let Ok(mut palette_target) = camera_targets.get_mut(pipeline.palette_camera) {
            *palette_target = RenderTarget::Image(output_image.clone().into());
        }
        if let Ok(mut overlay_target) = camera_targets.get_mut(pipeline.overlay_camera) {
            *overlay_target = RenderTarget::Image(output_image.clone().into());
        }
        if let Ok(mut raw_snapshot_target) = camera_targets.get_mut(pipeline.raw_snapshot_camera) {
            *raw_snapshot_target = RenderTarget::Image(source_image.clone().into());
        }
        if let (Some(audit_camera), Some(audit_image)) = (
            pipeline.audit_camera,
            pipeline.audit_images.get(output_index).cloned(),
        ) && let Ok(mut audit_target) = camera_targets.get_mut(audit_camera)
        {
            *audit_target = RenderTarget::Image(audit_image.into());
        }
        if let (Some(mask_camera), Some(mask_image)) = (
            pipeline.overlay_mask_camera,
            pipeline.overlay_mask_images.get(output_index).cloned(),
        ) && let Ok(mut mask_target) = camera_targets.get_mut(mask_camera)
        {
            *mask_target = RenderTarget::Image(mask_image.into());
        }

        let warming_up = throttle.warmup_frames_remaining > 0;
        if warming_up {
            throttle.warmup_frames_remaining -= 1;
        }

        let display_index = if warming_up {
            None
        } else {
            throttle.queued_output_indices.push_back(output_index);
            if throttle.queued_output_indices.len() >= throttle.batch_size {
                if !throttle.delay_filled {
                    throttle.delay_filled = true;
                    for mut visibility in &mut preview_visuals {
                        *visibility = Visibility::Inherited;
                    }
                    for mut visibility in &mut loading_text {
                        *visibility = Visibility::Hidden;
                    }
                }
                throttle.queued_output_indices.pop_front()
            } else {
                None
            }
        };
        if let Some(display_index) = display_index
            && let Some(mut display_material) =
                display_materials.get_mut(&throttle.display_material)
        {
            display_material.source_image = pipeline.output_images[display_index].clone();
            if let Some(mut raw_display_material) =
                raw_display_materials.get_mut(&throttle.raw_display_material)
            {
                raw_display_material.source_image = pipeline.source_images[display_index].clone();
            }
            if let Some(debug_state) = debug_state.as_deref_mut() {
                debug_state.raw_image = pipeline.source_images[display_index].clone();
                debug_state.quantized_image = pipeline.output_images[display_index].clone();
                if let Some(audit_image) = pipeline.audit_images.get(display_index) {
                    debug_state.audit_image = audit_image.clone();
                }
                if let Some(mask_image) = pipeline.overlay_mask_images.get(display_index) {
                    debug_state.overlay_mask_image = mask_image.clone();
                }
                debug_state.display_generation = debug_state.display_generation.wrapping_add(1);
            }
        }

        pipeline.current_output_index =
            (pipeline.current_output_index + 1) % pipeline.output_images.len();
    }

    for entity in [
        pipeline.source_copy_camera,
        pipeline.palette_camera,
        pipeline.raw_snapshot_camera,
    ] {
        if let Ok(mut camera) = cameras.get_mut(entity) {
            camera.is_active = should_render;
        }
    }
    if let Some(audit_camera) = pipeline.audit_camera
        && let Ok(mut camera) = cameras.get_mut(audit_camera)
    {
        camera.is_active = should_render;
    }
    if let Some(mask_camera) = pipeline.overlay_mask_camera
        && let Ok(mut camera) = cameras.get_mut(mask_camera)
    {
        camera.is_active = should_render && pipeline.overlay_enabled;
    }
    if let Ok(mut camera) = cameras.get_mut(pipeline.raw_overlay_camera) {
        camera.is_active = should_render && pipeline.overlay_enabled;
    }
    if let Ok(mut camera) = cameras.get_mut(pipeline.overlay_camera) {
        camera.is_active = should_render && pipeline.overlay_enabled;
    }
}

pub(crate) fn cycle_camera_render_targets(
    time: Res<Time>,
    stream_control: Res<StreamControl>,
    pipeline: Option<ResMut<GpuPalettePipeline>>,
    mut camera_targets: Query<&mut RenderTarget>,
    readback: Option<ResMut<StreamReadback>>,
    stats: Res<crate::stats::SharedStats>,
    mut commands: Commands,
) {
    let (Some(mut pipeline), Some(mut readback)) = (pipeline, readback) else {
        return;
    };

    if pipeline.output_images.is_empty() {
        return;
    }

    if !stream_control.is_streaming() {
        readback.pending_requests.clear();
        readback.batch_started_at = None;
        readback.batch_in_progress = false;
        readback.frame_due = false;
        readback.frame_accumulator = Duration::ZERO;
        readback.textures_rendered_in_batch = 0;
        readback.frame_waiting_for_render = None;
        readback.rendered_batch_frames.clear();
        readback.render_settle_frames_remaining = INITIAL_RENDER_SETTLE_FRAMES;
        return;
    }

    stats.with_mut(|stats| stats.record_custom_app_frame());
    readback.frame_accumulator += time.delta();
    if readback.frame_accumulator >= readback.frame_interval {
        readback.frame_due = true;
    }

    if !readback.frame_due {
        update_readback_diagnostics(&stats, &readback);
        return;
    }

    if let Some(rendered_frame) = readback.frame_waiting_for_render.take() {
        if readback.render_settle_frames_remaining > 0 {
            readback.render_settle_frames_remaining -= 1;
        } else {
            readback.textures_rendered_in_batch += 1;
            readback.rendered_batch_frames.push(rendered_frame);
        }
    }

    if readback.textures_rendered_in_batch >= readback.batch_size {
        readback.textures_rendered_in_batch = 0;
        readback.batch_in_progress = true;
        readback.batch_started_at.get_or_insert_with(Instant::now);

        let mut batch_frames = readback
            .rendered_batch_frames
            .drain(..)
            .collect::<Vec<_>>()
            .into_iter();
        while let Some(batch_frame) = batch_frames.next() {
            let current_image = readback.images[batch_frame.output_index].clone();
            let Some(readback_entity) = next_available_readback_entity(&mut readback) else {
                readback.rendered_batch_frames.push(batch_frame);
                readback.rendered_batch_frames.extend(batch_frames);
                break;
            };
            commands
                .entity(readback_entity)
                .insert(Readback::texture(current_image));
            readback.pending_requests.insert(
                readback_entity,
                PendingReadback {
                    captured_at: batch_frame.captured_at,
                    output_index: batch_frame.output_index,
                },
            );
        }
    }

    let current_output_index = pipeline.current_output_index;
    if readback
        .pending_requests
        .values()
        .any(|pending| pending.output_index == current_output_index)
    {
        update_readback_diagnostics(&stats, &readback);
        return;
    }

    let current_texture = pipeline.output_images[current_output_index].clone();

    if let Ok(mut palette_target) = camera_targets.get_mut(pipeline.palette_camera) {
        *palette_target = RenderTarget::Image(current_texture.clone().into());
    }

    if let Ok(mut overlay_target) = camera_targets.get_mut(pipeline.overlay_camera) {
        *overlay_target = RenderTarget::Image(current_texture.clone().into());
    }

    pipeline.current_output_index =
        (pipeline.current_output_index + 1) % pipeline.output_images.len();
    readback.frame_due = false;
    readback.frame_accumulator = readback
        .frame_accumulator
        .saturating_sub(readback.frame_interval);
    let captured_at = Instant::now();
    readback.frame_waiting_for_render = Some(RenderedBatchFrame {
        output_index: current_output_index,
        captured_at,
    });

    update_readback_diagnostics(&stats, &readback);
}

fn update_readback_diagnostics(stats: &crate::stats::SharedStats, readback: &StreamReadback) {
    stats.with_mut(|stats| {
        stats.custom_pending_readbacks = readback.pending_requests.len();
        stats.custom_batch_buffered_frames = readback.rendered_batch_frames.len()
            + usize::from(readback.frame_waiting_for_render.is_some());
    });
}

fn next_available_readback_entity(readback: &mut StreamReadback) -> Option<Entity> {
    if readback.readback_entities.is_empty() {
        return None;
    }

    for _ in 0..readback.readback_entities.len() {
        let index = readback.next_readback_entity % readback.readback_entities.len();
        readback.next_readback_entity = (index + 1) % readback.readback_entities.len();
        let entity = readback.readback_entities[index];
        if !readback.pending_requests.contains_key(&entity) {
            return Some(entity);
        }
    }

    None
}

fn output_image_count(batch_size: usize) -> usize {
    batch_size.max(1) * 2
}

fn palette_material_params(bias: &crate::palette::PaletteBias, palette_count: usize) -> Vec4 {
    Vec4::new(
        bias.lightness,
        bias.chroma,
        bias.hue,
        palette_count.max(1) as f32,
    )
}

fn palette_lookup_params() -> Vec4 {
    Vec4::new(1.0, 0.0, 0.0, 0.0)
}

fn palette_input_offset_a(bias: &crate::palette::PaletteBias) -> Vec4 {
    Vec4::new(
        bias.lightness_multiply,
        bias.lightness_add,
        bias.chroma_multiply,
        bias.chroma_add,
    )
}

fn palette_input_offset_b(bias: &crate::palette::PaletteBias) -> Vec4 {
    Vec4::new(bias.hue_add, 0.0, 0.0, 0.0)
}

#[cfg(test)]
mod tests {
    use super::indexed_unorm_byte;

    #[test]
    fn indexed_bytes_encode_inside_their_unorm_bins() {
        for byte in 0..=u8::MAX {
            let encoded = indexed_unorm_byte(byte);
            let srgb = if encoded <= 0.003_130_8 {
                encoded * 12.92
            } else {
                1.055 * encoded.powf(1.0 / 2.4) - 0.055
            };
            let decoded = (srgb * 255.0).round().clamp(0.0, 255.0) as u8;
            assert_eq!(decoded, byte, "indexed byte {byte} did not round-trip");
        }
    }
}
