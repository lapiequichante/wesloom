// The vertex stage, shared by both render paths, and the bridge from
// interpolated vertex output to the material function's input.
//
// `VertexIn`'s locations are the vertex buffer layout `wesloom-render`'s
// `mesh` module declares; the two must be edited together.

import package::wesloom::bindings::{camera, object_normal_matrix, object, scene};
import package::wesloom::surface::SurfaceContext;

struct VertexIn {
    @location(0) position: vec3f,
    @location(1) normal: vec3f,
    // xyz = tangent, w = bitangent sign (handedness).
    @location(2) tangent: vec4f,
    @location(3) uv: vec2f,
}

struct VertexOut {
    @builtin(position) clip_position: vec4f,
    @location(0) world_position: vec3f,
    @location(1) world_normal: vec3f,
    @location(2) world_tangent: vec3f,
    @location(3) world_bitangent: vec3f,
    @location(4) uv: vec2f,
}

// Object space to clip space, carrying a world-space shading basis along.
//
// Both render paths use this unchanged: the paths differ only in what the
// fragment stage does with the surface, never in how geometry is transformed.
fn transform_vertex(input: VertexIn) -> VertexOut {
    let world = object.model * vec4f(input.position, 1.0);
    let normal_matrix = object_normal_matrix();
    let normal = normalize(normal_matrix * input.normal);
    let tangent = normalize(normal_matrix * input.tangent.xyz);

    var out: VertexOut;
    out.clip_position = camera.view_proj * world;
    out.world_position = world.xyz;
    out.world_normal = normal;
    out.world_tangent = tangent;
    out.world_bitangent = cross(normal, tangent) * input.tangent.w;
    out.uv = input.uv;
    return out;
}

// Interpolated vertex output to material input.
//
// Interpolation denormalizes the basis vectors, so they are renormalized
// here rather than in every node that reads them.
fn surface_context(vertex: VertexOut) -> SurfaceContext {
    var ctx: SurfaceContext;
    ctx.world_position = vertex.world_position;
    ctx.world_normal = normalize(vertex.world_normal);
    ctx.world_tangent = normalize(vertex.world_tangent);
    ctx.world_bitangent = normalize(vertex.world_bitangent);
    ctx.view_direction = normalize(camera.position - vertex.world_position);
    ctx.uv = vertex.uv;
    ctx.time = scene.time;
    return ctx;
}
