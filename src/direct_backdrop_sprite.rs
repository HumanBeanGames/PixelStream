//! Stream-resolution backdrop sprites.
//!
//! Backdrops are drawn in stream pixel space before DirectText. They are useful
//! for star fields, sky layers, and other low-resolution assets that should not
//! be projected from world-space geometry.

use crate::public_types::{DirectColorLookup, DirectStreamTarget};
use bevy::{
    asset::{RenderAssetUsages, load_internal_asset, uuid_handle},
    camera::visibility::RenderLayers,
    light::NotShadowCaster,
    material::AlphaMode,
    math::Affine2,
    mesh::{Indices, MeshVertexBufferLayoutRef},
    pbr::{Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin},
    prelude::*,
    reflect::TypePath,
    render::render_resource::{
        AsBindGroup, ColorWrites, Face, PrimitiveTopology, RenderPipelineDescriptor,
        SpecializedMeshPipelineError,
    },
    shader::{Shader, ShaderRef},
    transform::TransformSystems,
};
use std::collections::{HashMap, HashSet};

pub struct DirectBackdropSpritePlugin;

const DIRECT_BACKDROP_MARKER_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("5706e440-f61e-46db-92b2-f5979f4d48e1");
const DIRECT_LOOKUP_ALPHA: f32 = (254.0 + 0.25) / 255.0;
const MARKER_DEPTH_OFFSET: f32 = 0.01;

impl Plugin for DirectBackdropSpritePlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(
            app,
            DIRECT_BACKDROP_MARKER_SHADER_HANDLE,
            "../assets/shaders/direct_backdrop_marker.wgsl",
            Shader::from_wgsl
        );
        app.add_plugins(MaterialPlugin::<DirectBackdropMarkerMaterial>::default())
            .init_resource::<DirectBackdropSpriteSettings>()
            .add_systems(
                PostUpdate,
                sync_direct_backdrop_sprites.after(TransformSystems::Propagate),
            );
    }
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
struct DirectBackdropMarkerMaterial {
    #[uniform(0)]
    uv_scale_offset: Vec4,
    #[texture(1)]
    #[sampler(2)]
    image: Handle<Image>,
    #[uniform(3)]
    alpha_params: Vec4,
}

impl Material for DirectBackdropMarkerMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(DIRECT_BACKDROP_MARKER_SHADER_HANDLE)
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            for target in fragment.targets.iter_mut().flatten() {
                target.blend = None;
                target.write_mask = ColorWrites::ALPHA;
            }
        }
        Ok(())
    }
}

#[derive(Component, Clone)]
pub struct DirectBackdropSprite {
    pub image: Handle<Image>,
    pub pixel_rect: Option<URect>,
    pub uv_offset: Vec2,
    pub uv_scale: Vec2,
    pub tint: Color,
    pub color_lookup: DirectColorLookup,
    pub alpha_cutoff: Option<f32>,
    pub layer: DirectBackdropLayer,
    pub depth: f32,
}

impl DirectBackdropSprite {
    pub fn new(image: Handle<Image>) -> Self {
        Self {
            image,
            pixel_rect: None,
            uv_offset: Vec2::ZERO,
            uv_scale: Vec2::ONE,
            tint: Color::WHITE,
            color_lookup: DirectColorLookup::Altered,
            alpha_cutoff: None,
            layer: DirectBackdropLayer::BehindWorld,
            depth: 512.0,
        }
    }

    pub fn with_pixel_rect(mut self, pixel_rect: URect) -> Self {
        self.pixel_rect = Some(pixel_rect);
        self
    }

    pub fn with_uv_offset(mut self, uv_offset: Vec2) -> Self {
        self.uv_offset = uv_offset;
        self
    }

    pub fn with_uv_scale(mut self, uv_scale: Vec2) -> Self {
        self.uv_scale = uv_scale;
        self
    }

    pub fn with_tint(mut self, tint: Color) -> Self {
        self.tint = tint;
        self
    }

    pub fn with_color_lookup(mut self, color_lookup: DirectColorLookup) -> Self {
        self.color_lookup = color_lookup;
        self
    }

    pub fn with_alpha_cutoff(mut self, alpha_cutoff: f32) -> Self {
        self.alpha_cutoff = Some(alpha_cutoff.clamp(0.0, 1.0));
        self
    }

    pub fn with_layer(mut self, layer: DirectBackdropLayer) -> Self {
        self.layer = layer;
        self
    }

    pub fn with_depth(mut self, depth: f32) -> Self {
        self.depth = depth;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectBackdropLayer {
    BehindWorld,
    BeforeWorldSprites,
    BeforeText,
}

#[derive(Resource, Clone)]
pub struct DirectBackdropSpriteSettings {
    pub enabled: bool,
    pub max_sprites: usize,
}

impl Default for DirectBackdropSpriteSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_sprites: 16,
        }
    }
}

#[derive(Component)]
struct DirectBackdropSpriteRender {
    material: Handle<StandardMaterial>,
    marker_material: Handle<DirectBackdropMarkerMaterial>,
    mesh: Handle<Mesh>,
    image: Handle<Image>,
    pixel_rect: Option<URect>,
    color_lookup: DirectColorLookup,
    alpha_cutoff: Option<f32>,
    layer: DirectBackdropLayer,
    depth: f32,
    target_size: UVec2,
}

#[derive(Clone, Copy)]
struct DirectBackdropRenderEntities {
    color: Entity,
    marker: Entity,
}

#[derive(Default)]
struct DirectBackdropSpriteRenderMap(HashMap<Entity, DirectBackdropRenderEntities>);

fn sync_direct_backdrop_sprites(
    mut commands: Commands,
    settings: Res<DirectBackdropSpriteSettings>,
    target: Res<DirectStreamTarget>,
    mut queries: ParamSet<(
        Query<(&Camera, Ref<GlobalTransform>, Option<&RenderLayers>)>,
        Query<(Entity, &DirectBackdropSprite), Without<DirectBackdropSpriteRender>>,
        Query<(
            &mut DirectBackdropSpriteRender,
            &mut Mesh3d,
            &mut MeshMaterial3d<StandardMaterial>,
            &mut Visibility,
        )>,
    )>,
    mut render_map: Local<DirectBackdropSpriteRenderMap>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut marker_materials: ResMut<Assets<DirectBackdropMarkerMaterial>>,
) {
    let target_changed = target.is_changed();
    let (camera, camera_transform, camera_changed, camera_layers) = {
        let camera_query = queries.p0();
        let Ok((camera, camera_transform, camera_layers)) = camera_query.get(target.camera) else {
            return;
        };
        (
            camera.clone(),
            *camera_transform,
            camera_transform.is_changed(),
            camera_layers.cloned(),
        )
    };

    let source_sprites = queries
        .p1()
        .iter()
        .map(|(entity, sprite)| (entity, sprite.clone()))
        .collect::<Vec<_>>();
    let active_owners = source_sprites
        .iter()
        .map(|(entity, _)| *entity)
        .collect::<HashSet<_>>();

    render_map.0.retain(|owner, render_entities| {
        let keep = active_owners.contains(owner);
        if !keep {
            commands.entity(render_entities.color).despawn();
            commands.entity(render_entities.marker).despawn();
        }
        keep
    });

    if !settings.enabled {
        for render_entities in render_map.0.values().copied() {
            if let Ok((_, _, _, mut visibility)) = queries.p2().get_mut(render_entities.color) {
                *visibility = Visibility::Hidden;
            }
            commands
                .entity(render_entities.marker)
                .insert(Visibility::Hidden);
        }
        return;
    }

    let render_layers = camera_layers.unwrap_or_else(|| RenderLayers::layer(0));
    let mut visible_count = 0usize;
    for (owner, sprite) in &source_sprites {
        if visible_count >= settings.max_sprites {
            if let Some(render_entities) = render_map.0.get(owner).copied()
                && let Ok((_, _, _, mut visibility)) = queries.p2().get_mut(render_entities.color)
            {
                *visibility = Visibility::Hidden;
                commands
                    .entity(render_entities.marker)
                    .insert(Visibility::Hidden);
            }
            continue;
        }

        visible_count += 1;
        let render_entities = if let Some(render_entities) = render_map.0.get(owner).copied() {
            render_entities
        } else {
            let Some(render_mesh) = backdrop_mesh(sprite, &camera, &camera_transform, &target)
            else {
                continue;
            };
            let mut marker_sprite = sprite.clone();
            marker_sprite.depth = (sprite.depth - MARKER_DEPTH_OFFSET).max(0.001);
            let Some(marker_mesh) =
                backdrop_mesh(&marker_sprite, &camera, &camera_transform, &target)
            else {
                continue;
            };
            let mesh = meshes.add(render_mesh);
            let marker_mesh = meshes.add(marker_mesh);
            let material = materials.add(backdrop_material(sprite));
            let marker_material = marker_materials.add(backdrop_marker_material(sprite));
            let marker_entity = commands
                .spawn((
                    Mesh3d(marker_mesh),
                    MeshMaterial3d(marker_material.clone()),
                    Transform::default(),
                    direct_marker_visibility(sprite.color_lookup),
                    render_layers.clone(),
                    NotShadowCaster,
                ))
                .id();
            let color_entity = commands
                .spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(material.clone()),
                    Transform::default(),
                    Visibility::Visible,
                    render_layers.clone(),
                    NotShadowCaster,
                    DirectBackdropSpriteRender {
                        material,
                        marker_material,
                        mesh,
                        image: sprite.image.clone(),
                        pixel_rect: sprite.pixel_rect,
                        color_lookup: sprite.color_lookup,
                        alpha_cutoff: sprite.alpha_cutoff,
                        layer: sprite.layer,
                        depth: sprite.depth,
                        target_size: UVec2::new(target.width, target.height),
                    },
                ))
                .id();
            render_map.0.insert(
                *owner,
                DirectBackdropRenderEntities {
                    color: color_entity,
                    marker: marker_entity,
                },
            );
            continue;
        };

        {
            let mut render_query = queries.p2();
            let Ok((mut render, mut mesh, mut material, mut visibility)) =
                render_query.get_mut(render_entities.color)
            else {
                render_map.0.remove(owner);
                continue;
            };

            let geometry_changed = camera_changed
                || target_changed
                || render.pixel_rect != sprite.pixel_rect
                || render.depth != sprite.depth
                || render.target_size != UVec2::new(target.width, target.height);

            if geometry_changed
                && let Some(next_mesh) = backdrop_mesh(sprite, &camera, &camera_transform, &target)
            {
                let next_mesh = meshes.add(next_mesh);
                mesh.0 = next_mesh.clone();
                render.mesh = next_mesh;
                let mut marker_sprite = sprite.clone();
                marker_sprite.depth = (sprite.depth - MARKER_DEPTH_OFFSET).max(0.001);
                if let Some(next_marker_mesh) =
                    backdrop_mesh(&marker_sprite, &camera, &camera_transform, &target)
                {
                    commands
                        .entity(render_entities.marker)
                        .insert(Mesh3d(meshes.add(next_marker_mesh)));
                }
                render.pixel_rect = sprite.pixel_rect;
                render.depth = sprite.depth;
                render.target_size = UVec2::new(target.width, target.height);
            }

            if render.image != sprite.image
                || render.pixel_rect != sprite.pixel_rect
                || render.color_lookup != sprite.color_lookup
                || render.alpha_cutoff != sprite.alpha_cutoff
                || render.layer != sprite.layer
            {
                render.image = sprite.image.clone();
                render.pixel_rect = sprite.pixel_rect;
                render.color_lookup = sprite.color_lookup;
                render.alpha_cutoff = sprite.alpha_cutoff;
                render.layer = sprite.layer;
            }

            if render.material != material.0 {
                material.0 = render.material.clone();
            }

            if let Some(mut existing_material) = materials.get_mut(&render.material) {
                existing_material.base_color = sprite.tint;
                existing_material.base_color_texture = Some(sprite.image.clone());
                existing_material.depth_bias = backdrop_depth_bias(sprite.layer);
                existing_material.alpha_mode = backdrop_alpha_mode(sprite);
                existing_material.uv_transform =
                    Affine2::from_scale_angle_translation(sprite.uv_scale, 0.0, sprite.uv_offset);
            }
            if let Some(mut existing_marker_material) =
                marker_materials.get_mut(&render.marker_material)
            {
                *existing_marker_material = backdrop_marker_material(sprite);
            }

            *visibility = Visibility::Visible;
        }
        commands
            .entity(render_entities.color)
            .insert(render_layers.clone());
        commands.entity(render_entities.marker).insert((
            render_layers.clone(),
            direct_marker_visibility(sprite.color_lookup),
        ));
    }
}

fn backdrop_mesh(
    sprite: &DirectBackdropSprite,
    camera: &Camera,
    camera_transform: &GlobalTransform,
    target: &DirectStreamTarget,
) -> Option<Mesh> {
    let rect = stream_rect(sprite, target)?;
    let depth = sprite.depth.max(0.001);
    let forward = camera_transform.forward();
    let plane_origin = camera_transform.translation() + *forward * depth;
    let plane_normal = *forward;

    let top_left = intersect_viewport_ray_with_plane(
        camera,
        camera_transform,
        rect.min.as_vec2(),
        plane_origin,
        plane_normal,
    )?;
    let top_right = intersect_viewport_ray_with_plane(
        camera,
        camera_transform,
        Vec2::new(rect.max.x as f32, rect.min.y as f32),
        plane_origin,
        plane_normal,
    )?;
    let bottom_right = intersect_viewport_ray_with_plane(
        camera,
        camera_transform,
        rect.max.as_vec2(),
        plane_origin,
        plane_normal,
    )?;
    let bottom_left = intersect_viewport_ray_with_plane(
        camera,
        camera_transform,
        Vec2::new(rect.min.x as f32, rect.max.y as f32),
        plane_origin,
        plane_normal,
    )?;

    Some(
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![
                top_left.to_array(),
                top_right.to_array(),
                bottom_right.to_array(),
                bottom_left.to_array(),
            ],
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![(-plane_normal).to_array(); 4])
        .with_inserted_attribute(
            Mesh::ATTRIBUTE_UV_0,
            vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        )
        .with_inserted_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3])),
    )
}

fn stream_rect(sprite: &DirectBackdropSprite, target: &DirectStreamTarget) -> Option<URect> {
    let rect = sprite
        .pixel_rect
        .unwrap_or_else(|| URect::new(0, 0, target.width, target.height));
    if rect.min.x >= rect.max.x || rect.min.y >= rect.max.y {
        return None;
    }
    Some(URect::new(
        rect.min.x.min(target.width),
        rect.min.y.min(target.height),
        rect.max.x.min(target.width),
        rect.max.y.min(target.height),
    ))
}

fn intersect_viewport_ray_with_plane(
    camera: &Camera,
    camera_transform: &GlobalTransform,
    viewport: Vec2,
    plane_origin: Vec3,
    plane_normal: Vec3,
) -> Option<Vec3> {
    let ray = camera.viewport_to_world(camera_transform, viewport).ok()?;
    let normal = plane_normal.normalize_or_zero();
    let denom = (*ray.direction).dot(normal);
    if denom.abs() <= f32::EPSILON {
        return None;
    }
    let distance = (plane_origin - ray.origin).dot(normal) / denom;
    if !distance.is_finite() || distance <= 0.0 {
        return None;
    }
    Some(ray.get_point(distance))
}

fn backdrop_material(sprite: &DirectBackdropSprite) -> StandardMaterial {
    StandardMaterial {
        base_color: sprite.tint,
        base_color_texture: Some(sprite.image.clone()),
        unlit: true,
        double_sided: true,
        cull_mode: None::<Face>,
        alpha_mode: backdrop_alpha_mode(sprite),
        depth_bias: backdrop_depth_bias(sprite.layer),
        uv_transform: Affine2::from_scale_angle_translation(sprite.uv_scale, 0.0, sprite.uv_offset),
        ..default()
    }
}

fn backdrop_marker_material(sprite: &DirectBackdropSprite) -> DirectBackdropMarkerMaterial {
    DirectBackdropMarkerMaterial {
        uv_scale_offset: Vec4::new(
            sprite.uv_scale.x,
            sprite.uv_scale.y,
            sprite.uv_offset.x,
            sprite.uv_offset.y,
        ),
        image: sprite.image.clone(),
        alpha_params: Vec4::new(
            sprite.alpha_cutoff.unwrap_or(0.5 / 255.0),
            sprite.tint.alpha(),
            DIRECT_LOOKUP_ALPHA,
            0.0,
        ),
    }
}

fn direct_marker_visibility(color_lookup: DirectColorLookup) -> Visibility {
    match color_lookup {
        DirectColorLookup::Direct => Visibility::Visible,
        DirectColorLookup::Altered => Visibility::Hidden,
    }
}

fn backdrop_alpha_mode(sprite: &DirectBackdropSprite) -> AlphaMode {
    if let Some(alpha_cutoff) = sprite.alpha_cutoff {
        AlphaMode::Mask(alpha_cutoff)
    } else {
        AlphaMode::Blend
    }
}

fn backdrop_depth_bias(layer: DirectBackdropLayer) -> f32 {
    match layer {
        DirectBackdropLayer::BehindWorld => -10_000.0,
        DirectBackdropLayer::BeforeWorldSprites => 5_000.0,
        DirectBackdropLayer::BeforeText => 9_000.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blended_backdrops_default_to_the_altered_lookup_path() {
        let sprite = DirectBackdropSprite::new(Handle::default());

        assert_eq!(sprite.color_lookup, DirectColorLookup::Altered);
        assert!(matches!(
            direct_marker_visibility(sprite.color_lookup),
            Visibility::Hidden
        ));
    }

    #[test]
    fn direct_backdrops_enable_the_direct_lookup_marker() {
        let sprite = DirectBackdropSprite::new(Handle::default())
            .with_color_lookup(DirectColorLookup::Direct);

        assert!(matches!(
            direct_marker_visibility(sprite.color_lookup),
            Visibility::Visible
        ));
    }
}
