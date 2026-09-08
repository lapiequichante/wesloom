// Rotate `uv` around `pivot` by `turns` (1.0 = a full turn).
//
// Turns rather than radians for the same reason hue is in turns: it composes
// with time and with fractional values without a magic constant in the graph.
fn rotate_uv(uv: vec2f, pivot: vec2f, turns: f32) -> vec2f {
    let angle = turns * 6.28318531;
    let c = cos(angle);
    let s = sin(angle);
    let offset = uv - pivot;
    return pivot + vec2f(offset.x * c - offset.y * s, offset.x * s + offset.y * c);
}
