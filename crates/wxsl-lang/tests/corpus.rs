//! The lexer, run over every shader the standard library ships.
//!
//! Unit tests cover the cases I thought of; this one covers the cases the
//! library actually contains. It reads `wxsl-stdlib`'s shader tree by
//! relative path rather than by a dependency: `wxsl-stdlib` ships shader
//! source and node descriptors and does not compile anything itself, so it
//! has no reason to depend on this crate, and a dev-dependency the other
//! way would leave the two able to drift silently.
//!
//! Skips itself if the tree is not where it expects, so this cannot fail for
//! someone building the crate in isolation.

use std::path::{Path, PathBuf};

use wxsl_lang::compile::compile;
use wxsl_lang::cond::{Bindings, Value};
use wxsl_lang::emit::emit;
use wxsl_lang::lexer::tokenize;
use wxsl_lang::parse::parse;
use wxsl_lang::resolve::Modules;
use wxsl_lang::token::Tok;

fn shader_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("wxsl-stdlib")
        .join("shaders");
    root.is_dir().then_some(root)
}

fn shader_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).expect("readable directory") {
            let path = entry.expect("readable entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "wxsl")
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

#[test]
fn every_shipped_shader_lexes() {
    let Some(root) = shader_root() else {
        eprintln!("skipping: wxsl-stdlib/shaders not found");
        return;
    };
    let files = shader_files(&root);
    assert!(
        files.len() >= 30,
        "expected the shader tree, found {files:?}"
    );

    let mut tokens_seen = 0usize;
    for path in &files {
        let source = std::fs::read_to_string(path).expect("readable shader");
        match tokenize(&source) {
            Ok(tokens) => {
                assert!(!tokens.is_empty(), "{} produced no tokens", path.display());
                tokens_seen += tokens.len();
            }
            Err(diagnostics) => panic!(
                "{} failed to lex:\n{}",
                path.display(),
                diagnostics.render(&|_| Some(source.clone()))
            ),
        }
    }
    eprintln!("lexed {} files, {tokens_seen} tokens", files.len());
}

#[test]
fn the_corpus_exercises_both_readings_of_an_angle_bracket() {
    // A guard against the corpus test passing vacuously: the shader tree has
    // to contain both a real template list and a real comparison, or it is
    // not testing the disambiguation pass at all.
    let Some(root) = shader_root() else {
        eprintln!("skipping: wxsl-stdlib/shaders not found");
        return;
    };

    let mut templates = 0usize;
    let mut comparisons = 0usize;
    for path in shader_files(&root) {
        let source = std::fs::read_to_string(&path).expect("readable shader");
        for token in tokenize(&source).expect("lexes") {
            match token.node {
                Tok::TemplateOpen => templates += 1,
                Tok::Lt | Tok::Le | Tok::Gt | Tok::Ge => comparisons += 1,
                _ => {}
            }
        }
    }
    assert!(templates > 0, "no template lists in the corpus");
    assert!(comparisons > 0, "no comparisons in the corpus");
    eprintln!("{templates} template lists, {comparisons} comparisons");
}

#[test]
fn every_shipped_shader_parses() {
    // The unit tests cover the grammar cases I thought to write; this covers
    // the ones the library actually contains. It is the acceptance test for
    // the grammar.
    let Some(root) = shader_root() else {
        eprintln!("skipping: wxsl-stdlib/shaders not found");
        return;
    };
    let files = shader_files(&root);
    assert!(
        files.len() >= 30,
        "expected the shader tree, found {files:?}"
    );

    let mut declarations = 0usize;
    let mut imports = 0usize;
    let mut functions = 0usize;
    for path in &files {
        let source = std::fs::read_to_string(path).expect("readable shader");
        match parse(&source) {
            Ok(module) => {
                declarations += module.declarations.len();
                imports += module.imports.len();
                functions += module
                    .declarations
                    .iter()
                    .filter(|d| matches!(d, wxsl_lang::ast::Declaration::Function(_)))
                    .count();
            }
            Err(diagnostics) => panic!(
                "{} failed to parse:
{}",
                path.display(),
                diagnostics.render(&|_| Some(source.clone()))
            ),
        }
    }
    // Non-vacuous: the tree really does contain the constructs we care about.
    assert!(functions >= 40, "only {functions} functions parsed");
    assert!(imports >= 20, "only {imports} imports parsed");
    eprintln!(
        "parsed {} files: {declarations} declarations ({functions} functions), {imports} imports",
        files.len()
    );
}

#[test]
fn every_shipped_shader_round_trips() {
    // parse -> emit -> parse -> emit must reach a fixed point. The first
    // emit normalizes formatting; if the parser drops a construct or the
    // emitter renders something it cannot read back, the second emit
    // differs. Run over the whole library, this is the strongest statement
    // available that the front end is lossless.
    let Some(root) = shader_root() else {
        eprintln!("skipping: wxsl-stdlib/shaders not found");
        return;
    };

    let mut bytes = 0usize;
    for path in shader_files(&root) {
        let source = std::fs::read_to_string(&path).expect("readable shader");
        let once = emit(&parse(&source).expect("parses"));
        let twice = match parse(&once) {
            Ok(module) => emit(&module),
            Err(diagnostics) => panic!(
                "{}: emitted text does not parse back:
{}
--- emitted ---
{once}",
                path.display(),
                diagnostics.render(&|_| Some(once.clone()))
            ),
        };
        assert_eq!(
            once,
            twice,
            "{} is not a round-trip fixed point",
            path.display()
        );
        bytes += once.len();
    }
    eprintln!("round-tripped {bytes} bytes of emitted source");
}

/// The shipped shader tree, keyed by module path the way an application
/// supplies it.
fn library(root: &Path) -> Modules {
    let mut modules = Modules::new();
    for path in shader_files(root) {
        let relative = path
            .strip_prefix(root)
            .expect("under the shader root")
            .with_extension("")
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/")
            .replace('/', "::");
        let source = std::fs::read_to_string(&path).expect("readable shader");
        modules.insert(format!("package::{relative}"), source);
    }
    modules
}

#[test]
fn the_deferred_lighting_pass_compiles_to_wgsl() {
    // A real root module from the library: its own entry points, six levels
    // of imports underneath it, and `@if`-gated macros. This is the closest
    // thing to the renderer's own job that can be tested without a GPU.
    let Some(root) = shader_root() else {
        eprintln!("skipping: wxsl-stdlib/shaders not found");
        return;
    };
    let modules = library(&root);
    let entry = "package::wxsl::lighting_pass";
    assert!(
        modules.get(entry).is_some(),
        "lighting pass is in the library"
    );

    let wgsl = match compile(&modules, entry, &Bindings::new()) {
        Ok(wgsl) => wgsl,
        Err(diagnostics) => panic!(
            "{}",
            diagnostics.render(&|path| modules.get(path).map(str::to_string))
        ),
    };

    // Nothing WXSL-only survived.
    assert!(
        !wgsl.contains("import "),
        "imports left:
{wgsl}"
    );
    assert!(!wgsl.contains("@if"), "conditionals left");
    assert!(!wgsl.contains("@macro"), "macro markers left");
    // The entry points kept their names -- the renderer looks them up.
    assert!(wgsl.contains("fn lighting_vs("), "vertex entry missing");
    assert!(wgsl.contains("fn lighting_fs("), "fragment entry missing");
    // Imported functions were inlined under mangled names.
    assert!(
        wgsl.contains("fn package_wxsl_shading_shade_surface("),
        "shade_surface missing"
    );
    assert!(
        wgsl.contains("@group(3) @binding(0)"),
        "g-buffer bindings missing"
    );
    eprintln!("lighting pass -> {} bytes of WGSL", wgsl.len());
}

#[test]
fn macro_bindings_change_the_compiled_lighting_pass() {
    let Some(root) = shader_root() else {
        eprintln!("skipping: wxsl-stdlib/shaders not found");
        return;
    };
    let modules = library(&root);
    let entry = "package::wxsl::lighting_pass";

    let default = compile(&modules, entry, &Bindings::new()).expect("compiles");
    // Tonemapping on by default; turning it off must remove the call.
    assert!(
        default.contains("tonemap_filmic"),
        "expected the tonemap by default"
    );

    let mut off = Bindings::new();
    off.insert("wxsl_tonemap".to_string(), Value::Bool(false));
    let without = compile(&modules, entry, &off).expect("compiles");
    assert!(
        !without.contains("tonemap_filmic"),
        "the tonemap survived being switched off"
    );

    // Debug normals replaces the whole lighting body, so the light loop goes.
    let mut debug = Bindings::new();
    debug.insert("wxsl_debug_normals".to_string(), Value::Bool(true));
    let normals = compile(&modules, entry, &debug).expect("compiles");
    assert!(
        !normals.contains("sample_light"),
        "debug normals should not light anything"
    );
    assert!(default.contains("sample_light"), "the default path lights");
}

/// A template that uses `components(T)`, for the test below.
const RAW_TEMPLATE: &str = r#"
fn widen<T: f32 | vec3f>(v: T, k: f32) -> T {
    let lanes = f32(components(T)) * k;
    return v * T(lanes);
}
"#;

/// A root that calls that template at two types, beside a real stdlib
/// template, for the test below — so both a template written here and one
/// the library ships have to instantiate in the same compilation.
const RAW_ROOT: &str = r#"
import package::demo::widen::widen;
import package::math::safe_normalize::safe_normalize;

@fragment
fn fs() -> @location(0) vec4f {
    let direction = safe_normalize<vec3f>(vec3f(1.0, 2.0, 3.0));
    let scaled = widen(direction, 0.5);
    let single = widen(0.25f, 2.0);
    return vec4f(scaled * single, 1.0);
}
"#;

#[test]
fn a_template_compiles_against_the_real_shader_library() {
    // The unit tests instantiate templates against sources written for the
    // occasion. This one puts a template in front of the shader tree the
    // project actually ships, so the pass has to survive real imports,
    // real macros and the ABI modules.
    let Some(root) = shader_root() else {
        eprintln!("skipping: wxsl-stdlib/shaders not found");
        return;
    };
    let mut modules = library(&root);
    modules.insert("package::demo::widen", RAW_TEMPLATE);
    modules.insert("package::demo::main", RAW_ROOT);

    let wgsl = match compile(&modules, "package::demo::main", &Bindings::new()) {
        Ok(wgsl) => wgsl,
        Err(diagnostics) => panic!(
            "{}",
            diagnostics.render(&|path| modules.get(path).map(str::to_string))
        ),
    };

    // Two instantiations, named by origin and by type, and no template left.
    assert!(
        wgsl.contains("fn package_demo_widen_widen_f32(v: f32, k: f32) -> f32"),
        "{wgsl}"
    );
    assert!(
        wgsl.contains("fn package_demo_widen_widen_vec3f(v: vec3f, k: f32) -> vec3f"),
        "{wgsl}"
    );
    assert!(!wgsl.contains("widen<"), "{wgsl}");
    assert!(!wgsl.contains("components("), "{wgsl}");
    // `components(T)` folded to a literal in each copy.
    assert!(wgsl.contains("f32(1) * k"), "{wgsl}");
    assert!(wgsl.contains("f32(3) * k"), "{wgsl}");
    // And the library's own template instantiated beside it.
    assert!(
        wgsl.contains("fn package_math_safe_normalize_safe_normalize_vec3f("),
        "{wgsl}"
    );
}
