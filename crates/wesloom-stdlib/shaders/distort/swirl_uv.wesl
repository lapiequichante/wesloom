// Swirl `uv` around `center`: rotate by an amount that falls off with
// distance, so the middle twists and the edges stay put.
//
// `radius` is where the swirl fades to nothing; `turns` is how far the very
// centre is rotated.
import package::space::rotate_uv::rotate_uv;

fn swirl_uv(uv: vec2f, center: vec2f, radius: f32, turns: f32) -> vec2f {
    let distance_to_center = length(uv - center);
    // Smooth falloff, so there is no visible seam at the radius.
    let falloff = 1.0 - smoothstep(0.0, max(radius, 1e-5), distance_to_center);
    return rotate_uv(uv, center, turns * falloff * falloff);
}
