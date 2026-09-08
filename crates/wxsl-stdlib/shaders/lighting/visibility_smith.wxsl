// Height-correlated Smith visibility for the GGX distribution.
//
// This is the *visibility* term, i.e. the Smith geometry function already
// divided by the 4·(n·l)·(n·v) denominator of the microfacet BRDF, so a
// specular lobe is `D * Vis * F` with no further division. Technique
// reference: Heitz, "Understanding the Masking-Shadowing Function in
// Microfacet-Based BRDFs" (2014); implementation our own.
fn visibility_smith(n_dot_v: f32, n_dot_l: f32, roughness: f32) -> f32 {
    let alpha = max(roughness * roughness, 1e-3);
    let alpha_squared = alpha * alpha;
    let view_term = n_dot_l * sqrt(n_dot_v * n_dot_v * (1.0 - alpha_squared) + alpha_squared);
    let light_term = n_dot_v * sqrt(n_dot_l * n_dot_l * (1.0 - alpha_squared) + alpha_squared);
    return 0.5 / max(view_term + light_term, 1e-5);
}
