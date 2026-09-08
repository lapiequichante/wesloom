// Analytic hemisphere ambient light: a cheap stand-in for an image-based
// environment when there is no cubemap to sample.
//
// The environment is modelled as two colours, sky above and bounce below,
// blended by the vertical component of the direction being looked along. The
// diffuse term looks along the normal; the specular term looks along the
// mirror direction and blurs towards the diffuse irradiance as roughness
// rises, which is the qualitative behaviour a prefiltered cubemap has.
//
// Original implementation; the "roughness blurs the reflection towards the
// average" idea is the standard split-sum intuition, not copied code.

import package::lighting::fresnel_schlick::fresnel_schlick;

fn ambient_environment(
    normal: vec3f,
    view_direction: vec3f,
    base_color: vec3f,
    metallic: f32,
    roughness: f32,
    sky_color: vec3f,
    ground_color: vec3f,
) -> vec3f {
    let diffuse_irradiance = mix(ground_color, sky_color, normal.y * 0.5 + 0.5);
    let mirror = reflect(-view_direction, normal);
    let mirror_irradiance = mix(ground_color, sky_color, mirror.y * 0.5 + 0.5);

    let f0 = mix(vec3f(0.04, 0.04, 0.04), base_color, saturate(metallic));
    let n_dot_v = saturate(dot(normal, view_direction));
    // A rough surface has a weaker, less saturated horizon response than a
    // smooth one, so the Fresnel edge is faded by roughness.
    let fresnel = mix(f0, fresnel_schlick(f0, n_dot_v), 1.0 - saturate(roughness));

    let diffuse = base_color * (1.0 - saturate(metallic)) * diffuse_irradiance;
    let specular = mix(mirror_irradiance, diffuse_irradiance, saturate(roughness)) * fresnel;
    return diffuse + specular;
}
