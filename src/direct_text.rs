//! DirectText overlay rendering.
//!
//! Text is converted into stream-pixel glyph quads and routed through the direct
//! IPSMAP lookup path so UI colors are stable after scene palette biasing.

use crate::{
    gpu_palette::{GpuPalettePipeline, INDEXED_DIRECT_OVERLAY_MARKER, indexed_unorm_byte},
    palette_lut::LUT_ENTRY_COUNT,
    public_types::{DirectColorLookup, DirectStreamTarget},
};
use bevy::{camera::visibility::RenderLayers, prelude::*};
use std::collections::{HashMap, HashSet};

pub struct DirectTextPlugin;

const BITMAP_FONT_WIDTH: usize = 3;
const BITMAP_FONT_HEIGHT: usize = 5;
const BITMAP_FONT_ADVANCE: f32 = 4.0;
const BITMAP_FONT_LINE_HEIGHT: f32 = 6.0;
const DEFAULT_DIRECT_TEXT_FONT_SIZE: f32 = BITMAP_FONT_HEIGHT as f32;
const GLYPH_ON_THRESHOLD: f32 = 0.5;

impl Plugin for DirectTextPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DirectTextOverlayCache>()
            .add_systems(Update, sync_direct_text_overlays);
    }
}

#[derive(Component, Clone)]
pub struct DirectText {
    pub text: String,
    pub x: u32,
    pub y: u32,
    pub font_size: f32,
    pub threshold: Option<f32>,
    pub color: Srgba,
    pub color_lookup: DirectColorLookup,
    pub palette_index: Option<u8>,
}

impl DirectText {
    pub fn new(text: impl Into<String>, x: u32, y: u32) -> Self {
        Self {
            text: text.into(),
            x,
            y,
            font_size: DEFAULT_DIRECT_TEXT_FONT_SIZE,
            threshold: None,
            color: Srgba::WHITE,
            color_lookup: DirectColorLookup::Direct,
            palette_index: None,
        }
    }

    pub fn with_scale(mut self, scale: u32) -> Self {
        self.font_size = DEFAULT_DIRECT_TEXT_FONT_SIZE * scale.max(1) as f32;
        self
    }

    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = Some(threshold.clamp(0.0, 1.0));
        self
    }

    pub fn with_color(mut self, color: Srgba) -> Self {
        self.color = color;
        self
    }

    pub fn with_color_lookup(mut self, color_lookup: DirectColorLookup) -> Self {
        self.color_lookup = color_lookup;
        self
    }

    pub fn with_palette_index(mut self, palette_index: u8) -> Self {
        self.palette_index = Some(palette_index);
        self
    }

    pub fn without_palette_index(mut self) -> Self {
        self.palette_index = None;
        self
    }
}

#[derive(Component, Clone, Copy)]
struct DirectTextOverlayPixel {
    owner: Entity,
}

#[derive(Clone, PartialEq)]
struct DirectTextLayoutKey {
    text: String,
    x: u32,
    y: u32,
    font_size: f32,
    threshold: Option<f32>,
    color_lookup: DirectColorLookup,
}

impl From<&DirectText> for DirectTextLayoutKey {
    fn from(text: &DirectText) -> Self {
        Self {
            text: text.text.clone(),
            x: text.x,
            y: text.y,
            font_size: text.font_size,
            threshold: text.threshold,
            color_lookup: text.color_lookup,
        }
    }
}

#[derive(Clone)]
struct DirectTextOverlayState {
    layout: DirectTextLayoutKey,
    color: Srgba,
    palette_index: Option<u8>,
}

#[derive(Resource, Default)]
struct DirectTextOverlayCache {
    entries: HashMap<Entity, DirectTextOverlayState>,
}

fn sync_direct_text_overlays(
    mut commands: Commands,
    target: Res<DirectStreamTarget>,
    gpu_palette: Option<Res<GpuPalettePipeline>>,
    changed_text: Query<(Entity, &DirectText), Or<(Added<DirectText>, Changed<DirectText>)>>,
    all_text: Query<(Entity, &DirectText)>,
    mut existing: Query<(Entity, &DirectTextOverlayPixel, &mut Sprite)>,
    mut removed_text: RemovedComponents<DirectText>,
    mut cache: ResMut<DirectTextOverlayCache>,
) {
    let removed_owners: HashSet<Entity> = removed_text.read().collect();
    let target_changed = target.is_changed();
    if !target_changed && changed_text.is_empty() && removed_owners.is_empty() {
        return;
    }

    let mut rebuild_owners: HashSet<Entity> = if target_changed {
        all_text.iter().map(|(entity, _)| entity).collect()
    } else {
        HashSet::default()
    };
    let mut recolor_owners: HashSet<Entity> = HashSet::default();

    if target_changed {
        cache.entries.clear();
    } else {
        for (owner, text) in &changed_text {
            let layout = DirectTextLayoutKey::from(text);
            let Some(previous) = cache.entries.get(&owner) else {
                rebuild_owners.insert(owner);
                continue;
            };

            if previous.layout != layout {
                rebuild_owners.insert(owner);
            } else if direct_text_color_changed(previous.color, text.color)
                || previous.palette_index != text.palette_index
            {
                recolor_owners.insert(owner);
            }
        }
    }

    for (entity, overlay, _) in &mut existing {
        if removed_owners.contains(&overlay.owner) || rebuild_owners.contains(&overlay.owner) {
            commands.entity(entity).despawn();
        }
    }

    for owner in &removed_owners {
        cache.entries.remove(owner);
    }

    if !recolor_owners.is_empty() {
        let recolors: HashMap<Entity, Color> = all_text
            .iter()
            .filter(|(owner, _)| recolor_owners.contains(owner))
            .map(|(owner, text)| {
                (
                    owner,
                    final_overlay_color(text, &target, gpu_palette.as_deref()),
                )
            })
            .collect();
        for (_, overlay, mut sprite) in &mut existing {
            if let Some(color) = recolors.get(&overlay.owner) {
                sprite.color = *color;
            }
        }
        for (owner, text) in &changed_text {
            if recolor_owners.contains(&owner)
                && let Some(entry) = cache.entries.get_mut(&owner)
            {
                entry.color = text.color;
                entry.palette_index = text.palette_index;
            }
        }
    }

    if rebuild_owners.is_empty() {
        return;
    }

    let overlay_layer = RenderLayers::layer(target.overlay_layer);
    let left = -(target.width as f32) * 0.5;
    let top = target.height as f32 * 0.5;

    for (owner, text) in &all_text {
        if !rebuild_owners.contains(&owner) {
            continue;
        }
        let scale = resolve_bitmap_scale(text.font_size);
        let threshold = text.threshold.unwrap_or(GLYPH_ON_THRESHOLD).clamp(0.0, 1.0);
        let color = final_overlay_color(text, &target, gpu_palette.as_deref());
        let mut cursor_x = text.x as f32;
        let mut cursor_y = text.y as f32;
        let start_x = cursor_x;
        let advance = BITMAP_FONT_ADVANCE * scale;
        let line_height = BITMAP_FONT_LINE_HEIGHT * scale;

        for character in text.text.chars() {
            match character {
                '\n' => {
                    cursor_x = start_x;
                    cursor_y += line_height;
                }
                '\r' => {}
                _ => {
                    let columns = glyph_columns(character);
                    spawn_bitmap_glyph(
                        &mut commands,
                        &overlay_layer,
                        owner,
                        color,
                        left,
                        top,
                        cursor_x,
                        cursor_y,
                        scale,
                        threshold,
                        columns,
                    );
                    cursor_x += advance;
                }
            }
        }

        cache.entries.insert(
            owner,
            DirectTextOverlayState {
                layout: DirectTextLayoutKey::from(text),
                color: text.color,
                palette_index: text.palette_index,
            },
        );
    }
}

fn direct_text_color_changed(current: Srgba, next: Srgba) -> bool {
    (current.red - next.red).abs() > f32::EPSILON
        || (current.green - next.green).abs() > f32::EPSILON
        || (current.blue - next.blue).abs() > f32::EPSILON
        || (current.alpha - next.alpha).abs() > f32::EPSILON
}

fn resolve_bitmap_scale(desired_pixel_height: f32) -> f32 {
    quantize_bitmap_scale(desired_pixel_height / BITMAP_FONT_HEIGHT as f32)
}

fn quantize_bitmap_scale(scale: f32) -> f32 {
    scale.round().max(1.0)
}

fn spawn_bitmap_glyph(
    commands: &mut Commands,
    overlay_layer: &RenderLayers,
    owner: Entity,
    color: Color,
    left: f32,
    top: f32,
    x: f32,
    y: f32,
    scale: f32,
    threshold: f32,
    glyph_columns: [u8; BITMAP_FONT_WIDTH],
) {
    if threshold > 1.0 {
        return;
    }

    for column in 0..BITMAP_FONT_WIDTH {
        for row in 0..BITMAP_FONT_HEIGHT {
            if glyph_bit_is_on(glyph_columns, column, row) {
                let pixel_x = left + x + column as f32 * scale + scale * 0.5;
                let pixel_y = top - y - row as f32 * scale - scale * 0.5;
                commands.spawn((
                    Sprite {
                        color,
                        custom_size: Some(Vec2::splat(scale)),
                        ..default()
                    },
                    Transform::from_xyz(pixel_x, pixel_y, 0.0),
                    overlay_layer.clone(),
                    DirectTextOverlayPixel { owner },
                ));
            }
        }
    }
}

fn indexed_overlay_color(
    text: &DirectText,
    target: &DirectStreamTarget,
    gpu_palette: Option<&GpuPalettePipeline>,
) -> Color {
    if target.output_is_indexed {
        let palette_index = text.palette_index.or_else(|| {
            gpu_palette.and_then(|gpu_palette| {
                lookup_palette_index(text.color, &gpu_palette.lookup_entries, true)
            })
        });
        let palette_index = palette_index.unwrap_or(0);
        let index_value = indexed_unorm_byte(palette_index);
        return Color::linear_rgba(index_value, INDEXED_DIRECT_OVERLAY_MARKER, 0.0, 1.0);
    }

    raw_overlay_color(text)
}

fn raw_overlay_color(text: &DirectText) -> Color {
    Color::srgba(
        text.color.red,
        text.color.green,
        text.color.blue,
        text.color.alpha,
    )
}

fn final_overlay_color(
    text: &DirectText,
    target: &DirectStreamTarget,
    gpu_palette: Option<&GpuPalettePipeline>,
) -> Color {
    if target.output_is_indexed {
        return indexed_overlay_color(text, target, gpu_palette);
    }
    raw_overlay_color(text)
}

fn lookup_palette_index(color: Srgba, lookup_entries: &[u8], direct: bool) -> Option<u8> {
    let r = (color.red.clamp(0.0, 1.0) * 255.0).round() as usize;
    let g = (color.green.clamp(0.0, 1.0) * 255.0).round() as usize;
    let b = (color.blue.clamp(0.0, 1.0) * 255.0).round() as usize;
    let mut offset = (r << 16) | (g << 8) | b;
    if direct && lookup_entries.len() >= LUT_ENTRY_COUNT.saturating_mul(2) {
        offset += LUT_ENTRY_COUNT;
    }
    lookup_entries.get(offset).copied()
}

fn glyph_bit_is_on(glyph_columns: [u8; BITMAP_FONT_WIDTH], column: usize, row: usize) -> bool {
    ((glyph_columns[column] >> row) & 1) != 0
}

fn glyph_columns(character: char) -> [u8; BITMAP_FONT_WIDTH] {
    match character {
        ' ' => [0x00, 0x00, 0x00],
        '!' => [0x00, 0x17, 0x00],
        '"' => [0x03, 0x00, 0x03],
        '%' => [0x09, 0x04, 0x12],
        '\'' => [0x00, 0x03, 0x00],
        '(' => [0x0E, 0x11, 0x00],
        ')' => [0x00, 0x11, 0x0E],
        '*' => [0x05, 0x02, 0x05],
        '+' => [0x04, 0x0E, 0x04],
        ',' => [0x00, 0x10, 0x08],
        '-' => [0x04, 0x04, 0x04],
        '.' => [0x00, 0x10, 0x00],
        '/' => [0x18, 0x0E, 0x03],
        '0' => [0x1F, 0x11, 0x1F],
        '1' => [0x12, 0x1F, 0x10],
        '2' => [0x1D, 0x15, 0x17],
        '3' => [0x11, 0x15, 0x1F],
        '4' => [0x07, 0x04, 0x1F],
        '5' => [0x17, 0x15, 0x1D],
        '6' => [0x1F, 0x15, 0x1D],
        '7' => [0x01, 0x1D, 0x03],
        '8' => [0x1F, 0x15, 0x1F],
        '9' => [0x17, 0x15, 0x1F],
        ':' => [0x00, 0x0A, 0x00],
        ';' => [0x10, 0x0A, 0x00],
        '<' => [0x04, 0x0A, 0x11],
        '=' => [0x0A, 0x0A, 0x0A],
        '>' => [0x11, 0x0A, 0x04],
        '?' => [0x01, 0x15, 0x03],
        '@' => [0x0E, 0x11, 0x1D],
        'A' => [0x1E, 0x09, 0x1E],
        'B' => [0x1F, 0x15, 0x0A],
        'C' => [0x1F, 0x11, 0x11],
        'D' => [0x1F, 0x11, 0x0E],
        'E' => [0x1F, 0x15, 0x11],
        'F' => [0x1F, 0x05, 0x01],
        'G' => [0x0E, 0x11, 0x0D],
        'H' => [0x1F, 0x04, 0x1F],
        'I' => [0x11, 0x1F, 0x11],
        'J' => [0x09, 0x11, 0x0F],
        'K' => [0x1F, 0x04, 0x1B],
        'L' => [0x1F, 0x10, 0x10],
        'M' => [0x1F, 0x06, 0x1F],
        'N' => [0x1F, 0x0E, 0x1F],
        'O' => [0x0E, 0x11, 0x0E],
        'P' => [0x1F, 0x09, 0x06],
        'Q' => [0x0E, 0x11, 0x1E],
        'R' => [0x1F, 0x09, 0x16],
        'S' => [0x12, 0x15, 0x09],
        'T' => [0x01, 0x1F, 0x01],
        'U' => [0x1F, 0x10, 0x1F],
        'V' => [0x0F, 0x18, 0x0F],
        'W' => [0x1F, 0x0C, 0x1F],
        'X' => [0x1B, 0x04, 0x1B],
        'Y' => [0x03, 0x1C, 0x03],
        'Z' => [0x19, 0x15, 0x13],
        '[' => [0x00, 0x00, 0x1F],
        '\\' => [0x01, 0x02, 0x04],
        ']' => [0x00, 0x11, 0x1F],
        '^' => [0x00, 0x02, 0x01],
        '_' => [0x00, 0x10, 0x10],
        '`' => [0x00, 0x01, 0x02],
        'a' => [0x0C, 0x12, 0x1C],
        'b' => [0x1F, 0x14, 0x08],
        'c' => [0x0C, 0x12, 0x12],
        'd' => [0x08, 0x14, 0x1F],
        'e' => [0x0C, 0x12, 0x16],
        'f' => [0x1E, 0x05, 0x01],
        'g' => [0x12, 0x15, 0x0E],
        'h' => [0x1F, 0x04, 0x18],
        'i' => [0x00, 0x1D, 0x00],
        'j' => [0x08, 0x10, 0x0D],
        'k' => [0x1F, 0x08, 0x14],
        'l' => [0x0F, 0x10, 0x00],
        'm' => [0x1E, 0x04, 0x1E],
        'n' => [0x1E, 0x02, 0x1C],
        'o' => [0x0C, 0x12, 0x0C],
        'p' => [0x1E, 0x0A, 0x04],
        'q' => [0x04, 0x0A, 0x1E],
        'r' => [0x1C, 0x02, 0x02],
        's' => [0x10, 0x14, 0x0A],
        't' => [0x04, 0x0E, 0x14],
        'u' => [0x1E, 0x10, 0x1E],
        'v' => [0x0E, 0x10, 0x0E],
        'w' => [0x1E, 0x08, 0x1E],
        'x' => [0x12, 0x0C, 0x12],
        'y' => [0x12, 0x14, 0x0E],
        'z' => [0x12, 0x1A, 0x16],
        '{' => [0x04, 0x1B, 0x11],
        '|' => [0x00, 0x1F, 0x00],
        '}' => [0x11, 0x1B, 0x04],
        '~' => [0x0C, 0x04, 0x06],
        _ => [0x01, 0x15, 0x03],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_lookup_uses_ipsmap_direct_entries() {
        let color = Srgba::new(1.0, 0.0, 0.0, 1.0);
        let lookup_key = 255usize << 16;
        let mut entries = vec![0u8; LUT_ENTRY_COUNT * 2];
        entries[lookup_key] = 7;
        entries[lookup_key + LUT_ENTRY_COUNT] = 42;

        assert_eq!(lookup_palette_index(color, &entries, false), Some(7));
        assert_eq!(lookup_palette_index(color, &entries, true), Some(42));
    }

    #[test]
    fn explicit_palette_index_overrides_direct_lookup_for_indexed_text() {
        let target = DirectStreamTarget {
            camera: Entity::PLACEHOLDER,
            overlay_camera: Entity::PLACEHOLDER,
            image: Handle::default(),
            output_image: Handle::default(),
            output_is_indexed: true,
            overlay_layer: 0,
            raw_overlay_layer: Some(1),
            width: 16,
            height: 16,
            fps: 30,
        };
        let mut entries = vec![0u8; LUT_ENTRY_COUNT * 2];
        entries[LUT_ENTRY_COUNT + (255usize << 16)] = 42;
        let gpu_palette = GpuPalettePipeline {
            material: Handle::default(),
            source_copy_material: Handle::default(),
            raw_snapshot_material: Handle::default(),
            palette_texture: Handle::default(),
            lookup_texture: Handle::default(),
            capture_image: Handle::default(),
            source_copy_camera: Entity::PLACEHOLDER,
            palette_camera: Entity::PLACEHOLDER,
            raw_overlay_camera: Entity::PLACEHOLDER,
            overlay_camera: Entity::PLACEHOLDER,
            raw_snapshot_camera: Entity::PLACEHOLDER,
            audit_camera: None,
            overlay_mask_camera: None,
            source_copy_quad_entity: Entity::PLACEHOLDER,
            quad_entity: Entity::PLACEHOLDER,
            raw_snapshot_quad_entity: Entity::PLACEHOLDER,
            audit_quad_entity: None,
            source_images: Vec::new(),
            output_images: Vec::new(),
            audit_images: Vec::new(),
            overlay_mask_images: Vec::new(),
            current_output_index: 0,
            palette_count: 0,
            palette_colors: Vec::new(),
            lookup_entries: std::sync::Arc::from(entries.into_boxed_slice()),
            overlay_enabled: true,
        };
        let text = DirectText::new("!", 0, 0)
            .with_color(Srgba::RED)
            .with_palette_index(9);

        let color = indexed_overlay_color(&text, &target, Some(&gpu_palette)).to_linear();
        assert_eq!(color.red, indexed_unorm_byte(9));
        assert_eq!(color.green, INDEXED_DIRECT_OVERLAY_MARKER);
        assert_eq!(color.alpha, 1.0);
    }

    #[test]
    fn explicit_palette_index_uses_final_index_overlay_even_with_raw_layer() {
        let target = DirectStreamTarget {
            camera: Entity::PLACEHOLDER,
            overlay_camera: Entity::PLACEHOLDER,
            image: Handle::default(),
            output_image: Handle::default(),
            output_is_indexed: true,
            overlay_layer: 0,
            raw_overlay_layer: Some(1),
            width: 16,
            height: 16,
            fps: 30,
        };
        let text = DirectText::new("!", 0, 0)
            .with_color(Srgba::RED)
            .with_palette_index(37);

        let color = final_overlay_color(&text, &target, None).to_linear();
        assert_eq!(color.red, indexed_unorm_byte(37));
        assert_eq!(color.green, INDEXED_DIRECT_OVERLAY_MARKER);
        assert!((color.alpha - 1.0).abs() < 0.0001);
    }
}
