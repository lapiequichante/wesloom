//! The shader variant cache: WXSL in, `wgpu` shader modules out, compiled
//! once per (shader, macro set, stage).
//!
//! Asking a material for a different *stage* means asking for a different
//! variant; so does flipping a macro variable. Both go through
//! [`ShaderVariants::material`], which compiles on a miss and hands back a
//! cached module on a hit — the caller never has to know which happened
//! ([ADR 0005](../../../docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md),
//! [ADR 0022](../../../docs/adr/0022-material-stages-replace-the-render-path-enum.md)).
//!
//! The key holds the *stage*, not the pipeline, and that is what makes a
//! pipeline swap cheap: two pipelines that both want a `gbuffer` stage
//! share the compiled result, so swapping between them costs nothing the
//! second time.
//!
//! A macro is declared, with a default, by the WXSL module that uses it
//! ([ADR 0011](../../../docs/adr/0011-own-the-shading-language.md)); the
//! values here override those defaults. Binding a name no module declares is
//! ignored rather than an error, because one binding set is applied to every
//! module in a compilation. The flip side is that a typo in a macro name is
//! silently ineffective rather than reported.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use wxsl_core::abi::{self, MaterialStage};
use wxsl_core::codegen;
use wxsl_core::macros::{MacroSet, MacroValue};
use wxsl_core::wxsl::stable_hash;

use crate::error::RenderError;
use crate::library::ShaderLibrary;
use crate::material::Material;

/// Which shader a variant is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VariantKind {
    /// A material: the module generated from a graph, plus the ABI around it.
    Material,
    /// The deferred lighting pass, which has no material graph.
    LightingPass,
}

/// Identity of one compiled shader module.
///
/// `identity` folds in the generated source *and* the macro values: the
/// generated module declares its macros at their effective values, but the
/// bindings also reach *imported* modules, whose own defaults are not part
/// of the root source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VariantKey {
    /// Material or lighting pass.
    pub kind: VariantKind,
    /// Hash of the source and macro values.
    pub identity: u64,
    /// The stage the variant was compiled for, or `None` for a shader that
    /// has no material graph behind it.
    ///
    /// Redundant with `identity` — the stage is in the generated source,
    /// so the hash already separates them — and kept anyway, because a
    /// cache key that says what it holds is worth more than a byte.
    pub stage: Option<MaterialStage>,
}

/// One compiled shader variant.
pub struct ShaderVariant {
    /// What this variant is.
    pub key: VariantKey,
    /// The `wgpu` module, ready to build pipelines from.
    pub module: wgpu::ShaderModule,
    /// The WGSL the WXSL compiler produced.
    ///
    /// Kept because it is the only readable answer to "what did my graph
    /// actually turn into", which is most of shader-graph debugging. It is a
    /// few kilobytes per variant.
    pub wgsl: String,
    /// Label used for the `wgpu` module and in error messages.
    pub label: String,
}

/// How often the cache has been hit and missed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Requests served from the cache.
    pub hits: u64,
    /// Requests that had to compile.
    pub misses: u64,
}

/// The variant cache.
#[derive(Default)]
pub struct ShaderVariants {
    entries: HashMap<VariantKey, Arc<ShaderVariant>>,
    stats: CacheStats,
}

impl ShaderVariants {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The variant of `material` for `stage`, compiling it if it is not
    /// cached.
    pub fn material(
        &mut self,
        device: &wgpu::Device,
        library: &ShaderLibrary,
        material: &Material,
        stage: MaterialStage,
    ) -> Result<Arc<ShaderVariant>, RenderError> {
        let key = Self::material_key(material, stage);
        let request = MaterialRequest::new(material, stage);
        self.get_or_compile(device, key, || request.compile(library))
    }

    /// The key `material`'s `stage` would be cached under, without
    /// compiling or even looking.
    pub fn material_key(material: &Material, stage: MaterialStage) -> VariantKey {
        VariantKey {
            kind: VariantKind::Material,
            identity: material.shader(stage).variant_key(),
            stage: Some(stage),
        }
    }

    /// Whether `key` is already compiled.
    pub fn contains(&self, key: &VariantKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Adopt an already-compiled module — how a background swap hands its
    /// results back to the cache it was compiling for
    /// ([`crate::swap`]).
    ///
    /// Counted as a miss, because it is one: the work happened, just not
    /// on this thread.
    pub fn insert(
        &mut self,
        device: &wgpu::Device,
        key: VariantKey,
        label: String,
        wgsl: String,
    ) -> Arc<ShaderVariant> {
        if let Some(existing) = self.entries.get(&key) {
            return Arc::clone(existing);
        }
        self.stats.misses += 1;
        let variant = Arc::new(build_module(device, key, label, wgsl));
        self.entries.insert(key, Arc::clone(&variant));
        variant
    }

    /// The deferred lighting pass variant for `macros`.
    ///
    /// Independent of any material: by the time this pass runs, the material
    /// has already been resolved into the G-buffer. It still varies with the
    /// macro set, because the shading function it calls has `@if`s of its own
    /// (tonemapping, the debug-normal view).
    pub fn lighting_pass(
        &mut self,
        device: &wgpu::Device,
        library: &ShaderLibrary,
        macros: &MacroSet,
    ) -> Result<Arc<ShaderVariant>, RenderError> {
        let key = Self::lighting_pass_key(macros);
        let macros = macros.clone();
        self.get_or_compile(device, key, || {
            let extra: [(&str, Cow<'_, str>); 0] = [];
            (
                "deferred lighting pass".to_string(),
                compile(library, &extra, abi::LIGHTING_PASS_MODULE, &macros),
            )
        })
    }

    /// The key the lighting pass for `macros` is cached under.
    pub fn lighting_pass_key(macros: &MacroSet) -> VariantKey {
        VariantKey {
            kind: VariantKind::LightingPass,
            identity: stable_hash(macros.signature().as_bytes()),
            stage: None,
        }
    }

    fn get_or_compile(
        &mut self,
        device: &wgpu::Device,
        key: VariantKey,
        build: impl FnOnce() -> (String, Result<String, RenderError>),
    ) -> Result<Arc<ShaderVariant>, RenderError> {
        if let Some(existing) = self.entries.get(&key) {
            self.stats.hits += 1;
            return Ok(Arc::clone(existing));
        }
        self.stats.misses += 1;
        let (label, wgsl) = build();
        let variant = Arc::new(build_module(device, key, label, wgsl?));
        self.entries.insert(key, Arc::clone(&variant));
        Ok(variant)
    }

    /// A cached variant, without compiling.
    pub fn get(&self, key: &VariantKey) -> Option<&Arc<ShaderVariant>> {
        self.entries.get(key)
    }

    /// Number of cached variants.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Hit and miss counts since the cache was created.
    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    /// Drop every cached variant, keeping the statistics.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Everything needed to compile one material stage, owned.
///
/// Owned rather than borrowed so it can be sent to a worker thread: a
/// background pipeline swap compiles these off the main thread and hands
/// the WGSL back ([`crate::swap`]).
#[derive(Clone, Debug)]
pub struct MaterialRequest {
    /// The cache key the result belongs under.
    pub key: VariantKey,
    /// Label for the `wgpu` module and for error messages.
    pub label: String,
    /// The generated WXSL for this stage.
    pub source: String,
    /// The macro values to bind.
    pub macros: MacroSet,
}

impl MaterialRequest {
    /// What compiling `material`'s `stage` needs.
    pub fn new(material: &Material, stage: MaterialStage) -> Self {
        let shader = material.shader(stage);
        MaterialRequest {
            key: ShaderVariants::material_key(material, stage),
            label: format!("{} ({stage})", material.name),
            source: shader.source.clone(),
            macros: shader.macros.clone(),
        }
    }

    /// Run the WXSL compiler. Pure, and no device in sight — which is what
    /// makes it safe to do on a worker thread.
    pub fn compile(&self, library: &ShaderLibrary) -> (String, Result<String, RenderError>) {
        let extra = [(
            codegen::MATERIAL_MODULE,
            Cow::Borrowed(self.source.as_str()),
        )];
        (
            self.label.clone(),
            compile(library, &extra, codegen::MATERIAL_MODULE, &self.macros),
        )
    }
}

/// Everything needed to compile the deferred lighting pass, owned.
#[derive(Clone, Debug)]
pub struct LightingRequest {
    /// The cache key the result belongs under.
    pub key: VariantKey,
    /// The macro values to bind.
    pub macros: MacroSet,
}

impl LightingRequest {
    /// What compiling the lighting pass for `macros` needs.
    pub fn new(macros: &MacroSet) -> Self {
        LightingRequest {
            key: ShaderVariants::lighting_pass_key(macros),
            macros: macros.clone(),
        }
    }

    /// Run the WXSL compiler, off any thread.
    pub fn compile(&self, library: &ShaderLibrary) -> (String, Result<String, RenderError>) {
        let extra: [(&str, Cow<'_, str>); 0] = [];
        (
            "deferred lighting pass".to_string(),
            compile(library, &extra, abi::LIGHTING_PASS_MODULE, &self.macros),
        )
    }
}

/// Turn compiled WGSL into a cached variant.
fn build_module(
    device: &wgpu::Device,
    key: VariantKey,
    label: String,
    wgsl: String,
) -> ShaderVariant {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(&label),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(wgsl.as_str())),
    });
    ShaderVariant {
        key,
        module,
        wgsl,
        label,
    }
}

/// Compile `root` to WGSL against `library`, with `extra` modules mounted on
/// top and `macros`' flags bound as conditional-translation features.
///
/// Public because "what WGSL does this graph produce" is worth answering
/// without a GPU — the `--dump-wgsl` flag of the demo, and the tests that
/// check both render paths compile, both go through here.
pub fn compile(
    library: &ShaderLibrary,
    extra: &[(&str, Cow<'_, str>)],
    root: &str,
    macros: &MacroSet,
) -> Result<String, RenderError> {
    // Validate the root path before the compiler sees it: `wxsl-lang`
    // keys its module map by string, so a malformed path would otherwise
    // be reported as a missing module rather than as the malformed path
    // it is. Dropping this check was an oversight of the migration.
    if wxsl_lang::ast::ModulePath::parse(root).is_none() {
        return Err(RenderError::InvalidModulePath {
            module: root.to_string(),
        });
    }

    let mut modules = wxsl_lang::Modules::new();
    for (path, source) in library.iter() {
        modules.insert(path, source);
    }
    for (path, source) in extra {
        modules.insert(*path, source.as_ref());
    }

    wxsl_lang::compile(&modules, root, &bindings(macros)).map_err(|diagnostics| {
        RenderError::ShaderCompile {
            module: root.to_string(),
            // Rendered with the module sources in hand, so the diagnostic
            // carries the offending line and a caret rather than just a
            // position. For generated code that is the difference between a
            // usable error and a shrug.
            diagnostic: diagnostics.render(&|path| modules.get(path).map(str::to_string)),
        }
    })
}

/// Convert the graph model's macro values into the compiler's bindings.
///
/// Two types for the same idea, on purpose: `wxsl-core` owns the node
/// format's spelling of a macro and `wxsl-lang` owns the language's, and
/// neither crate depends on the other (ADR 0002).
fn bindings(macros: &MacroSet) -> wxsl_lang::Bindings {
    macros
        .iter()
        .map(|(name, value)| {
            let value = match value {
                MacroValue::Flag(flag) => wxsl_lang::Value::Bool(flag),
                MacroValue::Int(number) => wxsl_lang::Value::Int(i64::from(number)),
                MacroValue::Float(number) => wxsl_lang::Value::Float(f64::from(number)),
            };
            (name.to_string(), value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_module_is_reported_by_name() {
        let library = ShaderLibrary::new();
        let macros = MacroSet::new();
        let error = compile(&library, &[], "package::nope", &macros)
            .expect_err("nothing to resolve against");
        match error {
            RenderError::ShaderCompile { module, .. } => assert_eq!(module, "package::nope"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn conditional_translation_follows_the_flags() {
        let mut library = ShaderLibrary::new();
        library.insert(
            "package::root",
            "@macro const demo_flag: bool = false;
\n             @if(demo_flag)\nfn value() -> f32 { return 1.0; }\n\
             @if(!demo_flag)\nfn value() -> f32 { return 0.0; }\n\
             @fragment fn main() -> @location(0) vec4f { return vec4f(value()); }",
        );

        let mut macros = MacroSet::new();
        macros.set("demo_flag", wxsl_core::macros::MacroValue::Flag(true));
        let enabled = compile(&library, &[], "package::root", &macros).unwrap();
        assert!(enabled.contains("return 1.0;"), "{enabled}");

        macros.set("demo_flag", wxsl_core::macros::MacroValue::Flag(false));
        let disabled = compile(&library, &[], "package::root", &macros).unwrap();
        assert!(disabled.contains("return 0.0;"), "{disabled}");
    }

    #[test]
    fn an_undeclared_flag_is_an_error_rather_than_silently_false() {
        // The old compiler treated an unspecified feature as disabled, so a
        // typo in an `@if` quietly took the false arm. WXSL requires the
        // declaration, which turns that class of bug into a diagnostic.
        let mut library = ShaderLibrary::new();
        library.insert(
            "package::root",
            "@if(never_declared)
fn value() -> f32 { return 1.0; }
             @fragment fn main() -> @location(0) vec4f { return vec4f(0.0); }",
        );
        let error =
            compile(&library, &[], "package::root", &MacroSet::new()).expect_err("undeclared flag");
        let message = error.to_string();
        assert!(message.contains("never_declared"), "{message}");
        assert!(message.contains("not a macro declared"), "{message}");
    }

    #[test]
    fn invalid_module_paths_are_rejected_before_the_compiler_sees_them() {
        let library = ShaderLibrary::new();
        let error = compile(&library, &[], "", &MacroSet::new()).expect_err("malformed root path");
        assert!(matches!(error, RenderError::InvalidModulePath { .. }));
    }
}
