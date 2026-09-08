// Schlick's approximation to the Fresnel reflectance of a surface.
//
// `f0` is the reflectance at normal incidence (0.04-ish for dielectrics, the
// base colour for metals); `cos_theta` is the cosine between the view (or
// light) direction and the microfacet half vector.
fn fresnel_schlick(f0: vec3f, cos_theta: f32) -> vec3f {
    let falloff = pow(saturate(1.0 - cos_theta), 5.0);
    return f0 + (vec3f(1.0, 1.0, 1.0) - f0) * falloff;
}
