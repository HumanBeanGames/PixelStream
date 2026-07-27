// Copies the raw scene preview texture for paired before/after debug views.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

@group(#{MATERIAL_BIND_GROUP}) @binding(0)
var source_image: texture_2d<f32>;

@group(#{MATERIAL_BIND_GROUP}) @binding(1)
var source_sampler: sampler;

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

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let source_size = textureDimensions(source_image);
    let source_uv = clamp(mesh.uv, vec2<f32>(0.0), vec2<f32>(0.999999));
    let source_coord = vec2<i32>(floor(source_uv * vec2<f32>(source_size)));
    let source = textureLoad(source_image, source_coord, 0);
    return vec4<f32>(linear_to_srgb(source.rgb), source.a);
}
