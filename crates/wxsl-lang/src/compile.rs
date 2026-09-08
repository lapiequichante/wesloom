//! The compiler driver: WXSL sources in, WGSL out.
//!
//! The whole pipeline in one call. The passes run in an order that is not
//! negotiable, and the reasons are worth stating because getting them wrong
//! produces failures that look like something else:
//!
//! 1. **parse** every module reachable from the root.
//! 2. **conditional translation and macro binding**, per module, before any
//!    renaming — an `@if` names macros in its own module's vocabulary, and a
//!    branch that is dropped should never have its references resolved.
//! 3. **resolution**: mangle each imported declaration by origin, rewrite
//!    references (respecting shadowing), concatenate dependencies first.
//! 4. **dead-code elimination** from the root's declarations, so a library
//!    module's unused half cannot fail to compile for reasons the shader
//!    author never sees.
//! 5. **emit WGSL**, refusing anything that is still WXSL-only — which
//!    catches a skipped pass here rather than inside `wgpu`.
//!
//! Steps 2 to 4 live in [`mod@crate::resolve`], which owns the ordering.

use crate::cond::Bindings;
use crate::diagnostic::Diagnostics;
use crate::emit::{emit, emit_wgsl};
use crate::resolve::{resolve, Modules};

/// Compile `root` and its imports to WGSL.
pub fn compile(modules: &Modules, root: &str, bindings: &Bindings) -> Result<String, Diagnostics> {
    let module = resolve(modules, root, bindings)?;
    emit_wgsl(&module)
}

/// Compile as far as the flattened WXSL, without the WGSL check.
///
/// What `--dump-wxsl` prints: useful when a shader fails to compile and the
/// question is what the imports and conditionals actually produced.
pub fn compile_to_wxsl(
    modules: &Modules,
    root: &str,
    bindings: &Bindings,
) -> Result<String, Diagnostics> {
    Ok(emit(&resolve(modules, root, bindings)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cond::Value;

    fn library() -> Modules {
        let mut modules = Modules::new();
        modules.insert(
            "package::math::remap",
            "fn remap(value: f32, low: f32, high: f32) -> f32 {\n\
             \x20   return low + value * (high - low);\n}\n",
        );
        modules.insert(
            "package::noise::fbm",
            "@macro const OCTAVES: i32 = 5;\n\
             @macro const RIDGED: bool = false;\n\n\
             fn fbm(x: f32) -> f32 {\n\
             \x20   var total = 0.0;\n\
             \x20   for (var i = 0; i < OCTAVES; i++) {\n\
             \x20       total += x;\n\
             \x20   }\n\
             \x20   @if(RIDGED) total = 1.0 - abs(total);\n\
             \x20   return total;\n}\n",
        );
        modules
    }

    fn root(source: &str) -> Modules {
        let mut modules = library();
        modules.insert("package::main", source);
        modules
    }

    #[test]
    fn the_whole_pipeline_produces_wgsl() {
        let modules = root(
            "import package::math::remap::remap;\n\
             import package::noise::fbm::fbm;\n\n\
             @fragment\n\
             fn fs() -> @location(0) vec4f {\n\
             \x20   return vec4f(remap(fbm(0.5), 0.0, 1.0));\n}\n",
        );
        let wgsl = match compile(&modules, "package::main", &Bindings::new()) {
            Ok(wgsl) => wgsl,
            Err(diagnostics) => panic!(
                "{}",
                diagnostics.render(&|path| modules.get(path).map(str::to_string))
            ),
        };

        // No WXSL left.
        assert!(!wgsl.contains("import"), "{wgsl}");
        assert!(!wgsl.contains("@if"), "{wgsl}");
        assert!(!wgsl.contains("@macro"), "{wgsl}");
        // Dependencies inlined and mangled, entry point untouched.
        assert!(wgsl.contains("fn package_math_remap_remap"), "{wgsl}");
        assert!(wgsl.contains("fn package_noise_fbm_fbm"), "{wgsl}");
        assert!(wgsl.contains("@fragment\nfn fs()"), "{wgsl}");
        // The macro became an ordinary const at its default.
        assert!(
            wgsl.contains("const package_noise_fbm_OCTAVES: i32 = 5;"),
            "{wgsl}"
        );
        // The false conditional's statement is gone.
        assert!(!wgsl.contains("1.0 - abs(total)"), "{wgsl}");
    }

    #[test]
    fn bindings_change_the_output() {
        let modules = root(
            "import package::noise::fbm::fbm;\n\n\
             @fragment\nfn fs() -> @location(0) vec4f { return vec4f(fbm(0.5)); }\n",
        );
        let mut bindings = Bindings::new();
        bindings.insert("OCTAVES".to_string(), Value::Int(2));
        bindings.insert("RIDGED".to_string(), Value::Bool(true));

        let wgsl = compile(&modules, "package::main", &bindings).expect("compiles");
        assert!(
            wgsl.contains("const package_noise_fbm_OCTAVES: i32 = 2;"),
            "{wgsl}"
        );
        assert!(wgsl.contains("1.0 - abs(total)"), "{wgsl}");

        let default = compile(&modules, "package::main", &Bindings::new()).expect("compiles");
        assert_ne!(wgsl, default, "bindings must change the shader");
    }

    #[test]
    fn the_same_module_compiles_twice_with_different_bindings() {
        // The A/B comparison case: one source, two compilations, different
        // compile-time settings, no second material.
        let modules = root(
            "import package::noise::fbm::fbm;\n\n\
             @fragment\nfn fs() -> @location(0) vec4f { return vec4f(fbm(0.5)); }\n",
        );
        let with = |octaves: i64| {
            let mut bindings = Bindings::new();
            bindings.insert("OCTAVES".to_string(), Value::Int(octaves));
            compile(&modules, "package::main", &bindings).expect("compiles")
        };
        let three = with(3);
        let seven = with(7);
        assert!(three.contains("OCTAVES: i32 = 3;"), "{three}");
        assert!(seven.contains("OCTAVES: i32 = 7;"), "{seven}");
        assert_ne!(three, seven);
    }

    #[test]
    fn an_unused_import_does_not_reach_the_output() {
        let modules = root(
            "import package::math::remap::remap;\n\
             import package::noise::fbm::fbm;\n\n\
             @fragment\nfn fs() -> @location(0) vec4f { return vec4f(fbm(0.5)); }\n",
        );
        let wgsl = compile(&modules, "package::main", &Bindings::new()).expect("compiles");
        assert!(wgsl.contains("package_noise_fbm_fbm"), "{wgsl}");
        assert!(!wgsl.contains("remap"), "unused import survived:\n{wgsl}");
    }

    #[test]
    fn dumping_wxsl_keeps_the_flattened_form_readable() {
        let modules = root(
            "import package::noise::fbm::fbm;\n\n\
             @fragment\nfn fs() -> @location(0) vec4f { return vec4f(fbm(0.5)); }\n",
        );
        let wxsl = compile_to_wxsl(&modules, "package::main", &Bindings::new()).expect("compiles");
        // Same content as the WGSL for this input, but produced without the
        // backend's refusal check, so it also works on a tree that still has
        // WXSL constructs in it.
        assert!(wxsl.contains("fn package_noise_fbm_fbm"), "{wxsl}");
    }

    #[test]
    fn a_missing_module_names_the_importer_and_the_path() {
        let mut modules = Modules::new();
        modules.insert(
            "package::main",
            "import package::nowhere::thing;\nfn f() { }\n",
        );
        let error = compile(&modules, "package::main", &Bindings::new()).expect_err("missing");
        let rendered = error.render(&|path| modules.get(path).map(str::to_string));
        assert!(
            rendered.contains("no module `package::nowhere`"),
            "{rendered}"
        );
        assert!(rendered.contains("package::main"), "{rendered}");
    }

    #[test]
    fn a_root_that_is_not_in_the_module_set_is_an_error() {
        let error =
            compile(&Modules::new(), "package::absent", &Bindings::new()).expect_err("no root");
        assert!(error.to_string().contains("no module"), "{error}");
    }
}
