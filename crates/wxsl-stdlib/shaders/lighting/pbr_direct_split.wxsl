// One light's contribution to a physically-based surface, kept split into
// its diffuse and specular halves.
//
// This is the "complex" end of the library: a real function with a body, a
// struct return, and four smaller functions imported rather than inlined.
// The split return is not decoration — a graph that only wants the specular
// response (a glint mask, a reflection-only pass) gets it without paying for
// the diffuse term, because the caller evaluates the function once and reads
// the field it needs.
//
// All directions are world space and unit length: `light_direction` points
// from the surface *towards* the light, `view_direction` towards the eye.
// `radiance` is the light's radiance already attenuated for distance.

import package::lighting::diffuse_lambert::diffuse_lambert;
import package::lighting::distribution_ggx::distribution_ggx;
import package::lighting::fresnel_schlick::fresnel_schlick;
import package::lighting::visibility_smith::visibility_smith;

struct PbrDirect {
    diffuse: vec3f,
    specular: vec3f,
}

fn pbr_direct_split(
    normal: vec3f,
    view_direction: vec3f,
    light_direction: vec3f,
    radiance: vec3f,
    base_color: vec3f,
    metallic: f32,
    roughness: f32,
) -> PbrDirect {
    var out: PbrDirect;
    out.diffuse = vec3f(0.0, 0.0, 0.0);
    out.specular = vec3f(0.0, 0.0, 0.0);

    // Backfacing with respect to the light: no contribution at all, and the
    // half vector below would be meaningless.
    let n_dot_l = dot(normal, light_direction);
    if n_dot_l <= 0.0 {
        return out;
    }

    let n_dot_v = max(dot(normal, view_direction), 1e-4);
    let half_vector = normalize(light_direction + view_direction);
    let n_dot_h = saturate(dot(normal, half_vector));
    let v_dot_h = saturate(dot(view_direction, half_vector));

    // Dielectrics reflect ~4% at normal incidence; metals reflect their own
    // colour and have no diffuse lobe (handled in `diffuse_lambert`).
    let f0 = mix(vec3f(0.04, 0.04, 0.04), base_color, saturate(metallic));
    let fresnel = fresnel_schlick(f0, v_dot_h);

    let specular = fresnel
        * distribution_ggx(n_dot_h, roughness)
        * visibility_smith(n_dot_v, n_dot_l, roughness);
    // What is not reflected is available to the diffuse lobe.
    let transmitted = vec3f(1.0, 1.0, 1.0) - fresnel;
    let diffuse = transmitted * diffuse_lambert(base_color, metallic);

    let irradiance = radiance * n_dot_l;
    out.diffuse = diffuse * irradiance;
    out.specular = specular * irradiance;
    return out;
}
