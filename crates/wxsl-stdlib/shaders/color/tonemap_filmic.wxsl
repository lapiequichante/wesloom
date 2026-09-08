// A filmic tonemap: gentle toe, long shoulder, slight highlight desaturation.
//
// Our own curve, not a fit of anyone else's. It is a rational function whose
// coefficients were chosen so that (a) the toe is close to linear for small
// values, so dark detail is not crushed, (b) it maps 1.0 to about 0.8, so
// there is visible headroom above "white", and (c) it approaches 1.0
// asymptotically, so nothing clips. Highlights are pulled towards their
// luminance as they compress, which is what film does to saturated colour.
import package::color::luminance::luminance;

fn tonemap_filmic(color: vec3f) -> vec3f {
    let x = max(color, vec3f(0.0, 0.0, 0.0));
    let toe = x * (x + vec3f(0.28, 0.28, 0.28));
    let shoulder = x * (x + vec3f(0.66, 0.66, 0.66)) + vec3f(0.14, 0.14, 0.14);
    let mapped = toe / shoulder;

    // Desaturate towards luminance in proportion to how hard the highlight
    // was compressed.
    let level = luminance(mapped);
    let compression = saturate(luminance(x) - 1.0);
    return mix(mapped, vec3f(level, level, level), compression * 0.35);
}
