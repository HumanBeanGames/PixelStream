// Displays the unquantized raw preview image.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

@group(#{MATERIAL_BIND_GROUP}) @binding(0)
var source_image: texture_2d<f32>;

@group(#{MATERIAL_BIND_GROUP}) @binding(1)
var source_sampler: sampler;

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

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let source_size = textureDimensions(source_image);
    let source_uv = clamp(mesh.uv, vec2<f32>(0.0), vec2<f32>(0.999999));
    let source_coord = vec2<i32>(floor(source_uv * vec2<f32>(source_size)));
    let source = textureLoad(source_image, source_coord, 0);
    return vec4<f32>(srgb_to_linear(source.rgb), source.a);
}
