// Fractal Brownian motion over `value_noise3`: octaves of noise at rising
// frequency and falling amplitude, normalized back to 0..1.
//
// This function is where macro variables earn their keep. The octave count
// is a loop bound, so it *must* be a compile-time constant — it cannot be a
// socket carrying a runtime value. It arrives as
// `WESLOOM_FBM_OCTAVES`, declared by the `generative.fbm3` node definition
// and imported from the generated macro module, so changing it in the node
// format recompiles this loop rather than branching at runtime.
//
// `wesloom_fbm_ridged` is a flag macro: with it enabled the octaves are
// folded into ridges (`1 - |2n - 1|`), which turns soft billows into the
// creased look wanted for rock and cloth. Conditional translation drops the
// unused arm entirely, so neither variant pays for the other.

import package::generative::value_noise3::value_noise3;
import package::wesloom::macros::WESLOOM_FBM_OCTAVES;

fn fbm3(p: vec3f, lacunarity: f32, gain: f32) -> f32 {
    var position = p;
    var amplitude = 1.0;
    var total = 0.0;
    var normalization = 0.0;

    for (var octave = 0i; octave < WESLOOM_FBM_OCTAVES; octave = octave + 1i) {
        var sample = value_noise3(position);
        @if(wesloom_fbm_ridged)
        sample = 1.0 - abs(sample * 2.0 - 1.0);

        total = total + sample * amplitude;
        normalization = normalization + amplitude;
        position = position * lacunarity;
        amplitude = amplitude * gain;
    }

    // A zero octave count is legal and means "no detail"; return the mid
    // value rather than dividing by zero.
    if normalization <= 0.0 {
        return 0.5;
    }
    return total / normalization;
}
