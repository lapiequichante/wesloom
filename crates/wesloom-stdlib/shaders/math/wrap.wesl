// Wrap `value` into [low, high), the way a repeating texture coordinate does.
//
// Unlike `%`, this is correct for negative inputs: wrap(-0.25, 0.0, 1.0) is
// 0.75, not -0.25.
fn wrap(value: f32, low: f32, high: f32) -> f32 {
    let span = high - low;
    if abs(span) < 1e-8 {
        return low;
    }
    return low + span * fract((value - low) / span);
}
