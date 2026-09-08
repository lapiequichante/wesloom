// The deferred lighting pass: a fullscreen pass that shades the G-buffer.
//
// Compiled as its own root module — it has no material graph, because the
// material already ran when the G-buffer was written. `wesloom-render`
// compiles it once per macro set, not once per material.
//
// World position is reconstructed from the depth buffer rather than stored in
// a fourth attachment: the inverse view-projection is already in the camera
// uniform, so the reconstruction is a matrix multiply against bandwidth we
// would otherwise spend every frame.

import package::wesloom::bindings::{camera, scene};
import package::wesloom::deferred::unpack_gbuffer;
import package::wesloom::shading::shade_surface;
import package::wesloom::surface::SurfaceContext;
import package::space::tangent_basis::tangent_basis;

@group(1) @binding(0) var gbuffer_base_color: texture_2d<f32>;
@group(1) @binding(1) var gbuffer_normal: texture_2d<f32>;
@group(1) @binding(2) var gbuffer_emissive: texture_2d<f32>;
@group(1) @binding(3) var gbuffer_depth: texture_depth_2d;

// A fullscreen triangle covering the viewport, from three vertices and no
// vertex buffer: (-1,-1), (3,-1), (-1,3).
@vertex
fn lighting_vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {
    let x = f32(i32(index & 1u) * 4 - 1);
    let y = f32(i32(index >> 1u) * 4 - 1);
    return vec4f(x, y, 0.0, 1.0);
}

@fragment
fn lighting_fs(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let coord = vec2i(position.xy);
    let depth = textureLoad(gbuffer_depth, coord, 0);
    // Nothing was drawn here: leave the clear colour alone.
    if depth >= 1.0 {
        discard;
    }

    let size = vec2f(textureDimensions(gbuffer_base_color));
    let uv = position.xy / size;
    // Framebuffer coordinates are y-down, clip space is y-up.
    let ndc = vec3f(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth);
    let world = camera.inverse_view_proj * vec4f(ndc, 1.0);
    let world_position = world.xyz / world.w;

    let surface = unpack_gbuffer(
        textureLoad(gbuffer_base_color, coord, 0),
        textureLoad(gbuffer_normal, coord, 0),
        textureLoad(gbuffer_emissive, coord, 0),
    );

    // The G-buffer carries no tangent frame, so one is synthesized around
    // the stored normal. `shade_surface` does not read it; it is filled in so
    // that the context is well-formed rather than full of zeroes.
    let basis = tangent_basis(surface.normal);
    var ctx: SurfaceContext;
    ctx.world_position = world_position;
    ctx.world_normal = surface.normal;
    ctx.world_tangent = basis[0];
    ctx.world_bitangent = basis[1];
    ctx.view_direction = normalize(camera.position - world_position);
    ctx.uv = uv;
    ctx.time = scene.time;

    return shade_surface(surface, ctx);
}
