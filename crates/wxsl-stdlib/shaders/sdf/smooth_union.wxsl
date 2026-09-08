// Union of two signed distance fields with a rounded seam of width `k`.
//
// Polynomial smooth minimum: cheaper than the exponential form and, unlike
// plain `min`, leaves no derivative discontinuity for a normal to catch on.
// `k` of 0 degenerates to `min`.
fn sdf_smooth_union(a: f32, b: f32, k: f32) -> f32 {
    if k <= 1e-6 {
        return min(a, b);
    }
    // How far the two fields are within blending distance of each other.
    let blend = saturate(0.5 + (b - a) / (2.0 * k));
    return mix(b, a, blend) - k * blend * (1.0 - blend);
}
