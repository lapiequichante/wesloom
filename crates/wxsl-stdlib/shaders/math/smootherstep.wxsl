// Quintic smoothstep: like `smoothstep`, with a continuous second derivative.
//
// Worth the extra multiply wherever the first derivative's discontinuity
// shows up as a visible crease — noise interpolation, or a value driving a
// normal.
fn smootherstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let span = edge1 - edge0;
    if abs(span) < 1e-8 {
        return select(0.0, 1.0, x >= edge1);
    }
    let t = saturate((x - edge0) / span);
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}
