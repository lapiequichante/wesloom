//! [`ShaderLibrary`]: the WXSL modules imports are resolved against.
//!
//! This crate compiles WXSL but ships none: the shader ABI and the node
//! library live in `wxsl-stdlib`, which this crate must not depend on
//! ([ADR 0002](../../../docs/adr/0002-cargo-workspace-crate-boundaries.md) —
//! the dependency arrow only points into `wxsl-core`). So the application
//! hands the modules in, and a consumer can substitute its own ABI
//! implementation or add hand-written WXSL of its own the same way:
//!
//! ```text
//! let mut library = ShaderLibrary::new();
//! library.insert_all(wxsl_stdlib::MODULES.iter().copied());
//! library.insert("package::game::water", include_str!("water.wxsl"));
//! ```

use std::borrow::Cow;
use std::collections::BTreeMap;

use wxsl_core::abi;

use crate::error::RenderError;

/// A set of WXSL modules, keyed by module path.
#[derive(Clone, Debug, Default)]
pub struct ShaderLibrary {
    modules: BTreeMap<String, Cow<'static, str>>,
}

impl ShaderLibrary {
    /// An empty library.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a module, returning the source it replaced.
    pub fn insert(
        &mut self,
        path: impl Into<String>,
        source: impl Into<Cow<'static, str>>,
    ) -> Option<Cow<'static, str>> {
        self.modules.insert(path.into(), source.into())
    }

    /// Add many modules, e.g. all of `wxsl_stdlib::MODULES`.
    pub fn insert_all<P, S>(&mut self, modules: impl IntoIterator<Item = (P, S)>)
    where
        P: Into<String>,
        S: Into<Cow<'static, str>>,
    {
        for (path, source) in modules {
            self.insert(path, source);
        }
    }

    /// The source of one module.
    pub fn get(&self, path: &str) -> Option<&str> {
        self.modules.get(path).map(|source| source.as_ref())
    }

    /// Whether `path` is present.
    pub fn contains(&self, path: &str) -> bool {
        self.modules.contains_key(path)
    }

    /// Number of modules.
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    /// Whether the library is empty.
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    /// Iterate over `(path, source)` in path order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.modules
            .iter()
            .map(|(path, source)| (path.as_str(), source.as_ref()))
    }

    /// Check that every module the shader ABI names is present.
    ///
    /// Worth calling once at startup: the failure mode otherwise is a
    /// a "no module" error on the first material compiled, which is
    /// both later and less obvious than being told the library is incomplete.
    ///
    /// The shading function and the lighting pass are *generated* now, from
    /// the enabled lighting models (ADR 0028), so they are not in any
    /// library — and a lighting model's module is checked by
    /// `Renderer::set_lighting`, which knows the set to check.
    pub fn check_abi(&self) -> Result<(), RenderError> {
        for module in [abi::SURFACE_MODULE, abi::VERTEX_MODULE, abi::SHADOW_MODULE] {
            if !self.contains(module) {
                return Err(RenderError::MissingModule {
                    module: module.to_string(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_abi_names_the_module_that_is_missing() {
        let mut library = ShaderLibrary::new();
        library.insert(abi::SURFACE_MODULE, "// stub");
        let error = library.check_abi().expect_err("incomplete library");
        match error {
            RenderError::MissingModule { module } => assert_eq!(module, abi::VERTEX_MODULE),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn insert_replaces_and_reports_the_previous_source() {
        let mut library = ShaderLibrary::new();
        assert!(library.insert("package::a", "one").is_none());
        assert_eq!(library.insert("package::a", "two").as_deref(), Some("one"));
        assert_eq!(library.get("package::a"), Some("two"));
        assert_eq!(library.len(), 1);
    }
}
