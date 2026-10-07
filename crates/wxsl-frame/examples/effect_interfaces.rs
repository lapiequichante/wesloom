//! Inspect shared effect interfaces without a renderer or GPU (ADR 0048).

use wxsl_frame::effect::{EffectRegistry, BRDF_LUT, LUT_VIEW, RAMP_FILL, RAMP_VIEW};

fn main() {
    let effects = EffectRegistry::shipped()
        .with(BRDF_LUT)
        .with(LUT_VIEW)
        .with(RAMP_FILL)
        .with(RAMP_VIEW);

    for effect in effects.iter() {
        let layout = effect.param_layout();
        println!(
            "{}: {} inputs, {} outputs, {} parameter bytes",
            effect.id,
            effect.inputs.len(),
            effect.outputs.len(),
            layout.size(),
        );
        if !layout.is_empty() {
            println!("{}", effect.params_header());
        }
    }
}
