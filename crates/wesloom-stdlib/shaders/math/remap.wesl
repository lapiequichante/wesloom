// Map `value` from the range [in_min, in_max] onto [out_min, out_max].
//
// The workhorse for wiring a 0..1 signal (a noise field, a mask) into a
// socket that wants some other range. Not clamped: feed the result through
// `clamp` if the input can leave its stated range.
fn remap(value: f32, in_min: f32, in_max: f32, out_min: f32, out_max: f32) -> f32 {
    let span = in_max - in_min;
    // A zero-width input range has no meaningful mapping; collapse to the
    // start of the output range instead of dividing by zero.
    if abs(span) < 1e-8 {
        return out_min;
    }
    return out_min + (value - in_min) * (out_max - out_min) / span;
}
