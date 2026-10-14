//! The GLSL ES 3.00 column of the corpus gate, with no device (ADR 0075).
//!
//! Compute is allowed; WebGL2 and GL are the fallback. What a fallback
//! device builds — every material stage the stock pipelines draw, the
//! lighting pass, every effect that is not native-only, the fragment arms
//! that stand in for compute, the editor's UI pass — must translate to
//! GLSL ES 3.00 through naga, the translator wgpu's GL backend runs. Every
//! node in the library is swept the same way in `graph_to_wgsl.rs`.
//!
//! And the column keeps the declarations honest the other way round: an
//! effect that names itself native-only must really fail — otherwise it
//! should be promoted, not left refused.

use std::borrow::Cow;

use wxsl::core::abi;
use wxsl::core::codegen;
use wxsl::core::graph::Graph;
use wxsl::core::lighting::{LightingSet, DEFAULT_MODELS};
use wxsl::core::macros::MacroSet;
use wxsl::render::effect::{
    Downlevel, Effect, EffectRegistry, BRDF_LUT, LUT_VIEW, RAMP_FILL, RAMP_VIEW,
};
use wxsl::render::material::Material;
use wxsl::render::variants;

mod glsl_es;

/// Compile `material`'s `stage` under the WebGL2 renderer's pins.
fn material_wgsl(material: &Material, stage: abi::MaterialStage) -> String {
    let shader = material.shader(stage);
    let mut macros = shader.macros.clone();
    macros.overlay(&glsl_es::webgl2_macros());
    let extra = [(
        codegen::MATERIAL_MODULE,
        Cow::Borrowed(shader.source.as_str()),
    )];
    variants::compile(
        &wxsl::stdlib_library(),
        &extra,
        codegen::MATERIAL_MODULE,
        &macros,
    )
    .unwrap_or_else(|error| panic!("{stage}: {error}"))
}

/// Compile `effect`'s module, as a renderer would for `set`.
fn effect_wgsl(effect: &Effect, set: &LightingSet) -> String {
    let (path, source) = effect.module_source(set, &[]);
    let extra = [(path, source)];
    let mut macros = effect.shader_macros(&MacroSet::new());
    macros.overlay(&glsl_es::webgl2_macros());
    variants::compile(&wxsl::stdlib_library(), &extra, path, &macros)
        .unwrap_or_else(|error| panic!("{}: {error}", effect.id))
}

fn demo_material() -> Material {
    let graph: Graph = serde_json::from_str(include_str!("../assets/pbr_cube.wxsl.json"))
        .expect("the demo graph parses");
    Material::from_graph(&graph, &wxsl::stdlib::registry()).expect("the demo graph compiles")
}

/// Every model enabled at once: the widest lighting pass there is.
fn every_model() -> LightingSet {
    LightingSet::new(DEFAULT_MODELS.iter().copied()).expect("the shipped models form a set")
}

#[test]
fn every_material_stage_translates() {
    // The demo graph declares a per-instance attribute, so the material's
    // own instance array is in here too, in its texel shape.
    let material = demo_material();
    let failures: Vec<String> = abi::MaterialStage::ALL
        .iter()
        .copied()
        .flat_map(|stage| glsl_es::translate(&format!("{stage}"), &material_wgsl(&material, stage)))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn the_lighting_pass_translates_for_every_model() {
    for set in [LightingSet::default(), every_model()] {
        let wgsl = effect_wgsl(&wxsl::render::effect::DEFERRED_LIGHTING, &set);
        let failures = glsl_es::translate(&format!("lighting ({})", set.signature()), &wgsl);
        assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    }
}

#[test]
fn every_effect_runs_what_its_downlevel_declaration_says() {
    let set = LightingSet::default();
    let effects = EffectRegistry::shipped()
        .with(BRDF_LUT)
        .with(LUT_VIEW)
        .with(RAMP_FILL)
        .with(RAMP_VIEW);
    let mut failures = Vec::new();
    for effect in effects.iter() {
        match &effect.downlevel {
            Downlevel::Portable => {
                assert!(
                    !effect.is_compute(),
                    "{} is compute and portable",
                    effect.id
                );
                failures.extend(glsl_es::translate(effect.id, &effect_wgsl(effect, &set)));
            }
            Downlevel::Fragment { .. } => {
                let arm = effect.on(&wxsl::render::Profile::WEBGL2);
                assert!(
                    !arm.is_compute(),
                    "{}'s fragment arm is a screen effect",
                    effect.id
                );
                failures.extend(glsl_es::translate(
                    &format!("{} (fragment arm)", effect.id),
                    &effect_wgsl(&arm, &set),
                ));
            }
            Downlevel::NativeOnly => assert!(
                !glsl_es::translate(effect.id, &effect_wgsl(effect, &set)).is_empty(),
                "`{}` translates to GLSL ES 3.00: declare it `Downlevel::Portable`",
                effect.id
            ),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn a_bake_s_generated_fragment_arm_translates() {
    let graph: Graph = serde_json::from_str(include_str!("../assets/bake_term.wxsl.json"))
        .expect("the bake material parses");
    let effect = Effect::from_bake(
        "demo.bake",
        "bake",
        "The demo bake.",
        &graph,
        "roughness_bake",
        &MacroSet::new(),
        &wxsl::stdlib::registry(),
    )
    .expect("the bake generates");
    let arm = effect.on(&wxsl::render::Profile::WEBGL2);
    let failures = glsl_es::translate(
        "bake (fragment arm)",
        &effect_wgsl(&arm, &LightingSet::default()),
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn the_editor_s_ui_pass_translates() {
    // The editor draws itself (ADR 0013), so a WebGL2 editor is this pass
    // plus the CPU MSDF generator; the GPU one is compute and stays native.
    let wgsl = variants::compile(
        &wxsl::stdlib_library(),
        &[],
        abi::UI_MODULE,
        &MacroSet::new(),
    )
    .expect("the UI pass compiles");
    let failures = glsl_es::translate("ui", &wgsl);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
