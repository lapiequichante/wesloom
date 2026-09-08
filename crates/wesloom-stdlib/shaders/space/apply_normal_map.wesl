// Take a tangent-space normal (as stored in a normal map, already decoded to
// -1..1) into world space, with an adjustable strength.
//
// `strength` scales the tangential components only, so 0 gives the geometric
// normal back and values above 1 exaggerate the detail without denormalizing
// the result.
fn apply_normal_map(
    tangent_normal: vec3f,
    normal: vec3f,
    tangent: vec3f,
    bitangent: vec3f,
    strength: f32,
) -> vec3f {
    let scaled = vec3f(tangent_normal.xy * strength, max(tangent_normal.z, 1e-4));
    let basis = mat3x3f(normalize(tangent), normalize(bitangent), normalize(normal));
    return normalize(basis * scaled);
}
