//! Effects as first-class units: the pass-level twin of what a node is to
//! a material.
//!
//! Before this module, a screen pass ran one of an enum's variants — and
//! the enum had one variant, `ScreenShader::DeferredLighting`, reachable
//! only by editing `wxsl-render`. An effect is now *data*, like everything
//! else a pipeline is made of: what it reads, the shader it runs, the
//! entry points to build a pipeline from
//! ([plan2 P4](../../../plan2.md)). The pipeline compiler validates a
//! `pass.screen` node against the registry's declarations; the renderer
//! compiles and runs the shader they name. An application adds an effect
//! with [`Renderer::add_effect`] and a document names it by `effect` id —
//! passes, like shaders before them, are addable without touching
//! `wxsl-render` (ADR 0009's rule, extended).
//!
//! # What an effect declares, and who reads it
//!
//! [`Effect::inputs`] is the contract both sides compile against, in
//! binding order: the compiler turns each wired socket into that many
//! pass-group reads, and the shader declares a `@group(3) @binding(n)`
//! per input in the same order. The lighting effect shades a G-buffer —
//! one texture per layout target, then depth; bloom takes one image.
//!
//! # The shipped effects
//!
//! [`EffectRegistry::shipped`] carries the migrated deferred lighting
//! pass — the first effect, generated from the enabled lighting set
//! exactly as it always was — and bloom, the proof that the mechanism is
//! real: a post effect over an image input, wired into a pipeline as a
//! document edit, which is what makes an effect *chain* expressible for
//! the first time (deferred lighting into a `resource.color`, bloom over
//! it, bloom's `into` left unconnected so it writes the frame's target).

use wxsl_core::abi;

/// Module path the shipped bloom effect's shader is mounted under.
pub const BLOOM_MODULE: &str = "package::wxsl::bloom";

/// One input an effect consumes.
///
/// The `name` is the `pass.screen` socket the document wires, and the
/// `kind` is what the compiler does with it — the two spellings exist
/// because a G-buffer input expands to one texture per layout target plus
/// depth, while an image is exactly one read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectInput {
    /// The `pass.screen` socket this input is wired through.
    pub name: &'static str,
    /// What the compiler turns the wire into.
    pub kind: EffectInputKind,
    /// One line for the palette and for error messages.
    pub description: &'static str,
}

/// The shape of an effect input, and therefore how many pass-group
/// bindings it becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectInputKind {
    /// The G-buffer: one texture per layout target, then depth — the order
    /// the generated lighting pass declares its bindings in.
    GBuffer,
    /// One colour image, one binding.
    Image,
}

/// The shader an effect runs.
///
/// Every effect's shader is a WXSL module with a vertex and a fragment
/// entry; the variants are *where the text comes from*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectShader {
    /// Generated from the enabled lighting set — the migrated deferred
    /// lighting pass, mounted under [`abi::LIGHTING_PASS_MODULE`] exactly
    /// as before. The one effect whose text is not fixed, because a pass
    /// that dispatches over models has to name them.
    Lighting,
    /// A fixed WXSL source owned by the effect, mounted under `path` when
    /// the variant is compiled. The effect travels with its shader: an
    /// application's `include_str!` of its own file is the same data.
    Source {
        /// Module path to mount the source under.
        path: &'static str,
        /// The WXSL text.
        wxsl: &'static str,
    },
}

/// A screen effect, as data: what it reads, the shader it runs, the entry
/// points a `wgpu` pipeline is built from.
///
/// `Copy` on purpose: an effect is a static description, and the renderer
/// hands copies around freely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effect {
    /// The id a document's `effect` setting names.
    pub id: &'static str,
    /// Display name, for palettes and labels.
    pub label: &'static str,
    /// One line for the palette.
    pub description: &'static str,
    /// The inputs, in pass-group binding order — the contract the
    /// pipeline compiler validates wiring against and the shader declares
    /// its `@group(3)` bindings by.
    pub inputs: &'static [EffectInput],
    /// Vertex entry point (a fullscreen triangle, no vertex buffer).
    pub vertex_entry: &'static str,
    /// Fragment entry point.
    pub fragment_entry: &'static str,
    /// Where the shader text comes from.
    pub shader: EffectShader,
}

impl Effect {
    /// Whether this effect declares an input wired through the named
    /// `pass.screen` socket.
    pub(crate) fn declares(&self, socket: &str) -> bool {
        self.inputs.iter().any(|input| input.name == socket)
    }
}

/// The deferred lighting pass, as an effect: the first one, migrated out
/// of the hardcoded enum it was reachable only through. Its generated
/// module and its bindings are byte-for-byte what they were.
pub const DEFERRED_LIGHTING: Effect = Effect {
    id: "deferred_lighting",
    label: "deferred lighting",
    description: "Shade the G-buffer with the enabled lighting models.",
    inputs: &[EffectInput {
        name: "gbuffer",
        kind: EffectInputKind::GBuffer,
        description: "The G-buffer to shade: every target, then depth.",
    }],
    vertex_entry: abi::LIGHTING_PASS_VERTEX_ENTRY,
    fragment_entry: abi::LIGHTING_PASS_FRAGMENT_ENTRY,
    shader: EffectShader::Lighting,
};

/// Bloom: glow for the bright parts of an image, the proof that effects
/// are real units — one descriptor row, one shader file, and any pipeline
/// document can wire it into a chain.
pub const BLOOM: Effect = Effect {
    id: "bloom",
    label: "bloom",
    description: "Blur the bright parts of an image and add them back: glow.",
    inputs: &[EffectInput {
        name: "image",
        kind: EffectInputKind::Image,
        description: "The image to glow from, in the frame's own encoding.",
    }],
    vertex_entry: "bloom_vs",
    fragment_entry: "bloom_fs",
    shader: EffectShader::Source {
        path: BLOOM_MODULE,
        wxsl: include_str!("../shaders/bloom.wxsl"),
    },
};

/// The effects a renderer knows how to run, and a pipeline document may
/// name.
///
/// Shipped with the two above; extended with [`EffectRegistry::add`] —
/// that is the whole of "add a post effect" now.
#[derive(Clone, Debug, Default)]
pub struct EffectRegistry {
    effects: Vec<Effect>,
}

impl EffectRegistry {
    /// The effects this crate ships: the migrated lighting pass and bloom.
    pub fn shipped() -> Self {
        EffectRegistry {
            effects: vec![DEFERRED_LIGHTING, BLOOM],
        }
    }

    /// An empty registry — for an application that wants exactly its own
    /// effects and not this crate's.
    pub fn empty() -> Self {
        EffectRegistry {
            effects: Vec::new(),
        }
    }

    /// Register `effect`, replacing any effect already registered under
    /// the same id — an application overriding a shipped effect is the
    /// same gesture as adding a new one.
    pub fn add(&mut self, effect: Effect) {
        match self.effects.iter().position(|known| known.id == effect.id) {
            Some(at) => self.effects[at] = effect,
            None => self.effects.push(effect),
        }
    }

    /// A builder form of [`EffectRegistry::add`].
    pub fn with(mut self, effect: Effect) -> Self {
        self.add(effect);
        self
    }

    /// The effect named by `id`, as documents name them.
    pub fn get(&self, id: &str) -> Option<Effect> {
        self.effects.iter().copied().find(|effect| effect.id == id)
    }

    /// Every registered effect, in registration order — what a palette
    /// lists.
    pub fn iter(&self) -> impl Iterator<Item = &Effect> {
        self.effects.iter()
    }

    /// The ids, for an error message that says what *would* have matched.
    pub fn ids(&self) -> Vec<String> {
        self.effects
            .iter()
            .map(|effect| effect.id.to_string())
            .collect()
    }

    /// How many effects are registered.
    pub fn len(&self) -> usize {
        self.effects.len()
    }

    /// Whether no effect is registered — a registry a pipeline document
    /// naming any `pass.screen` cannot compile against.
    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }
}

impl std::fmt::Display for EffectRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, effect) in self.effects.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            let _ = write!(f, "{}", effect.id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_registry_holds_the_migrated_lighting_pass_and_bloom() {
        let registry = EffectRegistry::shipped();
        assert_eq!(registry.len(), 2);

        let lighting = registry
            .get("deferred_lighting")
            .expect("the lighting pass");
        assert!(matches!(lighting.shader, EffectShader::Lighting));
        assert_eq!(lighting.vertex_entry, abi::LIGHTING_PASS_VERTEX_ENTRY);
        assert_eq!(
            lighting.inputs.len(),
            1,
            "the lighting effect takes exactly a G-buffer"
        );
        assert_eq!(lighting.inputs[0].kind, EffectInputKind::GBuffer);

        let bloom = registry.get("bloom").expect("bloom");
        assert!(matches!(bloom.shader, EffectShader::Source { .. }));
        assert_eq!(bloom.inputs[0].kind, EffectInputKind::Image);
    }

    #[test]
    fn an_added_effect_can_replace_a_shipped_one_under_the_same_id() {
        let replacement = Effect {
            description: "a re-tuned lighting pass",
            ..DEFERRED_LIGHTING
        };
        let registry = EffectRegistry::shipped().with(replacement);
        assert_eq!(registry.len(), 2, "replacing, not appending");
        assert_eq!(
            registry
                .get("deferred_lighting")
                .expect("replaced")
                .description,
            "a re-tuned lighting pass"
        );
    }

    #[test]
    fn the_bloom_shader_declares_one_binding_for_its_one_input() {
        // The contract between descriptor and shader, checked where a
        // change to either side fails in seconds rather than as a `wgpu`
        // bind-group complaint minutes later: one image input means one
        // `@group(3)` binding, and the entry points the descriptor names
        // are the ones the file declares.
        let EffectShader::Source { path: _, wxsl } = BLOOM.shader else {
            panic!("bloom ships its source");
        };
        assert!(
            wxsl.contains("@group(3) @binding(0) var image:"),
            "bloom.wxsl binds its one image input"
        );
        for entry in ["fn bloom_vs(", "fn bloom_fs("] {
            assert!(wxsl.contains(entry), "bloom.wxsl declares no `{entry}`");
        }
        assert!(
            !wxsl.contains("@binding(1)"),
            "one input, one binding — a second is a drift from the descriptor"
        );
    }
}
