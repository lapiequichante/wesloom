// Linear light to sRGB-encoded values, per the sRGB transfer function.
//
// Needed when writing to a texture whose format is *not* `*Srgb`: the GPU
// then does no encoding, and handing it linear values makes everything look
// washed out. Both branches of the piecewise curve are evaluated and blended
// with `select`, which is cheaper than a divergent branch per channel.
fn linear_to_srgb(color: vec3f) -> vec3f {
    let low = color * 12.92;
    let high = pow(max(color, vec3f(0.0, 0.0, 0.0)), vec3f(1.0 / 2.4, 1.0 / 2.4, 1.0 / 2.4))
        * 1.055
        - vec3f(0.055, 0.055, 0.055);
    return select(low, high, color > vec3f(0.0031308, 0.0031308, 0.0031308));
}
