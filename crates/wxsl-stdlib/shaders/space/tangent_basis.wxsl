// An arbitrary but stable orthonormal basis around `normal`, as the columns
// (tangent, bitangent, normal) of a mat3x3f.
//
// For geometry with real UVs the interpolated tangent frame is the right
// thing to use. This is for the cases where there is none: the deferred
// lighting pass, which reads a normal out of the G-buffer with no tangent
// alongside it, and procedural nodes that just need *some* frame.
fn tangent_basis(normal: vec3f) -> mat3x3f {
    let n = normalize(normal);
    // Pick the reference axis least aligned with the normal, so the cross
    // product below never degenerates.
    let reference = select(vec3f(0.0, 1.0, 0.0), vec3f(1.0, 0.0, 0.0), abs(n.y) > 0.99);
    let tangent = normalize(cross(reference, n));
    return mat3x3f(tangent, cross(n, tangent), n);
}
