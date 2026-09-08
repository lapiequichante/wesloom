// Relative luminance of a linear-light colour (Rec. 709 primaries).
fn luminance(color: vec3f) -> f32 {
    return dot(color, vec3f(0.2126, 0.7152, 0.0722));
}
