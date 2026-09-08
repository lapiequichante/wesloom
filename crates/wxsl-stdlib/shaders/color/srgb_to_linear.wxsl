// sRGB-encoded values to linear light, per the sRGB transfer function.
//
// Colours a human picked (in a colour wheel, or as a hex code) are sRGB
// encoded; every lighting computation wants linear light. Put this between a
// colour input and a `base_color` socket.
fn srgb_to_linear(color: vec3f) -> vec3f {
    let low = color / 12.92;
    let high = pow(
        (max(color, vec3f(0.0, 0.0, 0.0)) + vec3f(0.055, 0.055, 0.055)) / 1.055,
        vec3f(2.4, 2.4, 2.4),
    );
    return select(low, high, color > vec3f(0.04045, 0.04045, 0.04045));
}
