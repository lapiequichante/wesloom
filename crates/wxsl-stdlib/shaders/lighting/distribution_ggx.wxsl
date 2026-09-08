// GGX / Trowbridge-Reitz microfacet normal distribution.
//
// Technique reference: the GGX distribution as described in Walter et al.,
// "Microfacet Models for Refraction through Rough Surfaces" (2007). The
// implementation below is our own.
//
// `roughness` is perceptual: it is squared into the microfacet alpha, which
// makes an artist-facing 0..1 slider behave linearly.
fn distribution_ggx(n_dot_h: f32, roughness: f32) -> f32 {
    // Clamped away from a perfect mirror: alpha = 0 makes the lobe a Dirac
    // spike no finite-sample estimator can represent, and NaNs follow.
    let alpha = max(roughness * roughness, 1e-3);
    let alpha_squared = alpha * alpha;
    let denominator = n_dot_h * n_dot_h * (alpha_squared - 1.0) + 1.0;
    return alpha_squared / max(3.14159265 * denominator * denominator, 1e-7);
}
