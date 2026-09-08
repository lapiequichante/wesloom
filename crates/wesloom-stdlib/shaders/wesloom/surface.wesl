// The two structs a material graph is written against.
//
// `SurfaceContext` is what the renderer hands the material function;
// `Surface` is what the material function returns. Both mirror the tables in
// `wesloom_core::abi` field for field, in order — that module is the single
// place the Rust and WESL halves of the ABI agree, so a change here is a
// change there.

struct SurfaceContext {
    world_position: vec3f,
    world_normal: vec3f,
    world_tangent: vec3f,
    world_bitangent: vec3f,
    view_direction: vec3f,
    uv: vec2f,
    time: f32,
}

struct Surface {
    base_color: vec3f,
    metallic: f32,
    roughness: f32,
    normal: vec3f,
    emissive: vec3f,
    occlusion: f32,
    alpha: f32,
}

// A neutral dielectric surface using the geometric normal.
//
// The generated material function starts from this and overwrites only the
// fields the graph drives, which is how an unconnected `normal` input keeps
// the interpolated geometric normal. The literal values here are mirrored by
// `SURFACE_FIELDS`' editable defaults in `wesloom_core::abi`, so what the
// editor shows on an unconnected input is what the shader would have used.
fn default_surface(ctx: SurfaceContext) -> Surface {
    var out: Surface;
    out.base_color = vec3f(0.8, 0.8, 0.8);
    out.metallic = 0.0;
    out.roughness = 0.5;
    out.normal = normalize(ctx.world_normal);
    out.emissive = vec3f(0.0, 0.0, 0.0);
    out.occlusion = 1.0;
    out.alpha = 1.0;
    return out;
}
