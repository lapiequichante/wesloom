// Where `value` sits between `a` and `b`, as a 0..1 factor.
//
// The inverse of `mix`: `inverse_lerp(a, b, mix(a, b, t)) == t`.
fn inverse_lerp(a: f32, b: f32, value: f32) -> f32 {
    let span = b - a;
    if abs(span) < 1e-8 {
        return 0.0;
    }
    return (value - a) / span;
}
