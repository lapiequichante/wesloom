// One light's total contribution to a physically-based surface.
//
// The common case of `pbr_direct_split`: diffuse plus specular. Kept as its
// own function so the frequent case is one node with one output instead of a
// split node plus an add node.

import package::lighting::pbr_direct_split::pbr_direct_split;

fn pbr_direct(
    normal: vec3f,
    view_direction: vec3f,
    light_direction: vec3f,
    radiance: vec3f,
    base_color: vec3f,
    metallic: f32,
    roughness: f32,
) -> vec3f {
    let split = pbr_direct_split(
        normal,
        view_direction,
        light_direction,
        radiance,
        base_color,
        metallic,
        roughness,
    );
    return split.diffuse + split.specular;
}
