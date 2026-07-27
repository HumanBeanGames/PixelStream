// Converts captured scene colors into palette indices through the IPSMAP lookup.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct PaletteParams {
    bias: vec4<f32>,
};

struct LookupParams {
    flags: vec4<f32>,
};

struct InputOffsetParams {
    value_chroma: vec4<f32>,
};

struct InputHueOffsetParams {
    values: vec4<f32>,
};

struct DitherParams {
    values: vec4<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0)
var<uniform> palette_params: PaletteParams;

@group(#{MATERIAL_BIND_GROUP}) @binding(1)
var source_image: texture_2d<f32>;

@group(#{MATERIAL_BIND_GROUP}) @binding(2)
var source_sampler: sampler;

@group(#{MATERIAL_BIND_GROUP}) @binding(3)
var palette_texture: texture_2d<f32>;

@group(#{MATERIAL_BIND_GROUP}) @binding(4)
var<uniform> lookup_params: LookupParams;

@group(#{MATERIAL_BIND_GROUP}) @binding(5)
var lookup_texture: texture_2d<u32>;

@group(#{MATERIAL_BIND_GROUP}) @binding(6)
var<uniform> input_offset_params: InputOffsetParams;

@group(#{MATERIAL_BIND_GROUP}) @binding(7)
var<uniform> input_hue_offset_params: InputHueOffsetParams;

@group(#{MATERIAL_BIND_GROUP}) @binding(8)
var<uniform> dither_params_a: DitherParams;

@group(#{MATERIAL_BIND_GROUP}) @binding(9)
var<uniform> dither_params_b: DitherParams;

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
    let l = 0.41222146 * rgb.r + 0.53633255 * rgb.g + 0.051445995 * rgb.b;
    let m = 0.2119035 * rgb.r + 0.6806995 * rgb.g + 0.10739696 * rgb.b;
    let s = 0.08830246 * rgb.r + 0.28171884 * rgb.g + 0.6299787 * rgb.b;

    let l_ = pow(max(l, 0.0), 1.0 / 3.0);
    let m_ = pow(max(m, 0.0), 1.0 / 3.0);
    let s_ = pow(max(s, 0.0), 1.0 / 3.0);

    return vec3<f32>(
        0.21045426 * l_ + 0.7936178 * m_ - 0.004072047 * s_,
        1.9779985 * l_ - 2.4285922 * m_ + 0.4505937 * s_,
        0.025904037 * l_ + 0.78277177 * m_ - 0.80867577 * s_,
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
        -0.0041960863 * l - 0.7034186 * m + 1.7076147 * s,
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
    let value_chroma = input_offset_params.value_chroma;
    let grey_chroma_threshold = clamp(input_hue_offset_params.values.y, 0.0, 1.0);
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
        color.z + input_hue_offset_params.values.x * 2.0 * 3.14159265,
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

fn nearest_palette_index_direct(source: vec3<f32>) -> u32 {
    let palette_width = textureDimensions(palette_texture).x;
    let palette_count = min(u32(max(palette_params.bias.w, 1.0)), palette_width);
    let source_oklab = oklch_to_oklab(apply_input_offset(oklab_to_oklch(rgb_to_oklab(srgb_to_linear(source)))));
    var best_index = 0u;
    var best_distance = 3.4028234663852886e38;
    for (var index = 0u; index < 256u; index = index + 1u) {
        if index >= palette_count {
            break;
        }
        let palette_rgb = textureLoad(palette_texture, vec2<i32>(i32(index), 0), 0).rgb;
        let palette_oklab = rgb_to_oklab(srgb_to_linear(palette_rgb));
        let distance = delta_e_ok_distance_squared(source_oklab, palette_oklab);
        if distance < best_distance {
            best_distance = distance;
            best_index = index;
        }
    }
    return best_index;
}

fn nearest_palette_index_raw(source: vec3<f32>) -> u32 {
    let palette_width = textureDimensions(palette_texture).x;
    let palette_count = min(u32(max(palette_params.bias.w, 1.0)), palette_width);
    let source_oklab = rgb_to_oklab(srgb_to_linear(source));
    var best_index = 0u;
    var best_distance = 3.4028234663852886e38;
    for (var index = 0u; index < 256u; index = index + 1u) {
        if index >= palette_count {
            break;
        }
        let palette_rgb = textureLoad(palette_texture, vec2<i32>(i32(index), 0), 0).rgb;
        let palette_oklab = rgb_to_oklab(srgb_to_linear(palette_rgb));
        let distance = delta_e_ok_distance_squared(source_oklab, palette_oklab);
        if distance < best_distance {
            best_distance = distance;
            best_index = index;
        }
    }
    return best_index;
}

fn exact_palette_index(source: vec3<f32>) -> u32 {
    let palette_width = textureDimensions(palette_texture).x;
    let palette_count = min(u32(max(palette_params.bias.w, 1.0)), palette_width);
    let tolerance = 0.5 / 255.0;
    for (var index = 0u; index < 256u; index = index + 1u) {
        if index >= palette_count {
            break;
        }
        let palette_rgb = textureLoad(palette_texture, vec2<i32>(i32(index), 0), 0).rgb;
        if all(abs(source - palette_rgb) <= vec3<f32>(tolerance)) {
            return index;
        }
    }
    return 256u;
}

fn linear_to_srgb_channel(value: f32) -> f32 {
    let clamped = clamp(value, 0.0, 1.0);
    if clamped <= 0.0031308 {
        return clamped * 12.92;
    }
    return 1.055 * pow(clamped, 1.0 / 2.4) - 0.055;
}

fn linear_to_srgb(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        linear_to_srgb_channel(rgb.r),
        linear_to_srgb_channel(rgb.g),
        linear_to_srgb_channel(rgb.b)
    );
}

fn bayer2_index(x: u32, y: u32) -> u32 {
    if x == 0u && y == 0u {
        return 0u;
    }
    if x == 1u && y == 0u {
        return 2u;
    }
    if x == 0u && y == 1u {
        return 3u;
    }
    return 1u;
}

fn bayer8(cell: vec2<u32>) -> f32 {
    var index = 0u;
    for (var bit = 0u; bit < 3u; bit = bit + 1u) {
        let x_bit = (cell.x >> bit) & 1u;
        let y_bit = (cell.y >> bit) & 1u;
        index = index + (bayer2_index(x_bit, y_bit) << (bit * 2u));
    }
    return (f32(index) + 0.5) / 64.0;
}

fn ordered_dither(cell: vec2<i32>, offset: vec2<i32>) -> f32 {
    let shifted = vec2<u32>(
        u32((cell.x + offset.x) & 7),
        u32((cell.y + offset.y) & 7)
    );
    return bayer8(shifted) * 2.0 - 1.0;
}

fn stable_quantize_srgb(source: vec3<f32>) -> vec3<f32> {
    let scaled = clamp(source, vec3<f32>(0.0), vec3<f32>(1.0)) * 255.0;
    let lower = floor(scaled);
    let fraction = scaled - lower;
    let near_boundary = abs(fraction - vec3<f32>(0.5)) <= vec3<f32>(0.25);
    let quantized = select(floor(scaled + vec3<f32>(0.5)), lower, near_boundary);
    return quantized / 255.0;
}

fn apply_dither(source: vec3<f32>, source_coord: vec2<i32>) -> vec3<f32> {
    let scale = max(dither_params_a.values.x, 0.125);
    let intensity = max(dither_params_a.values.y, 0.0);
    if intensity <= 0.0 {
        return source;
    }

    let cell = vec2<i32>(floor((vec2<f32>(f32(source_coord.x), f32(source_coord.y)) + vec2<f32>(0.5)) / scale));
    let value_noise = ordered_dither(cell, vec2<i32>(0, 0));
    let chroma_noise = ordered_dither(cell, vec2<i32>(3, 5));
    let hue_noise = ordered_dither(cell, vec2<i32>(6, 2));
    var oklch = oklab_to_oklch(rgb_to_oklab(srgb_to_linear(source)));
    oklch.x = clamp(oklch.x + value_noise * dither_params_a.values.z * intensity, 0.0, 1.0);
    oklch.y = max(oklch.y + chroma_noise * dither_params_a.values.w * intensity, 0.0);
    oklch.z = oklch.z + hue_noise * dither_params_b.values.x * intensity * 2.0 * 3.14159265;
    oklch.y = clamp_chroma_to_srgb_gamut(oklch);
    let transformed = clamp(linear_to_srgb(oklch_to_linear_srgb(oklch)), vec3<f32>(0.0), vec3<f32>(1.0));
    return stable_quantize_srgb(transformed);
}

fn lookup_palette_index(source: vec3<f32>, direct: bool) -> u32 {
    let lookup_size = textureDimensions(lookup_texture);
    let source_u8 = vec3<u32>(round(source * 255.0));
    let lookup_index = source_u8.r * 65536u + source_u8.g * 256u + source_u8.b;
    var lookup_y = lookup_index / 4096u;
    if direct && lookup_size.y >= 8192u {
        lookup_y = lookup_y + 4096u;
    }
    let lookup_coord = vec2<i32>(
        i32(lookup_index % 4096u),
        i32(lookup_y)
    );
    return textureLoad(lookup_texture, lookup_coord, 0).r;
}

fn lookup_has_direct_entries() -> bool {
    return textureDimensions(lookup_texture).y >= 8192u;
}

fn encode_indexed_byte(value: u32) -> f32 {
    let encoded = min((f32(value) + 0.25) / 255.0, 1.0);
    return srgb_to_linear_channel(encoded);
}

fn indexed_output(palette_index: u32, lookup_source: vec3<f32>) -> vec4<f32> {
    let lookup_rgb = vec3<u32>(round(lookup_source * 255.0));
    return vec4<f32>(
        encode_indexed_byte(palette_index),
        encode_indexed_byte(lookup_rgb.r),
        encode_indexed_byte(lookup_rgb.g),
        min((f32(lookup_rgb.b) + 0.25) / 255.0, 1.0),
    );
}

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let source_size = textureDimensions(source_image);
    let framebuffer_coord = vec2<i32>(floor(mesh.position.xy));
    let source_coord = clamp(
        framebuffer_coord,
        vec2<i32>(0),
        vec2<i32>(source_size) - vec2<i32>(1),
    );
    let raw_sample = textureLoad(source_image, source_coord, 0);
    let raw_source = clamp(raw_sample.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let direct_alpha_flag = abs(raw_sample.a - (254.0 / 255.0)) <= (0.5 / 255.0);
    if direct_alpha_flag {
        var palette_index = nearest_palette_index_raw(raw_source);
        if lookup_params.flags.x > 0.5 && lookup_has_direct_entries() {
            palette_index = lookup_palette_index(raw_source, true);
        }
        return indexed_output(palette_index, raw_source);
    }
    let exact_index = exact_palette_index(raw_source);
    if exact_index < 256u {
        return indexed_output(exact_index, raw_source);
    }
    let source = apply_dither(raw_source, framebuffer_coord);
    var palette_index = nearest_palette_index_direct(source);
    if lookup_params.flags.x > 0.5 {
        palette_index = lookup_palette_index(source, false);
    }
    return indexed_output(palette_index, source);
}
