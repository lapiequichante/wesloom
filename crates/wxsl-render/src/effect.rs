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
//! exactly as it always was — [`TONEMAP`], the display transform every
//! stock chain now ends in, and bloom, the proof that the mechanism is
//! real: a post effect over an image input, wired into a pipeline as a
//! document edit, which is what makes an effect *chain* expressible for
//! the first time (shading into a `resource.color`, bloom over it, the
//! tonemap's `into` left unconnected so *it* writes the frame's target).
//! Two further effects ship as descriptors but stay out of the registry:
//! [`BRDF_LUT`] and [`LUT_VIEW`], the execution-policy proof (plan2 P10)
//! — an application registers them with `Renderer::add_effect` exactly as
//! it would its own.

use wxsl_core::abi;

/// Module path the shipped bloom effect's shader is mounted under.
pub const BLOOM_MODULE: &str = "package::wxsl::bloom";
/// Module path the shipped tonemap effect's shader is mounted under.
pub const TONEMAP_MODULE: &str = "package::wxsl::tonemap";
/// Module path the BRDF-LUT bake's shader is mounted under.
pub const BRDF_LUT_MODULE: &str = "package::wxsl::brdf_lut";
/// Module path the LUT viewer's shader is mounted under.
pub const LUT_VIEW_MODULE: &str = "package::wxsl::lut_view";

/// One input an effect consumes.
///
/// The `name` is the `pass.screen` socket the document wires, and the
/// `kind` is what the compiler does with it — the spellings exist
/// because a G-buffer input expands to one texture per layout target plus
/// depth, an image is exactly one read, and a buffer one more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectInput {
    /// The socket this input is wired through (the `pass.screen` socket in
    /// a document).
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
    /// One storage buffer, read as storage — one binding. The reader
    /// declares `var<storage>` in its shader, so it sees the data the
    /// writer's compute left there without a copy through a texture
    /// (plan2 P11).
    Buffer,
}

/// One output an effect writes that is not an attachment: a compute
/// effect's storage target.
///
/// Named for palettes and diagnostics; the binding comes from the pass's
/// `writes`, in this order, after every input. A screen effect writes its
/// attachment and declares no outputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectOutput {
    /// The name the output goes by.
    pub name: &'static str,
    /// One line for the palette and for error messages.
    pub description: &'static str,
}

/// What work an effect does, and therefore which entry points a `wgpu`
/// pipeline is built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectKind {
    /// One fullscreen triangle, with a vertex and a fragment entry. The
    /// fragment writes the pass's colour attachment.
    Screen {
        /// Vertex entry point (a fullscreen triangle, no vertex buffer).
        vertex_entry: &'static str,
        /// Fragment entry point.
        fragment_entry: &'static str,
    },
    /// One compute dispatch. The entry is the module's; the workgroup
    /// count is the effect's own business — it knows its shader's
    /// `@workgroup_size`, which is why it lives here and not on the pass
    /// (plan2 P10). Indirect dispatch waits for a consumer that needs it.
    Compute {
        /// Entry point in the module.
        entry: &'static str,
        /// Workgroups in x, y, z.
        workgroups: [u32; 3],
    },
}

/// The shader an effect runs.
///
/// Every effect's shader is a WXSL module; the variants are *where the
/// text comes from*.
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

/// An effect, as data: what it reads and writes, the shader it runs, the
/// entry points a `wgpu` pipeline is built from.
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
    /// What work the effect does, and its entry points.
    pub kind: EffectKind,
    /// The inputs, in pass-group binding order — the contract the
    /// pipeline compiler validates wiring against and the shader declares
    /// its `@group(3)` bindings by.
    pub inputs: &'static [EffectInput],
    /// The non-attachment writes, in pass-group binding order after the
    /// inputs — a compute effect's storage targets.
    pub outputs: &'static [EffectOutput],
    /// Where the shader text comes from.
    pub shader: EffectShader,
}

impl Effect {
    /// Whether this effect declares an input wired through the named
    /// socket.
    pub(crate) fn declares(&self, socket: &str) -> bool {
        self.inputs.iter().any(|input| input.name == socket)
    }

    /// Whether this is a compute effect — and so whether the pass running
    /// it dispatches rather than draws.
    pub fn is_compute(&self) -> bool {
        matches!(self.kind, EffectKind::Compute { .. })
    }
}

/// The deferred lighting pass, as an effect: the first one, migrated out
/// of the hardcoded enum it was reachable only through. Its generated
/// module and its bindings are byte-for-byte what they were.
pub const DEFERRED_LIGHTING: Effect = Effect {
    id: "deferred_lighting",
    label: "deferred lighting",
    description: "Shade the G-buffer with the enabled lighting models.",
    kind: EffectKind::Screen {
        vertex_entry: abi::LIGHTING_PASS_VERTEX_ENTRY,
        fragment_entry: abi::LIGHTING_PASS_FRAGMENT_ENTRY,
    },
    inputs: &[EffectInput {
        name: "gbuffer",
        kind: EffectInputKind::GBuffer,
        description: "The G-buffer to shade: every target, then depth.",
    }],
    outputs: &[],
    shader: EffectShader::Lighting,
};

/// Bloom: glow for the bright parts of an image, the proof that effects
/// are real units — one descriptor row, one shader file, and any pipeline
/// document can wire it into a chain.
pub const BLOOM: Effect = Effect {
    id: "bloom",
    label: "bloom",
    description: "Blur the bright parts of an image and add them back: glow.",
    kind: EffectKind::Screen {
        vertex_entry: "bloom_vs",
        fragment_entry: "bloom_fs",
    },
    inputs: &[EffectInput {
        name: "image",
        kind: EffectInputKind::Image,
        description: "The image to glow from, as linear radiance.",
    }],
    outputs: &[],
    shader: EffectShader::Source {
        path: BLOOM_MODULE,
        wxsl: include_str!("../shaders/bloom.wxsl"),
    },
};

/// Tonemap: the display transform, as the last pass of every stock
/// chain — the filmic curve and the sRGB encode that used to sit inside
/// the generated `shade_surface` under a macro
/// ([ADR 0039](../../../docs/adr/0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md)).
///
/// Shipped, and in the stock pipelines, because a pipeline that does not
/// end in one presents linear radiance — correct for a chain that goes on
/// to another effect, wrong on a screen.
pub const TONEMAP: Effect = Effect {
    id: "tonemap",
    label: "tonemap",
    description: "Curve linear radiance for the display, and encode it.",
    kind: EffectKind::Screen {
        vertex_entry: "tonemap_vs",
        fragment_entry: "tonemap_fs",
    },
    inputs: &[EffectInput {
        name: "image",
        kind: EffectInputKind::Image,
        description: "The linear image to tonemap.",
    }],
    outputs: &[],
    shader: EffectShader::Source {
        path: TONEMAP_MODULE,
        wxsl: include_str!("../shaders/tonemap.wxsl"),
    },
};

/// The split-sum environment-BRDF LUT, baked by a compute effect — the
/// execution-policy proof (plan2 P10). Not in the shipped registry: no
/// stock pipeline reads it yet (the IBL that would is M7's), so this is
/// a descriptor an application registers and a worked example of a
/// `once`-policy compute effect. Its shader is a pure function of its
/// coordinates, which is why `once` is the honest policy for it.
pub const BRDF_LUT: Effect = Effect {
    id: "brdf_lut",
    label: "BRDF LUT",
    description: "Bake the split-sum environment-BRDF LUT, once.",
    kind: EffectKind::Compute {
        entry: "bake_lut",
        workgroups: [8, 8, 1],
    },
    inputs: &[],
    outputs: &[EffectOutput {
        name: "lut",
        description: "The LUT: a 64x64 storage texture of (scale, bias).",
    }],
    shader: EffectShader::Source {
        path: BRDF_LUT_MODULE,
        wxsl: include_str!("../shaders/brdf_lut.wxsl"),
    },
};

/// The LUT viewer: one image, stretched over the frame's target — the
/// screen half of the [`BRDF_LUT`] proof, and how a demo or a test looks
/// at what a once-only bake left behind.
pub const LUT_VIEW: Effect = Effect {
    id: "lut_view",
    label: "LUT view",
    description: "Draw one image across the frame's target.",
    kind: EffectKind::Screen {
        vertex_entry: "view_vs",
        fragment_entry: "view_fs",
    },
    inputs: &[EffectInput {
        name: "image",
        kind: EffectInputKind::Image,
        description: "The image to display.",
    }],
    outputs: &[],
    shader: EffectShader::Source {
        path: LUT_VIEW_MODULE,
        wxsl: include_str!("../shaders/lut_view.wxsl"),
    },
};

/// Module path the ramp fill's shader is mounted under.
pub const RAMP_MODULE: &str = "package::wxsl::ramp";
/// Module path the ramp viewer's shader is mounted under.
pub const RAMP_VIEW_MODULE: &str = "package::wxsl::ramp_view";

/// The buffer proof (plan2 P11): a compute effect whose one output is a
/// 256-entry storage buffer of eased values. Like [`BRDF_LUT`], a
/// descriptor an application registers rather than a shipped pass — and
/// the smallest complete example of a buffer-writing effect.
pub const RAMP_FILL: Effect = Effect {
    id: "ramp_fill",
    label: "ramp fill",
    description: "Fill a storage buffer with an eased ramp, one f32 per entry.",
    kind: EffectKind::Compute {
        entry: "fill_ramp",
        workgroups: [4, 1, 1],
    },
    inputs: &[],
    outputs: &[EffectOutput {
        name: "ramp",
        description: "The buffer: 256 f32 values, a smoothstep ease of the index.",
    }],
    shader: EffectShader::Source {
        path: RAMP_MODULE,
        wxsl: include_str!("../shaders/ramp.wxsl"),
    },
};

/// The buffer reader: a screen effect that draws a storage buffer as one
/// value per screen column — the half that proves a fragment stage can
/// consume what a compute pass wrote, with no texture in between.
pub const RAMP_VIEW: Effect = Effect {
    id: "ramp_view",
    label: "ramp view",
    description: "Draw a storage buffer as one value per screen column.",
    kind: EffectKind::Screen {
        vertex_entry: "view_vs",
        fragment_entry: "view_fs",
    },
    inputs: &[EffectInput {
        name: "ramp",
        kind: EffectInputKind::Buffer,
        description: "The buffer to draw, read as storage.",
    }],
    outputs: &[],
    shader: EffectShader::Source {
        path: RAMP_VIEW_MODULE,
        wxsl: include_str!("../shaders/ramp_view.wxsl"),
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
    /// The effects this crate ships: the migrated lighting pass, the
    /// display transform every stock chain ends in, and bloom.
    pub fn shipped() -> Self {
        EffectRegistry {
            effects: vec![DEFERRED_LIGHTING, TONEMAP, BLOOM],
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
        assert_eq!(registry.len(), 3);

        let lighting = registry
            .get("deferred_lighting")
            .expect("the lighting pass");
        assert!(matches!(lighting.shader, EffectShader::Lighting));
        let EffectKind::Screen {
            vertex_entry,
            fragment_entry,
        } = lighting.kind
        else {
            panic!("the lighting pass is a screen effect");
        };
        assert_eq!(vertex_entry, abi::LIGHTING_PASS_VERTEX_ENTRY);
        assert_eq!(fragment_entry, abi::LIGHTING_PASS_FRAGMENT_ENTRY);
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
        assert_eq!(registry.len(), 3, "replacing, not appending");
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

    #[test]
    fn the_tonemap_shader_applies_the_librarys_curve_to_its_one_input() {
        let EffectShader::Source { path: _, wxsl } = TONEMAP.shader else {
            panic!("the tonemap ships its source");
        };
        assert!(
            wxsl.contains("@group(3) @binding(0) var image:"),
            "the tonemap binds its one image input"
        );
        assert!(!wxsl.contains("@binding(1)"), "one input, one binding");
        for entry in ["fn tonemap_vs(", "fn tonemap_fs("] {
            assert!(wxsl.contains(entry), "tonemap.wxsl declares no `{entry}`");
        }
        // The curve is the library's, not a second copy of it: there is one
        // filmic curve in this repo and the effect imports it.
        for import in [
            "package::color::tonemap_filmic",
            "package::color::linear_to_srgb",
        ] {
            assert!(
                wxsl.contains(import),
                "the tonemap applies the library's `{import}`"
            );
        }
    }

    #[test]
    fn the_brdf_lut_shader_writes_its_one_output_and_reads_nothing() {
        // A compute effect with no inputs and one storage write: the
        // write is binding 0 (outputs come after inputs, of which there
        // are none), and the descriptor's entry and workgroup count are
        // the shader's.
        let EffectShader::Source { path: _, wxsl } = BRDF_LUT.shader else {
            panic!("the LUT bake ships its source");
        };
        assert!(
            wxsl.contains("@group(3) @binding(0) var lut: texture_storage_2d"),
            "the bake writes its one output"
        );
        assert!(wxsl.contains("fn bake_lut("), "the declared entry exists");
        assert!(
            wxsl.contains("@workgroup_size(8, 8)"),
            "the workgroup size divides the declared 8x8x1 dispatch"
        );
        assert!(!wxsl.contains("@binding(1)"), "one output, one binding");
        let EffectKind::Compute { workgroups, .. } = BRDF_LUT.kind else {
            panic!("the LUT bake is a compute effect");
        };
        assert_eq!(workgroups, [8, 8, 1]);
    }
}
