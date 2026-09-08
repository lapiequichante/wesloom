// Signed distance to an axis-aligned box centred on the origin.
//
// Exact outside; inside it returns the (negative) distance to the nearest
// face, which is the usual bounded approximation.
fn sdf_box(p: vec3f, half_extents: vec3f) -> f32 {
    let q = abs(p) - half_extents;
    let outside = length(max(q, vec3f(0.0)));
    let inside = min(max(q.x, max(q.y, q.z)), 0.0);
    return outside + inside;
}
