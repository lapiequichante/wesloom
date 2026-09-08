//! The shader variant cache: WESL in, `wgpu` shader modules out, compiled
//! once per (shader, macro set, render path).
//!
//! Switching render path at runtime means asking for a different variant of
//! the same material; so does flipping a macro variable. Both go through
//! [`ShaderVariants::material`], which compiles on a miss and hands back a
//! cached module on a hit — the caller never has to know which happened
//! ([ADR 0005](../../../docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md)).
//!
//! Unspecified conditional-translation features compile as *disabled* rather
//! than as an error, because a graph may pin flags this renderer knows
//! nothing about (hand-written WESL brought by the application). The flip
//! side is that a typo in an `@if` silently takes the false arm.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use wesl::{VirtualResolver, Wesl};
use wesloom_core::abi;
use wesloom_core::codegen::{self, GeneratedShader};
use wesloom_core::macros::MacroSet;
use wesloom_core::wesl::stable_hash;

use crate::error::RenderError;
use crate::library::ShaderLibrary;
use crate::path::RenderPath;

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
/// `identity` folds in the generated source *and* the macro values, since
/// neither flag macros (bound as compiler features) nor numeric ones
/// (declared in the generated macro module) show up in the root module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VariantKey {
    /// Material or lighting pass.
    pub kind: VariantKind,
    /// Hash of the source and macro values.
    pub identity: u64,
    /// The render path the variant was compiled for.
    pub path: RenderPath,
}

/// One compiled shader variant.
pub struct ShaderVariant {
    /// What this variant is.
    pub key: VariantKey,
    /// The `wgpu` module, ready to build pipelines from.
    pub module: wgpu::ShaderModule,
    /// The WGSL the WESL compiler produced.
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

    /// The variant for `shader` on `path`, compiling it if it is not cached.
    pub fn material(
        &mut self,
        device: &wgpu::Device,
        library: &ShaderLibrary,
        shader: &GeneratedShader,
        path: RenderPath,
    ) -> Result<Arc<ShaderVariant>, RenderError> {
        let mut macros = shader.macros.clone();
        path.apply_to(&mut macros);
        let key = VariantKey {
            kind: VariantKind::Material,
            identity: shader.variant_key(),
            path,
        };
        self.get_or_compile(device, key, || {
            let extra = [
                (
                    codegen::MATERIAL_MODULE,
                    Cow::Borrowed(shader.source.as_str()),
                ),
                (
                    abi::MACROS_MODULE,
                    Cow::Owned(codegen::macro_module(&macros)),
                ),
            ];
            (
                format!("material ({path})"),
                compile(library, &extra, codegen::MATERIAL_MODULE, &macros),
            )
        })
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
        let mut macros = macros.clone();
        RenderPath::Deferred.apply_to(&mut macros);
        let key = VariantKey {
            kind: VariantKind::LightingPass,
            identity: stable_hash(macros.signature().as_bytes()),
            path: RenderPath::Deferred,
        };
        self.get_or_compile(device, key, || {
            let extra = [(
                abi::MACROS_MODULE,
                Cow::Owned(codegen::macro_module(&macros)),
            )];
            (
                "deferred lighting pass".to_string(),
                compile(library, &extra, abi::LIGHTING_PASS_MODULE, &macros),
            )
        })
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
        let wgsl = wgsl?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&label),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(wgsl.as_str())),
        });
        let variant = Arc::new(ShaderVariant {
            key,
            module,
            wgsl,
            label,
        });
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
    let mut resolver = VirtualResolver::new();
    for (path, source) in library.iter() {
        resolver.add_module(parse_module_path(path)?, Cow::Borrowed(source));
    }
    for (path, source) in extra {
        resolver.add_module(parse_module_path(path)?, Cow::Borrowed(source.as_ref()));
    }

    let mut compiler = Wesl::new(".").set_custom_resolver(resolver);
    // Sourcemapping makes the compiler's diagnostics point at the module and
    // line the problem came from, which for generated code is the difference
    // between a usable error and a shrug.
    compiler.use_sourcemap(true);
    for (name, value) in macros.flags() {
        compiler.set_feature(name, value);
    }

    compiler
        .compile(&parse_module_path(root)?)
        .map(|compiled| compiled.to_string())
        .map_err(|error| RenderError::ShaderCompile {
            module: root.to_string(),
            diagnostic: error.to_string(),
        })
}

fn parse_module_path(path: &str) -> Result<wesl::ModulePath, RenderError> {
    path.parse().map_err(|_| RenderError::InvalidModulePath {
        module: path.to_string(),
    })
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
            "@if(demo_flag)\nfn value() -> f32 { return 1.0; }\n\
             @if(!demo_flag)\nfn value() -> f32 { return 0.0; }\n\
             @fragment fn main() -> @location(0) vec4f { return vec4f(value()); }",
        );

        let mut macros = MacroSet::new();
        macros.set("demo_flag", wesloom_core::macros::MacroValue::Flag(true));
        let enabled = compile(&library, &[], "package::root", &macros).unwrap();
        assert!(enabled.contains("return 1.0;"), "{enabled}");

        macros.set("demo_flag", wesloom_core::macros::MacroValue::Flag(false));
        let disabled = compile(&library, &[], "package::root", &macros).unwrap();
        assert!(disabled.contains("return 0.0;"), "{disabled}");
    }

    #[test]
    fn invalid_module_paths_are_rejected_before_the_compiler_sees_them() {
        let library = ShaderLibrary::new();
        let error = compile(&library, &[], "", &MacroSet::new()).expect_err("malformed root path");
        assert!(matches!(error, RenderError::InvalidModulePath { .. }));
    }
}
