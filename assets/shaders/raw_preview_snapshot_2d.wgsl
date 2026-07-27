// Captures a raw preview snapshot for pixel-debug readback.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

@group(#{MATERIAL_BIND_GROUP}) @binding(0)
var source_image: texture_2d<f32>;

@group(#{MATERIAL_BIND_GROUP}) @binding(1)
var source_sampler: sampler;

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let source_size = textureDimensions(source_image);
    let source_coord = clamp(
        vec2<i32>(floor(mesh.position.xy)),
        vec2<i32>(0),
        vec2<i32>(source_size) - vec2<i32>(1),
    );
    return textureLoad(source_image, source_coord, 0);
}
