// Luminance-preserving Reinhard tonemap.
//
// Compresses on luminance rather than per channel, so a bright saturated
// colour desaturates far less than the naive `c / (1 + c)` form.
import package::color::luminance::luminance;

fn tonemap_reinhard(color: vec3f) -> vec3f {
    let level = luminance(color);
    let compressed = level / (1.0 + level);
    // Rescale the original hue to the compressed luminance.
    return color * (compressed / max(level, 1e-5));
}
