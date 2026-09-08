// Value noise in three dimensions: a hashed value per lattice corner,
// trilinearly interpolated with a quintic fade.
//
// Returns 0..1, is continuous everywhere, and its first two derivatives are
// continuous across cell boundaries (that is what the quintic fade buys over
// a linear one — a linear fade leaves visible creases on the lattice planes).

import package::generative::hash13::hash13;
import package::math::smootherstep::smootherstep;

fn value_noise3(p: vec3f) -> f32 {
    let cell = floor(p);
    let local = p - cell;
    let fade = vec3f(
        smootherstep(0.0, 1.0, local.x),
        smootherstep(0.0, 1.0, local.y),
        smootherstep(0.0, 1.0, local.z),
    );

    let c000 = hash13(cell + vec3f(0.0, 0.0, 0.0));
    let c100 = hash13(cell + vec3f(1.0, 0.0, 0.0));
    let c010 = hash13(cell + vec3f(0.0, 1.0, 0.0));
    let c110 = hash13(cell + vec3f(1.0, 1.0, 0.0));
    let c001 = hash13(cell + vec3f(0.0, 0.0, 1.0));
    let c101 = hash13(cell + vec3f(1.0, 0.0, 1.0));
    let c011 = hash13(cell + vec3f(0.0, 1.0, 1.0));
    let c111 = hash13(cell + vec3f(1.0, 1.0, 1.0));

    let x00 = mix(c000, c100, fade.x);
    let x10 = mix(c010, c110, fade.x);
    let x01 = mix(c001, c101, fade.x);
    let x11 = mix(c011, c111, fade.x);
    return mix(mix(x00, x10, fade.y), mix(x01, x11, fade.y), fade.z);
}
