//! The lighting models this crate ships against the registry that names
//! them.
//!
//! `wxsl_core::lighting` owns the registry *entries* — ids, names, module
//! paths, the extra-target contract — while this crate owns the *sources*
//! those entries point at. The two live in different crates on purpose
//! (ADR 0002), so the agreement between them is checked here, by compiling
//! each model and by compiling the generated lighting pass against the
//! shipped library, for a few telling sets.

use wxsl_core::lighting::{self, Dispatch, LightingSet};

/// The shipped sources, as the compiler sees them.
fn library() -> wxsl_lang::Modules {
    let mut modules = wxsl_lang::Modules::new();
    for (path, source) in wxsl_stdlib::MODULES {
        modules.insert(*path, *source);
    }
    modules
}

/// Compile `root` against the shipped sources, with `flags` bound.
fn compile(root: &str, flags: &[(&str, bool)]) -> Result<String, String> {
    let modules = library();
    let mut bindings = wxsl_lang::Bindings::new();
    for (name, value) in flags {
        bindings.insert((*name).to_string(), wxsl_lang::Value::Bool(*value));
    }
    wxsl_lang::compile(&modules, root, &bindings)
        .map_err(|diagnostics| diagnostics.render(&|path| modules.get(path).map(str::to_string)))
}

/// Mount a generated piece — source plus its import lines, exactly as a
/// generated material module is assembled — and compile it as a root.
fn compile_generated(name: &str, generated: &lighting::GeneratedLighting) {
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
    wxsl_lang::compile(
        &modules,
        "package::test::generated",
        &wxsl_lang::Bindings::new(),
    )
    .unwrap_or_else(|diagnostics| {
        panic!(
            "{name} does not compile:\n{}",
            diagnostics.render(&|path| modules.get(path).map(str::to_string))
        )
    });
}

fn model(name: &str) -> lighting::LightingModel {
    *lighting::DEFAULT_MODELS
        .iter()
        .find(|model| model.name == name)
        .unwrap_or_else(|| panic!("no shipped model named {name}"))
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

/// The two shapes of generated shading — one model called directly, a set
/// switched over the id — compile against the real library. These are the
/// shapes a forward module and a dispatching lighting pass contain.
#[test]
fn the_generated_shading_compiles_for_one_model_and_for_a_set() {
    compile_generated(
        "direct pbr",
        &lighting::shade_surface(&Dispatch::Direct(model("pbr"))),
    );
    compile_generated(
        "direct clearcoat",
        &lighting::shade_surface(&Dispatch::Direct(model("clearcoat"))),
    );
    compile_generated(
        "switch over everything",
        &lighting::shade_surface(&Dispatch::Switch(lighting::default_set().unwrap())),
    );
}

/// The generated lighting pass compiles against the shipped library, with
/// every macro bound (the strictest combination — the shadow lookup and
/// the tonemap are both in), for a single-model set, the full set, and a
/// partial set whose layout carries an extra target.
#[test]
fn the_generated_lighting_pass_compiles_for_telling_sets() {
    let sets: Vec<(&str, LightingSet)> = vec![
        ("single pbr", LightingSet::single(model("pbr"))),
        ("everything", lighting::default_set().unwrap()),
        (
            "lambert + clearcoat",
            LightingSet::new([model("lambert"), model("clearcoat")]).unwrap(),
        ),
    ];
    for (name, set) in sets {
        let source = lighting::lighting_pass_source(&set);
        // The pass reads the G-buffer through the pass group and nothing
        // else: no frame group, no material group, never the user's.
        assert!(source.contains("@group(3)"));
        assert!(!source.contains("@group(0)"));
        assert!(!source.contains("@group(1)"));
        assert!(!source.contains("@group(2)"));
        let mut modules = library();
        let root = wxsl_core::abi::LIGHTING_PASS_MODULE;
        modules.insert(root, source);
        let mut bindings = wxsl_lang::Bindings::new();
        for (flag, value) in [
            ("wxsl_receive_shadows", true),
            ("wxsl_tonemap", true),
            ("wxsl_debug_normals", false),
            ("wxsl_relative_to_eye", false),
            ("wxsl_previous_frame", false),
        ] {
            bindings.insert(flag.to_string(), wxsl_lang::Value::Bool(value));
        }
        wxsl_lang::compile(&modules, root, &bindings).unwrap_or_else(|diagnostics| {
            panic!(
                "{name} lighting pass does not compile:\n{}",
                diagnostics.render(&|path| modules.get(path).map(str::to_string))
            )
        });
    }
}
