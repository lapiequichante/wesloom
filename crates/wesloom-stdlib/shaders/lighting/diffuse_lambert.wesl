// Lambertian diffuse albedo, energy-normalized and metal-aware.
//
// Metals have no diffuse lobe, so the base colour is faded out as `metallic`
// rises rather than being handled by a branch.
fn diffuse_lambert(base_color: vec3f, metallic: f32) -> vec3f {
    let inverse_pi = 0.31830989;
    return base_color * (1.0 - saturate(metallic)) * inverse_pi;
}
