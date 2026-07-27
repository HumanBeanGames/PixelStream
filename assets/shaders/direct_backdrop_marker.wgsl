// Draws backdrop sprites while marking output pixels for the direct lookup path.

#import bevy_pbr::forward_io::VertexOutput

@group(#{MATERIAL_BIND_GROUP}) @binding(0)
var<uniform> uv_scale_offset: vec4<f32>;

@group(#{MATERIAL_BIND_GROUP}) @binding(1)
var backdrop_texture: texture_2d<f32>;

@group(#{MATERIAL_BIND_GROUP}) @binding(2)
var backdrop_sampler: sampler;

@group(#{MATERIAL_BIND_GROUP}) @binding(3)
var<uniform> alpha_params: vec4<f32>;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv * uv_scale_offset.xy + uv_scale_offset.zw;
    let source_alpha = textureSample(backdrop_texture, backdrop_sampler, uv).a * alpha_params.y;
    if source_alpha < alpha_params.x {
        discard;
    }
    return vec4<f32>(0.0, 0.0, 0.0, alpha_params.z);
}
