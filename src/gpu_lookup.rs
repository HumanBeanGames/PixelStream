//! GPU-assisted IPSMAP lookup baking.
//!
//! The full RGB cube is large enough that a compute pass is useful when a GPU is
//! available; CPU paths remain for tests and environments without the feature.

use crate::palette_lut::{LUT_ENTRY_COUNT, PaletteConfig};
use std::sync::mpsc;
use wgpu::util::DeviceExt;

const PACKED_LOOKUP_WORDS: usize = (LUT_ENTRY_COUNT * 2) / 4;
const WORKGROUP_SIZE: u32 = 64;
const WORKGROUPS_X: u32 = 1024;
const INVOCATIONS_PER_ROW: usize = (WORKGROUP_SIZE * WORKGROUPS_X) as usize;

pub(crate) fn build_lookup_gpu_with_progress(
    config: &PaletteConfig,
    mut progress: impl FnMut(usize),
) -> Result<Vec<u8>, String> {
    progress(1);
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .map_err(|error| format!("GPU palette adapter unavailable: {error}"))?;

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("direct_stream_palette_lookup_device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .map_err(|error| format!("GPU palette device unavailable: {error}"))?;
    progress(5);

    let palette_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("palette_lookup_config"),
        contents: &palette_config_bytes(config),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let output_size = (PACKED_LOOKUP_WORDS * std::mem::size_of::<u32>()) as u64;
    let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("palette_lookup_output"),
        size: output_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("palette_lookup_readback"),
        size: output_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("palette_lookup_compute_shader"),
        source: wgpu::ShaderSource::Wgsl(PALETTE_LOOKUP_COMPUTE_SHADER.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("palette_lookup_bind_group_layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("palette_lookup_bind_group"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: palette_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output_buffer.as_entire_binding(),
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("palette_lookup_pipeline_layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("palette_lookup_pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("main"),
        cache: None,
        compilation_options: wgpu::PipelineCompilationOptions::default(),
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("palette_lookup_encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("palette_lookup_pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        let rows = PACKED_LOOKUP_WORDS.div_ceil(INVOCATIONS_PER_ROW) as u32;
        pass.dispatch_workgroups(WORKGROUPS_X, rows, 1);
    }
    encoder.copy_buffer_to_buffer(&output_buffer, 0, &readback_buffer, 0, output_size);
    queue.submit(Some(encoder.finish()));
    progress(85);

    let slice = readback_buffer.slice(..);
    let (sender, receiver) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|error| format!("GPU palette poll failed: {error}"))?;
    receiver
        .recv()
        .map_err(|error| format!("GPU palette readback channel failed: {error}"))?
        .map_err(|error| format!("GPU palette readback failed: {error}"))?;

    let mapped = slice.get_mapped_range();
    let mut lookup = Vec::with_capacity(LUT_ENTRY_COUNT * 2);
    for word in mapped.chunks_exact(4) {
        lookup.extend_from_slice(word);
    }
    drop(mapped);
    readback_buffer.unmap();
    lookup.truncate(LUT_ENTRY_COUNT * 2);
    progress(100);
    Ok(lookup)
}

fn palette_config_bytes(config: &PaletteConfig) -> Vec<u8> {
    let mut floats = Vec::with_capacity(12 + 256 * 4);
    push_vec4(
        &mut floats,
        config.matching.lightness,
        config.matching.chroma,
        config.matching.hue,
        config.colors.len().max(1) as f32,
    );
    push_vec4(
        &mut floats,
        config.matching.lightness_multiply,
        config.matching.lightness_add,
        config.matching.chroma_multiply,
        config.matching.chroma_add,
    );
    push_vec4(
        &mut floats,
        config.matching.hue_add,
        config.matching.grey_chroma_threshold,
        0.0,
        0.0,
    );
    for index in 0..256 {
        let [r, g, b, a] = config.colors.get(index).copied().unwrap_or([0, 0, 0, 255]);
        push_vec4(
            &mut floats,
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            f32::from(a) / 255.0,
        );
    }

    let mut bytes = Vec::with_capacity(floats.len() * std::mem::size_of::<f32>());
    for value in floats {
        bytes.extend_from_slice(&value.to_ne_bytes());
    }
    bytes
}

fn push_vec4(output: &mut Vec<f32>, x: f32, y: f32, z: f32, w: f32) {
    output.extend_from_slice(&[x, y, z, w]);
}

const PALETTE_LOOKUP_COMPUTE_SHADER: &str = r#"
const LUT_ENTRY_COUNT: u32 = 16777216u;
const PACKED_LOOKUP_WORDS: u32 = 8388608u;
const WORKGROUP_SIZE: u32 = 64u;
const WORKGROUPS_X: u32 = 1024u;
const INVOCATIONS_PER_ROW: u32 = WORKGROUP_SIZE * WORKGROUPS_X;

struct PaletteData {
    bias: vec4<f32>,
    input_offset_a: vec4<f32>,
    input_offset_b: vec4<f32>,
    colors: array<vec4<f32>, 256>,
};

@group(0) @binding(0)
var<storage, read> palette_data: PaletteData;

@group(0) @binding(1)
var<storage, read_write> output_words: array<u32>;

fn srgb_to_linear_channel(value: f32) -> f32 {
    let clamped = clamp(value, 0.0, 1.0);
    if clamped <= 0.04045 {
        return clamped / 12.92;
    }
    return pow((clamped + 0.055) / 1.055, 2.4);
}

fn srgb_to_linear(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        srgb_to_linear_channel(rgb.r),
        srgb_to_linear_channel(rgb.g),
        srgb_to_linear_channel(rgb.b)
    );
}

fn rgb_to_oklab(rgb: vec3<f32>) -> vec3<f32> {
    let linear = srgb_to_linear(rgb);
    let l = 0.41222146 * linear.r + 0.53633255 * linear.g + 0.051445995 * linear.b;
    let m = 0.2119035 * linear.r + 0.6806995 * linear.g + 0.10739696 * linear.b;
    let s = 0.08830246 * linear.r + 0.28171884 * linear.g + 0.6299787 * linear.b;

    let l_ = pow(max(l, 0.0), 1.0 / 3.0);
    let m_ = pow(max(m, 0.0), 1.0 / 3.0);
    let s_ = pow(max(s, 0.0), 1.0 / 3.0);

    return vec3<f32>(
        0.21045426 * l_ + 0.7936178 * m_ - 0.004072047 * s_,
        1.9779985 * l_ - 2.4285922 * m_ + 0.4505937 * s_,
        0.025904037 * l_ + 0.78277177 * m_ - 0.80867577 * s_
    );
}

fn oklab_to_oklch(oklab: vec3<f32>) -> vec3<f32> {
    let chroma = sqrt(oklab.y * oklab.y + oklab.z * oklab.z);
    let hue = select(0.0, atan2(oklab.z, oklab.y), chroma > 0.000001);
    return vec3<f32>(oklab.x, chroma, hue);
}

fn oklch_to_linear_srgb(color: vec3<f32>) -> vec3<f32> {
    let a = cos(color.z) * color.y;
    let b = sin(color.z) * color.y;

    let l_ = color.x + 0.39633778 * a + 0.21580376 * b;
    let m_ = color.x - 0.105561346 * a - 0.06385417 * b;
    let s_ = color.x - 0.08948418 * a - 1.2914855 * b;

    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    return vec3<f32>(
        4.0767417 * l - 3.3077116 * m + 0.23096994 * s,
        -1.268438 * l + 2.6097574 * m - 0.34131938 * s,
        -0.0041960863 * l - 0.7034186 * m + 1.7076147 * s
    );
}

fn in_srgb_gamut(rgb: vec3<f32>) -> bool {
    let epsilon = 0.000001;
    return all(rgb >= vec3<f32>(-epsilon)) && all(rgb <= vec3<f32>(1.0 + epsilon));
}

fn clamp_chroma_to_srgb_gamut(color: vec3<f32>) -> f32 {
    if color.y <= 0.0 || in_srgb_gamut(oklch_to_linear_srgb(color)) {
        return max(color.y, 0.0);
    }

    var low = 0.0;
    var high = color.y;
    for (var i = 0; i < 16; i = i + 1) {
        let mid = (low + high) * 0.5;
        if in_srgb_gamut(oklch_to_linear_srgb(vec3<f32>(color.x, mid, color.z))) {
            low = mid;
        } else {
            high = mid;
        }
    }
    return low;
}

fn apply_input_offset(color: vec3<f32>) -> vec3<f32> {
    let value_chroma = palette_data.input_offset_a;
    let grey_chroma_threshold = clamp(palette_data.input_offset_b.y, 0.0, 1.0);
    let chroma_offset_weight = smoothstep(
        grey_chroma_threshold,
        grey_chroma_threshold + 0.04,
        color.y,
    );
    let adjusted_chroma = max(
        color.y + (color.y * value_chroma.z + value_chroma.w) * chroma_offset_weight,
        0.0,
    );
    var adjusted = vec3<f32>(
        clamp(color.x * (1.0 + value_chroma.x) + value_chroma.y, 0.0, 1.0),
        adjusted_chroma,
        color.z + palette_data.input_offset_b.x * 2.0 * 3.14159265,
    );
    adjusted.y = clamp_chroma_to_srgb_gamut(adjusted);
    return adjusted;
}

fn oklch_to_oklab(color: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(color.x, cos(color.z) * color.y, sin(color.z) * color.y);
}

fn delta_e_ok_distance_squared(color: vec3<f32>, palette_color: vec3<f32>) -> f32 {
    let delta = color - palette_color;
    return dot(delta, delta);
}

fn palette_index_for_entry(entry_index: u32) -> u32 {
    let direct = entry_index >= LUT_ENTRY_COUNT;
    let rgb_index = select(entry_index, entry_index - LUT_ENTRY_COUNT, direct);
    let r = f32((rgb_index / 65536u) & 255u) / 255.0;
    let g = f32((rgb_index / 256u) & 255u) / 255.0;
    let b = f32(rgb_index & 255u) / 255.0;
    var source = oklab_to_oklch(rgb_to_oklab(vec3<f32>(r, g, b)));
    if !direct {
        source = apply_input_offset(source);
    }
    let source_oklab = oklch_to_oklab(source);

    let palette_count = min(u32(max(palette_data.bias.w, 1.0)), 256u);
    var best_index = 0u;
    var best_distance = 3.4028234663852886e38;
    for (var index = 0u; index < 256u; index = index + 1u) {
        if index >= palette_count {
            break;
        }
        let palette_color = rgb_to_oklab(palette_data.colors[index].rgb);
        let distance = delta_e_ok_distance_squared(source_oklab, palette_color);
        if distance < best_distance {
            best_distance = distance;
            best_index = index;
        }
    }
    return best_index;
}

@compute @workgroup_size(64, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let packed_index = id.y * INVOCATIONS_PER_ROW + id.x;
    if packed_index >= PACKED_LOOKUP_WORDS {
        return;
    }
    let base_entry = packed_index * 4u;
    let a = palette_index_for_entry(base_entry);
    let b = palette_index_for_entry(base_entry + 1u);
    let c = palette_index_for_entry(base_entry + 2u);
    let d = palette_index_for_entry(base_entry + 3u);
    output_words[packed_index] = a | (b << 8u) | (c << 16u) | (d << 24u);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette_lut::PaletteMatching;

    #[test]
    #[ignore = "requires a GPU and builds the full IPSMAP6 altered/direct entry table"]
    fn gpu_lookup_builds_ipsmap6_altered_and_direct_entries() {
        let config = PaletteConfig {
            colors: vec![[0, 0, 0, 255], [255, 255, 255, 255]],
            matching: PaletteMatching {
                lightness_add: 1.0,
                ..PaletteMatching::default()
            },
        };

        let lookup = build_lookup_gpu_with_progress(&config, |_| {}).expect("gpu lookup");
        assert_eq!(lookup.len(), LUT_ENTRY_COUNT * 2);
        assert_eq!(lookup[0], 1);
        assert_eq!(lookup[LUT_ENTRY_COUNT], 0);
    }
}
