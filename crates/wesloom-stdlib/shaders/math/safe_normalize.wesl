// Normalize a vector, returning zero instead of NaN for a zero-length input.
//
// `normalize` on a zero vector is a division by zero; the NaN then spreads
// through everything downstream and shows up as black or missing pixels far
// from the node that caused it.
fn safe_normalize(v: vec3f) -> vec3f {
    let length_squared = dot(v, v);
    if length_squared < 1e-12 {
        return vec3f(0.0);
    }
    return v * inverseSqrt(length_squared);
}
