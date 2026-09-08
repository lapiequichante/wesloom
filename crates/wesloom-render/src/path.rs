//! [`RenderPath`]: which pipeline shape a shader variant is compiled for.
//!
//! A `RenderPath` is a property of the *pipeline*, never of the graph. The
//! same material graph compiles for either path, because the difference is
//! expressed as conditional translation inside one WESL module
//! ([ADR 0005](../../../docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md)):
//! selecting a path binds [`wesloom_core::abi::FEATURE_DEFERRED`], which
//! picks one of the two fragment entry points codegen emitted.

use core::fmt;

use wesloom_core::abi;
use wesloom_core::macros::{MacroSet, MacroValue};

/// The pipeline shape a variant is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum RenderPath {
    /// Shade in the material's own fragment pass: one pass, lighting done
    /// where the surface is evaluated.
    #[default]
    Forward,
    /// Write the surface to a G-buffer, then light it in a fullscreen pass.
    Deferred,
}

impl RenderPath {
    /// Both paths, in declaration order. Handy for "compile everything up
    /// front" and for tests that must cover each path.
    pub const ALL: &'static [RenderPath] = &[RenderPath::Forward, RenderPath::Deferred];

    /// The path's name, as used in shader labels and on the command line.
    pub fn name(&self) -> &'static str {
        match self {
            RenderPath::Forward => "forward",
            RenderPath::Deferred => "deferred",
        }
    }

    /// Parse a path from its [`RenderPath::name`].
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "forward" => Some(RenderPath::Forward),
            "deferred" => Some(RenderPath::Deferred),
            _ => None,
        }
    }

    /// Whether this path lights the surface in a later pass.
    pub fn is_deferred(&self) -> bool {
        matches!(self, RenderPath::Deferred)
    }

    /// Add this path's conditional-translation flag to `macros`.
    ///
    /// The flag is set by the renderer rather than being editable, so this
    /// overwrites whatever a graph might have pinned under that name — the
    /// pipeline decides the path, and a graph claiming otherwise would
    /// compile a shader whose entry points do not match the pass it is used
    /// in.
    pub fn apply_to(&self, macros: &mut MacroSet) {
        macros.set(
            abi::FEATURE_DEFERRED.to_string(),
            MacroValue::Flag(self.is_deferred()),
        );
    }
}

impl fmt::Display for RenderPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `pad` so `{:>8}` in a progress line actually aligns.
        f.pad(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for path in RenderPath::ALL {
            assert_eq!(RenderPath::parse(path.name()), Some(*path));
        }
        assert_eq!(RenderPath::parse("  Deferred "), Some(RenderPath::Deferred));
        assert_eq!(RenderPath::parse("visibility"), None);
    }

    #[test]
    fn the_path_flag_wins_over_whatever_the_graph_pinned() {
        let mut macros = MacroSet::new();
        macros.set(abi::FEATURE_DEFERRED, MacroValue::Flag(true));
        RenderPath::Forward.apply_to(&mut macros);
        assert_eq!(
            macros.get(abi::FEATURE_DEFERRED),
            Some(MacroValue::Flag(false))
        );
    }
}
