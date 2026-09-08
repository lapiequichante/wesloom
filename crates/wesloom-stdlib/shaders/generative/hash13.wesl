// A 3-in, 1-out hash: a deterministic pseudo-random value in 0..1 for a
// lattice point.
//
// Integer-free on purpose: `sin`-based hashes are the usual trick because
// they need no bit operations, but the classic `fract(sin(dot(p, k)) * n)`
// form bands badly at large coordinates. This mixes twice and folds the
// coordinate down first, which keeps it usable over the range a world-space
// noise field actually sees. Our own construction; the technique (multiply,
// take the fractional part, feed it back) is folklore.
fn hash13(p: vec3f) -> f32 {
    // Fold to a bounded range so the sine argument stays well-conditioned.
    var q = fract(p * 0.3183099 + vec3f(0.1, 0.2, 0.3));
    q = q * (q + vec3f(33.33, 33.33, 33.33));
    q = q * (q.yzx + vec3f(17.17, 17.17, 17.17));
    return fract(q.x + q.y * q.z);
}
