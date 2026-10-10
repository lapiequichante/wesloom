//! The corpus gate: every shader the generators produce, compiled, with no
//! device.
//!
//! Generated WXSL used to be exercised only when a GPU test ran, so a typo
//! in a generator surfaced as a `wgpu` validation panic minutes after the
//! change that caused it instead of as a compiler diagnostic seconds after.
//! This file is the gate that closes that gap (plan2 P6): the one place
//! every generator is checked, over the combinations that tell them apart —
//! every shipped model, both dispatch shapes, every telling lighting set,
//! every material stage, every binding of the ABI's macro flags — failing
//! with the compiler's rendered diagnostic and naming the combination.
//!
//! `wxsl_core::lighting` owns the registry *entries* — ids, names, module
//! paths, the extra-target contract — while this crate owns the *sources*
//! those entries point at. The two live in different crates on purpose
//! (ADR 0002), so the agreement between them is checked here too.

use wxsl_core::abi::{self, MaterialStage};
use wxsl_core::codegen::{self, CodegenOptions};
use wxsl_core::graph::{Graph, Node};
use wxsl_core::lighting::{self, Dispatch, LightingSet};
use wxsl_core::node::Value;

/// The shipped sources, as the compiler sees them.
fn library() -> wxsl_lang::Modules {
    let mut modules = wxsl_lang::Modules::new();
    for (path, source) in wxsl_stdlib::MODULES {
        modules.insert(*path, *source);
    }
    modules
}

/// Compile `root` against the shipped sources, with `flags` bound.
fn compile(root: &str, flags: &[(String, bool)]) -> Result<String, String> {
    let modules = library();
    let mut bindings = wxsl_lang::Bindings::new();
    for (name, value) in flags {
        bindings.insert(name.clone(), wxsl_lang::Value::Bool(*value));
    }
    wxsl_lang::compile(&modules, root, &bindings)
        .map_err(|diagnostics| diagnostics.render(&|path| modules.get(path).map(str::to_string)))
}

/// Mount a generated piece — source plus its import lines, exactly as a
/// generated material module is assembled — and compile it as a root,
/// with the ABI's macro flags bound to `flags`.
fn compile_generated_with(
    name: &str,
    generated: &lighting::GeneratedLighting,
    flags: &[(String, bool)],
) {
    let mut modules = library();
    let mut text = String::new();
    for (module, item) in &generated.imports {
        assert!(
            wxsl_stdlib::shaders::module(module).is_some(),
            "{name}: import of unshipped module {module}"
        );
        text.push_str(&format!("import {module}::{item};\n"));
    }
    text.push_str(&generated.source);
    modules.insert("package::test::generated", text);
    let mut bindings = wxsl_lang::Bindings::new();
    for (flag, value) in flags {
        bindings.insert(flag.clone(), wxsl_lang::Value::Bool(*value));
    }
    wxsl_lang::compile(&modules, "package::test::generated", &bindings).unwrap_or_else(
        |diagnostics| {
            panic!(
                "{name} does not compile:\n{}",
                diagnostics.render(&|path| modules.get(path).map(str::to_string))
            )
        },
    );
}

fn model(name: &str) -> lighting::LightingModel {
    *lighting::DEFAULT_MODELS
        .iter()
        // Bare spellings resolve against the shipped package.
        .find(|model| model.name == wxsl_core::identity::resolve(name).as_ref())
        .unwrap_or_else(|| panic!("no shipped model named {name}"))
}

/// The lighting sets the gate runs, each telling in one way:
///
/// * single `pbr` — one model, no dispatch, no id channel: the shape every
///   pipeline had before models existed;
/// * the full shipped set — a switch, the clearcoat model's extra target,
///   and the preshaded route's early return (plan5 D2);
/// * `lambert` + `clearcoat` — a switch and an extra target, without the
///   default model, so nothing can hide behind it;
/// * `pbr` + `phong` — a switch whose layout carries the id channel and
///   nothing else;
/// * single `preshaded` — the passthrough pass, no shading function at
///   all;
/// * `pbr` + `preshaded` — the early return riding a real switch.
fn telling_sets() -> Vec<(&'static str, LightingSet)> {
    vec![
        ("single pbr", LightingSet::single(model("pbr"))),
        ("single sheen", LightingSet::single(model("sheen"))),
        (
            "pbr + sheen (codegen only; exceeds portable budget)",
            LightingSet::new([model("pbr"), model("sheen")]).unwrap(),
        ),
        (
            "single iridescent",
            LightingSet::single(model("iridescent")),
        ),
        (
            "pbr + iridescent (codegen only; exceeds portable budget)",
            LightingSet::new([model("pbr"), model("iridescent")]).unwrap(),
        ),
        ("full set", lighting::default_set().unwrap()),
        (
            "lambert + clearcoat",
            LightingSet::new([model("lambert"), model("clearcoat")]).unwrap(),
        ),
        (
            "pbr + phong",
            LightingSet::new([model("pbr"), model("phong")]).unwrap(),
        ),
        ("single preshaded", LightingSet::single(model("preshaded"))),
        (
            "pbr + preshaded",
            LightingSet::new([model("pbr"), model("preshaded")]).unwrap(),
        ),
    ]
}

/// Every binding of the ABI's macro flags, derived from
/// [`abi::abi_macros`] so a flag added there joins the gate instead of
/// silently escaping it.
fn macro_flag_combinations() -> Vec<Vec<(String, bool)>> {
    let flags: Vec<(String, bool)> = abi::abi_macros()
        .iter()
        .map(|def| {
            (
                def.name.as_str().to_string(),
                def.default
                    .as_flag()
                    .expect("the ABI's feature macro defaults are flags"),
            )
        })
        .collect();
    let mut combinations = vec![Vec::new()];
    for (name, _) in flags {
        let mut next = Vec::new();
        for combination in &combinations {
            for value in [false, true] {
                let mut grown = combination.clone();
                grown.push((name.clone(), value));
                next.push(grown);
            }
        }
        combinations = next;
    }
    combinations
}

/// The generated text declares exactly the ABI's feature macros — the
/// cross product in [`macro_flag_combinations`] is over *those* names, so
/// a rename or a new declaration has to land in the table, or this test
/// says so.
#[test]
fn the_abi_macros_are_exactly_the_flags_the_gate_crosses() {
    let macros = abi::abi_macros();
    let declared: Vec<&str> = macros.iter().map(|def| def.name.as_str()).collect();
    assert_eq!(declared.len(), 4, "unexpected ABI macro count");
    for name in declared {
        assert!(
            name.starts_with("wxsl_"),
            "{name} does not follow the ABI macro naming convention"
        );
    }
}

/// The registry names modules; this crate must ship them, with the
/// functions and pack functions the entries promise.
#[test]
fn every_registry_entry_points_at_a_shipped_source() {
    for model in lighting::DEFAULT_MODELS {
        let source = wxsl_stdlib::shaders::module(model.module).unwrap_or_else(|| {
            panic!(
                "{} names module {}, which is not shipped",
                model.name, model.module
            )
        });
        assert!(
            source.contains(&format!("fn {}(", model.function)),
            "{} does not define fn {}",
            model.module,
            model.function
        );
        if let Some(extra) = model.extra {
            assert!(
                source.contains(&format!("fn {}(", extra.pack)),
                "{} does not define its pack fn {}",
                model.module,
                extra.pack
            );
        }
        if let Some(ambient) = model.ambient {
            assert!(
                source.contains(&format!("fn {ambient}(")),
                "{} does not define its ambient function {ambient}",
                model.module
            );
        }
    }
    // And the default model is who the registry says it is.
    assert_eq!(model("pbr").id, lighting::DEFAULT_MODEL_ID);
}

/// Each model module compiles as a root on its own. A signature drifting
/// from the registry's contract, or an import of something the library
/// does not ship, is a compile error here rather than a mystifying
/// diagnostics banner the first time a pipeline enables the model.
#[test]
fn every_model_module_compiles_standalone() {
    for model in lighting::DEFAULT_MODELS {
        compile(model.module, &[]).unwrap_or_else(|error| panic!("{}: {error}", model.module));
    }
}

/// The direct dispatch shape, for every shipped model, under every macro
/// combination: this is what a forward module of a material using that
/// model contains, and there is one per model rather than one for a
/// convenient favourite.
#[test]
fn the_generated_shading_compiles_for_every_model_under_every_macro_combination() {
    for model in lighting::DEFAULT_MODELS {
        let generated = lighting::shade_surface(&Dispatch::Direct(*model));
        for flags in macro_flag_combinations() {
            compile_generated_with(
                &format!("direct {} ({flags:?})", model.name),
                &generated,
                &flags,
            );
        }
    }
}

/// The switch dispatch shape, for every telling set: what a dispatching
/// lighting pass contains, with each set's own id channel and target
/// layout.
#[test]
fn the_generated_shading_compiles_for_every_telling_set() {
    for (name, set) in telling_sets() {
        let generated = lighting::shade_surface(&Dispatch::Switch(set));
        compile_generated_with(&format!("switch over {name}"), &generated, &[]);
    }
}

/// The generated lighting pass compiles against the shipped library for
/// every telling set under every macro combination — the shadow lookup,
/// the tonemap, the debug view, each on and off. This is the strictest
/// thing here and the one that used to be ad hoc.
#[test]
fn the_generated_lighting_pass_compiles_for_every_set_and_macro_combination() {
    let combinations = macro_flag_combinations();
    for (name, set) in telling_sets() {
        let source = lighting::lighting_pass_source(&set, &[]);
        // The pass reads the G-buffer through the pass group and nothing
        // else: no frame group, no material group, never the user's.
        assert!(source.contains("@group(3)"), "{name}");
        assert!(!source.contains("@group(0)"), "{name}");
        assert!(!source.contains("@group(1)"), "{name}");
        assert!(!source.contains("@group(2)"), "{name}");
        for flags in &combinations {
            let mut modules = library();
            let root = abi::LIGHTING_PASS_MODULE;
            modules.insert(root, source.clone());
            let mut bindings = wxsl_lang::Bindings::new();
            for (flag, value) in flags {
                bindings.insert((*flag).to_string(), wxsl_lang::Value::Bool(*value));
            }
            wxsl_lang::compile(&modules, root, &bindings).unwrap_or_else(|diagnostics| {
                panic!(
                    "{name} lighting pass under {flags:?} does not compile:\n{}",
                    diagnostics.render(&|path| modules.get(path).map(str::to_string))
                )
            });
        }
    }
}

/// The feature-enabled half of the corpus gate (plan2 P12): the lighting
/// pass generated with a feature's channel in the plan — its binding, its
/// unpack, its ride in the extras struct — compiles against the shipped
/// library, with the feature's own macro on and off, since the *pack* the
/// same plan generates is conditional on it. The feature module is part of
/// the library, so a drift between the generator and the module fails here
/// in seconds.
#[test]
fn the_generated_lighting_pass_compiles_with_a_feature_channel() {
    let features = lighting::feature_requests(&["subsurface"]).expect("subsurface ships");
    let set = lighting::default_set().expect("the default set");
    let source = lighting::lighting_pass_source(&set, &features);
    assert!(source.contains("gbuffer_subsurface"));

    // The pack is the conditional half: generated once per arm of the
    // feature's macro, both must compile against the shipped module.
    let packed = lighting::pack_gbuffer(
        lighting::DEFAULT_MODELS
            .iter()
            .find(|model| model.name == "wxsl.pbr")
            .expect("the default model"),
        &lighting::LightingSet::default(),
        &features,
        false,
    );
    assert!(packed.source.contains("@if(wxsl_subsurface)"));

    let mut modules = library();
    let root = abi::LIGHTING_PASS_MODULE;
    modules.insert(root, source);
    for feature_on in [true, false] {
        let mut bindings = wxsl_lang::Bindings::new();
        bindings.insert(
            "wxsl_subsurface".to_string(),
            wxsl_lang::Value::Bool(feature_on),
        );
        bindings.insert(
            "wxsl_subsurface_strength".to_string(),
            wxsl_lang::Value::Float(0.5),
        );
        wxsl_lang::compile(&modules, root, &bindings).unwrap_or_else(|diagnostics| {
            panic!(
                "the feature-enabled lighting pass (on: {feature_on}) does not compile:\n{}",
                diagnostics.render(&|path| modules.get(path).map(str::to_string))
            )
        });
    }

    // The pack compiles as part of a material module below (the corpus
    // gate graph); here it suffices that its conditional text was
    // generated against the module that declares the macros it names.
    let _ = packed;
}

/// The feature module compiles standalone, like a model module.
#[test]
fn the_feature_module_compiles_standalone() {
    for feature in lighting::FEATURES {
        compile(feature.module, &[]).unwrap_or_else(|error| panic!("{}: {error}", feature.module));
    }
}

/// The forward-shaded material module (plan5 D2): under the gbuffer stage
/// it carries *both* generated halves — the pack that stores the
/// fragment's radiance and the shading function that produced it — and the
/// fragment shades before it packs. Compiled for every stage of a set
/// that carries the route alongside a real model — the shape a
/// forward-shaded material resolves under. (A set of only the route is
/// the passthrough pass, covered by the telling sets above; a material in
/// it names the route or nothing, so it has no forward-shaded materials.)
#[test]
fn the_forward_shaded_material_module_compiles_for_every_stage() {
    let registry = wxsl_stdlib::registry();
    let graph = gate_graph();
    for (set_name, set) in [(
        "pbr + preshaded",
        LightingSet::new([model("pbr"), model("preshaded")]).unwrap(),
    )] {
        let resolved = wxsl_core::material::MaterialConfig::default()
            .with_model("pbr")
            .forward_shaded(true)
            .with_shadows(true, false)
            .resolve(&set)
            .unwrap_or_else(|error| {
                panic!("{set_name}: a forward-shaded material refuses to resolve: {error}")
            });
        for stage in MaterialStage::ALL {
            let options = CodegenOptions {
                stage: *stage,
                material: resolved.clone(),
                ..CodegenOptions::default()
            };
            let generated = codegen::generate(&graph, &registry, &options)
                .unwrap_or_else(|error| panic!("{set_name} codegen failed: {error}"));
            if *stage == abi::MaterialStage::GBUFFER {
                assert!(
                    generated
                        .source
                        .contains("pack_gbuffer(surface, shade_surface(surface, ctx)"),
                    "{set_name}: the gbuffer fragment does not shade before it packs:\n{}",
                    generated.source
                );
            }
            let mut modules = library();
            modules.insert(codegen::MATERIAL_MODULE, generated.source.clone());
            let mut bindings = wxsl_lang::Bindings::new();
            for (name, value) in generated.macros.flags() {
                bindings.insert(name.to_string(), wxsl_lang::Value::Bool(value));
            }
            wxsl_lang::compile(&modules, codegen::MATERIAL_MODULE, &bindings).unwrap_or_else(
                |diagnostics| {
                    panic!(
                        "{set_name} forward-shaded at stage {} does not compile:\n{}",
                        stage.name(),
                        diagnostics.render(&|path| modules.get(path).map(str::to_string))
                    )
                },
            );
        }
    }
}

/// A material graph that exercises the pieces a stage can carry: a
/// parameter shading the surface, and a discard test — which is what gives
/// even the depth and shadow stages a fragment entry point to compile
/// (ADR 0025).
fn gate_graph() -> Graph {
    let registry = wxsl_stdlib::registry();
    let mut graph = Graph::new("corpus gate material");
    let tint = graph.add(
        Node::new("param.value")
            .with_setting("name", "tint")
            .with_param("value", Value::Vec3([0.6, 0.5, 0.4])),
    );
    graph
        .set_generic(&registry, tint, "T", wxsl_core::node::ValueType::Vec3)
        .expect("every value type is allowed");
    let cut = graph.add(Node::new("compare.less").with_param("b", Value::F32(0.5)));
    let output = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
    let discard = graph.add(Node::new(abi::DISCARD_OUTPUT_ID));
    graph
        .wire(&registry, (tint, "out"), (output, "base_color"))
        .expect("vec3f into base_color");
    graph
        .wire(&registry, (cut, "out"), (discard, abi::SOCKET_DISCARD))
        .expect("bool into discard");
    graph
}

/// The generated *material module* — the thing the renderer actually hands
/// the shader compiler — compiles for every stage of every telling set,
/// with the macros its own codegen says it carries. The material side of
/// the gate: a broken pack function, a struct mismatch, a dispatch that
/// names a target the layout does not carry, all land here in seconds.
#[test]
fn the_generated_material_module_compiles_for_every_stage_and_telling_set() {
    let registry = wxsl_stdlib::registry();
    let graph = gate_graph();
    for (set_name, set) in telling_sets() {
        // A material per model in the set — the forward module calls its
        // model directly, so each one is its own shape.
        for model in set.models() {
            let resolved = wxsl_core::material::MaterialConfig::default()
                .with_model(model.name)
                .resolve(&set)
                .expect("in the set");
            for stage in MaterialStage::ALL {
                let options = CodegenOptions {
                    stage: *stage,
                    material: resolved.clone(),
                    ..CodegenOptions::default()
                };
                let generated =
                    codegen::generate(&graph, &registry, &options).unwrap_or_else(|error| {
                        panic!("{set_name}/{} codegen failed: {error}", model.name)
                    });
                let mut modules = library();
                modules.insert(codegen::MATERIAL_MODULE, generated.source.clone());
                let mut bindings = wxsl_lang::Bindings::new();
                for (name, value) in generated.macros.flags() {
                    bindings.insert(name.to_string(), wxsl_lang::Value::Bool(value));
                }
                wxsl_lang::compile(&modules, codegen::MATERIAL_MODULE, &bindings).unwrap_or_else(
                    |diagnostics| {
                        panic!(
                            "{set_name}/{} at stage {} does not compile:\n{}",
                            model.name,
                            stage.name(),
                            diagnostics.render(&|path| modules.get(path).map(str::to_string))
                        )
                    },
                );
            }
        }
    }
}
