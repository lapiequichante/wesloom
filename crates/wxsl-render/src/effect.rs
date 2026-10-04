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
//! [`Effect::parameters`] is the tunable half of the same contract
//! ([ADR 0042](../../../docs/adr/0042-effect-parameters-are-uniforms-the-descriptor-declares-them.md)):
//! name and default per knob, laid out by `wxsl-core`'s uniform-layout
//! computer into one block the pass group binds after the inputs and the
//! outputs. The struct the shader reads is *generated* from the
//! declaration and prepended to the module when it compiles — the
//! material parameters' one-spelling rule, pass-level — and the variant
//! key folds the layout, never the values, so a slider is a buffer write.
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

use std::sync::Arc;

use wxsl_core::abi;
use wxsl_core::codegen::{self, GeneratedShader, ScreenOptions};
use wxsl_core::error::CodegenError;
use wxsl_core::graph::Graph;
use wxsl_core::identity;
use wxsl_core::macros::MacroSet;
use wxsl_core::node::{NodeDefinition, NodeRegistry, SettingDef, Socket, Value, ValueType};
use wxsl_core::pipeline::{self as doc, SETTING_POLICY};
use wxsl_core::resources::BufferLayout;
use wxsl_core::wxsl::WxslIdent;

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
/// The `name` is the `pass.screen` socket the document wires — and for a
/// derived `pass.screen.<effect>` node, the socket's own name — and the
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
    /// How many frames back this input reads. `0` is this frame's
    /// contents, which orders the pass after whoever wrote them; anything
    /// else reads a persistent resource's *history*, and creates no
    /// ordering edge — the property that makes a temporal technique
    /// schedulable at all. A resource whose ring is too shallow is the
    /// scheduler's named error, exactly as a hand-built pass list's would
    /// be.
    pub history: u32,
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

/// What shape a non-attachment output writes: the socket type a document
/// wires it through.
///
/// The *binding* still comes from the wired resource's shape, in the
/// pass's `writes`, after every input — the shape here types the socket and
/// is what the pipeline compiler checks the wiring against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectOutputShape {
    /// A storage buffer — wired from a `resource.buffer`.
    Buffer,
    /// A storage texture, written write-only — wired from a
    /// `resource.color`, whose format is the storage format.
    StorageTexture,
}

/// One output an effect writes that is not an attachment: a compute
/// effect's storage target.
///
/// A screen effect writes its attachment and declares no outputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectOutput {
    /// The name the output goes by — the `pass.compute.<effect>` output
    /// socket a document wires.
    pub name: &'static str,
    /// What shape of thing the output writes.
    pub shape: EffectOutputShape,
    /// One line for the palette and for error messages.
    pub description: &'static str,
}

/// One parameter an effect declares: a uniform the host can change without
/// a recompile — the material parameter's story, pass-level (plan3 N4).
///
/// The `name` is the struct field the shader reads and how the host
/// addresses the value; the `default` is both the starting value a fresh
/// buffer is filled with and the parameter's *type* — `Value` carries one
/// and implies the other, which is one less way for a descriptor row to
/// disagree with itself. Bloom's `THRESHOLD` was the `const` this exists to
/// retire: a re-tune used to be a new descriptor row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectParameter {
    /// The field name in the generated struct, and how the host sets it.
    /// A WGSL identifier — it becomes one.
    pub name: &'static str,
    /// The starting value, and the type by implication.
    pub default: Value,
}

impl EffectParameter {
    /// The parameter's type.
    pub fn ty(&self) -> ValueType {
        self.default.ty()
    }
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
#[derive(Clone, Debug, PartialEq)]
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
    /// Generated from a **graph**: a screen graph, or — since
    /// [ADR 0045](../../../docs/adr/0045-a-bake-is-an-effect-over-a-material-subgraph.md)
    /// — a *material subgraph*, a bake. Compiled through
    /// [`wxsl_core::codegen::generate_screen`] or
    /// [`wxsl_core::codegen::generate_bake`].
    ///
    /// The WXSL is generated once, when the effect is built
    /// ([`Effect::from_graph`]), rather than per compile: generation needs
    /// a node registry, and an effect descriptor that carried one would be
    /// a descriptor the renderer had to keep a registry for. What the
    /// renderer sees afterwards is a module path and some text — which is
    /// exactly what [`EffectShader::Source`] is, and why nothing
    /// downstream of the seam had to learn a third case.
    Graph {
        /// The authored graph, kept so an editor can open what it shows
        /// and a document can round-trip it.
        graph: Arc<Graph>,
        /// The WXSL generated from it, mounted at `module`.
        wxsl: Arc<str>,
        /// The module path the text is mounted under —
        /// [`wxsl_core::codegen::SCREEN_MODULE`] or
        /// [`wxsl_core::codegen::BAKE_MODULE`], by what generated it.
        module: &'static str,
    },
}

impl EffectShader {
    /// Where the text is mounted and what it is, for the compiler.
    pub(crate) fn source(&self) -> Option<(&'static str, std::borrow::Cow<'static, str>)> {
        match self {
            EffectShader::Lighting => None,
            EffectShader::Source { path, wxsl } => Some((path, std::borrow::Cow::Borrowed(*wxsl))),
            EffectShader::Graph { wxsl, module, .. } => {
                Some((*module, std::borrow::Cow::Owned(wxsl.to_string())))
            }
        }
    }
}

/// An effect, as data: what it reads and writes, the shader it runs, the
/// entry points a `wgpu` pipeline is built from.
///
/// `Clone` rather than `Copy`: a descriptor effect is a handful of
/// `&'static` fields, but a graph-authored one owns its graph and its
/// generated text behind an [`Arc`], so cloning is a refcount bump rather
/// than a copy of either.
#[derive(Clone, Debug, PartialEq)]
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
    /// The uniform parameters the host can tune at runtime, laid out by
    /// [`Effect::param_layout`] into one uniform block at the pass group's
    /// next binding after the inputs and the outputs
    /// ([ADR 0042](../../../docs/adr/0042-effect-parameters-are-uniforms-the-descriptor-declares-them.md)).
    /// Empty for every effect that has no knobs.
    pub parameters: &'static [EffectParameter],
    /// Where the shader text comes from.
    pub shader: EffectShader,
}

impl Effect {
    /// Build a screen effect from a screen graph (ADR 0040).
    ///
    /// The graph is compiled to WXSL here and now, so that a graph that
    /// does not generate is a failure at registration rather than at the
    /// first frame that wanted the pass. What comes back is an ordinary
    /// effect: one image input, the generated module's entry points, and a
    /// shader the renderer compiles exactly as it compiles a file's.
    ///
    /// The one input is fixed because the screen ABI's is
    /// ([`abi::SCREEN_IMAGE_VAR`]): a graph cannot yet declare what the
    /// pipeline must wire into it, so it reads the one image every shipped
    /// screen effect already reads.
    pub fn from_graph(
        id: &'static str,
        label: &'static str,
        description: &'static str,
        graph: Graph,
        registry: &NodeRegistry,
    ) -> Result<Effect, CodegenError> {
        let generated: GeneratedShader =
            codegen::generate_screen(&graph, registry, &ScreenOptions::default())?;
        Ok(Effect {
            id,
            label,
            description,
            kind: EffectKind::Screen {
                vertex_entry: abi::SCREEN_VERTEX_ENTRY,
                fragment_entry: abi::SCREEN_FRAGMENT_ENTRY,
            },
            inputs: SCREEN_GRAPH_INPUTS,
            outputs: &[],
            parameters: &[],
            shader: EffectShader::Graph {
                graph: Arc::new(graph),
                wxsl: Arc::from(generated.source),
                // The nominal mount, not the ABI's own path: the generated
                // module *imports* `abi::SCREEN_MODULE`, and mounting it
                // there would be a module importing itself.
                module: codegen::SCREEN_MODULE,
            },
        })
    }

    /// Build a *bake* effect from a material graph's bake declaration
    /// ([ADR 0045](../../../docs/adr/0045-a-bake-is-an-effect-over-a-material-subgraph.md)):
    /// the descriptor's shader is generated from the subgraph the
    /// declaration names, the way a material module is generated from the
    /// graph, and the effect is an ordinary compute effect from there on —
    /// one declared output (the table, wired from a `resource.color` the
    /// document labels with the declaration's texture name), one dispatch
    /// whose shape is the table's.
    ///
    /// `texture` is the declaration's name — how an application addresses
    /// a bake without knowing node ids, and the same string the pipeline's
    /// `resource.color` must be labelled and the host must bind the
    /// created table under. `macros` are what the module is generated at;
    /// keep them equal to what the *material* compiles the same subgraph
    /// under, or the two arms of the toggle disagree.
    ///
    /// A graph that does not generate fails here, at registration, rather
    /// than at the first frame — the cone's purity among the rest.
    ///
    /// The graph is borrowed, not taken: the caller almost always keeps it
    /// — the material is compiled from the same document — and an `Arc`
    /// behind the descriptor makes sharing free.
    pub fn from_bake(
        id: &'static str,
        label: &'static str,
        description: &'static str,
        graph: &Graph,
        texture: &str,
        macros: &MacroSet,
        registry: &NodeRegistry,
    ) -> Result<Effect, CodegenError> {
        let decl = graph.bake(texture).ok_or_else(|| {
            CodegenError::Invalid(wxsl_core::error::GraphErrors(vec![
                wxsl_core::error::GraphError::InvalidBake {
                    texture: texture.to_string(),
                    reason: "no bake declaration names this texture".to_string(),
                },
            ]))
        })?;
        let generated = codegen::generate_bake(
            graph,
            decl,
            registry,
            &wxsl_core::codegen::BakeOptions {
                macros: macros.clone(),
                ..wxsl_core::codegen::BakeOptions::default()
            },
        )?;
        Ok(Effect {
            id,
            label,
            description,
            kind: EffectKind::Compute {
                entry: generated.entry,
                workgroups: generated.workgroups,
            },
            inputs: &[],
            outputs: &[EffectOutput {
                name: "bake",
                shape: EffectOutputShape::StorageTexture,
                description: "The bake table this effect fills, one value per texel.",
            }],
            parameters: &[],
            shader: EffectShader::Graph {
                graph: Arc::new(graph.clone()),
                wxsl: Arc::from(generated.source),
                module: abi::BAKE_MODULE,
            },
        })
    }

    /// The screen graph behind this effect, if it has one.
    pub fn graph(&self) -> Option<&Graph> {
        match &self.shader {
            EffectShader::Graph { graph, .. } => Some(graph),
            _ => None,
        }
    }

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

    /// Whether this screen effect's wiring fits the fixed `pass.screen`
    /// socket set: at most one image and at most one buffer, which is all
    /// that node can name. Effects that do not — TAA's three images, the
    /// blur's two — are placed as their derived `pass.screen.<id>` row
    /// instead, whose sockets *are* the declaration, exactly as a compute
    /// pass's are.
    pub fn fits_pass_screen(&self) -> bool {
        let images = self
            .inputs
            .iter()
            .filter(|input| input.kind == EffectInputKind::Image)
            .count();
        let buffers = self
            .inputs
            .iter()
            .filter(|input| input.kind == EffectInputKind::Buffer)
            .count();
        images <= 1 && buffers <= 1
    }

    /// The parameters, laid out for the uniform block the pass group
    /// binds: one field per declared parameter, under the same WGSL
    /// uniform rules every host-shared buffer here follows.
    ///
    /// This is ADR 0023's layout computer with its second customer: the
    /// offsets are computed here and nowhere else — the shader's struct is
    /// *generated from it* ([`Effect::params_header`]) and the host writes
    /// *through it*, so there are not two spellings to drift apart. The
    /// signature of the result is what a variant key folds, and it holds
    /// names, types and offsets — never values, or every slider would be
    /// a recompile again.
    ///
    /// A parameter's name has to be a WGSL identifier, because it becomes
    /// a struct field; the descriptor rows are static, reviewed data, so a
    /// name that is not one is a bug at rest and said so where it sits.
    pub(crate) fn param_layout(&self) -> BufferLayout {
        let fields = self.parameters.iter().map(|parameter| {
            (
                WxslIdent::new(parameter.name).unwrap_or_else(|| {
                    panic!(
                        "effect `{}` declares parameter `{}`, which is not a WGSL identifier",
                        self.id, parameter.name
                    )
                }),
                parameter.ty(),
            )
        });
        BufferLayout::uniform(fields)
    }

    /// The WGSL declaring the parameter block — struct and `var<uniform>`
    /// at the pass group's next binding after the inputs and the outputs —
    /// or an empty string for an effect with no parameters.
    ///
    /// Prepended to the module when the variant compiles, so the effect's
    /// file reads `params.threshold` and never states the struct itself:
    /// the descriptor is the one declaration, and this text is its shadow.
    pub(crate) fn params_header(&self) -> String {
        if self.parameters.is_empty() {
            return String::new();
        }
        // The id carries a package segment (`wxsl.bloom`) the shader
        // cannot spell; the struct names the package with an underscore.
        let name = self.id.replace('.', "_");
        let layout = self.param_layout();
        let mut header = layout.wgsl_struct(&format!("{name}_params"));
        let _ = std::fmt::Write::write_fmt(
            &mut header,
            format_args!(
                "@group(3) @binding({}) var<uniform> params: {name}_params;\n",
                self.inputs.len() + self.outputs.len(),
            ),
        );
        header
    }
}

/// The inputs every graph-authored screen effect declares: the one image
/// the screen ABI binds.
const SCREEN_GRAPH_INPUTS: &[EffectInput] = &[EffectInput {
    name: "image",
    kind: EffectInputKind::Image,
    description: "The image this effect reads, as linear radiance.",
    history: 0,
}];

/// The deferred lighting pass, as an effect: the first one, migrated out
/// of the hardcoded enum it was reachable only through. Its generated
/// module and its bindings are byte-for-byte what they were.
pub const DEFERRED_LIGHTING: Effect = Effect {
    id: "wxsl.deferred_lighting",
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
        history: 0,
    }],
    outputs: &[],
    parameters: &[],
    shader: EffectShader::Lighting,
};

/// Bloom: glow for the bright parts of an image, the proof that effects
/// are real units — one descriptor row, one shader file, and any pipeline
/// document can wire it into a chain. The first effect with parameters
/// (ADR 0042): the three knobs its shader used to `const` into itself are
/// the descriptor's declaration now, at the same values, so a re-tune is
/// a buffer write instead of a new row.
pub const BLOOM: Effect = Effect {
    id: "wxsl.bloom",
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
        history: 0,
    }],
    outputs: &[],
    parameters: &[
        EffectParameter {
            name: "threshold",
            // In linear radiance: diffuse white is the line, and what glows
            // is what is brighter than a white surface fully lit — which is
            // what "highlight" means.
            default: Value::F32(1.0),
        },
        EffectParameter {
            name: "knee",
            // How far above `threshold` the ramp to "fully kept" runs.
            default: Value::F32(0.6),
        },
        EffectParameter {
            name: "strength",
            // How much of the blurred bright signal is added back.
            default: Value::F32(0.85),
        },
    ],
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
    id: "wxsl.tonemap",
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
        history: 0,
    }],
    outputs: &[],
    parameters: &[],
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
    id: "wxsl.brdf_lut",
    label: "BRDF LUT",
    description: "Bake the split-sum environment-BRDF LUT, once.",
    kind: EffectKind::Compute {
        entry: "bake_lut",
        workgroups: [8, 8, 1],
    },
    inputs: &[],
    outputs: &[EffectOutput {
        name: "lut",
        shape: EffectOutputShape::StorageTexture,
        description: "The LUT: a 64x64 storage texture of (scale, bias).",
    }],
    parameters: &[],
    shader: EffectShader::Source {
        path: BRDF_LUT_MODULE,
        wxsl: include_str!("../shaders/brdf_lut.wxsl"),
    },
};

/// The LUT viewer: one image, stretched over the frame's target — the
/// screen half of the [`BRDF_LUT`] proof, and how a demo or a test looks
/// at what a once-only bake left behind.
pub const LUT_VIEW: Effect = Effect {
    id: "wxsl.lut_view",
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
        history: 0,
    }],
    outputs: &[],
    parameters: &[],
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
    id: "wxsl.ramp_fill",
    label: "ramp fill",
    description: "Fill a storage buffer with an eased ramp, one f32 per entry.",
    kind: EffectKind::Compute {
        entry: "fill_ramp",
        workgroups: [4, 1, 1],
    },
    inputs: &[],
    outputs: &[EffectOutput {
        name: "ramp",
        shape: EffectOutputShape::Buffer,
        description: "The buffer: 256 f32 values, a smoothstep ease of the index.",
    }],
    parameters: &[],
    shader: EffectShader::Source {
        path: RAMP_MODULE,
        wxsl: include_str!("../shaders/ramp.wxsl"),
    },
};

/// The buffer reader: a screen effect that draws a storage buffer as one
/// value per screen column — the half that proves a fragment stage can
/// consume what a compute pass wrote, with no texture in between.
pub const RAMP_VIEW: Effect = Effect {
    id: "wxsl.ramp_view",
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
        history: 0,
    }],
    outputs: &[],
    parameters: &[],
    shader: EffectShader::Source {
        path: RAMP_VIEW_MODULE,
        wxsl: include_str!("../shaders/ramp_view.wxsl"),
    },
};

/// Module path the TAA resolve's shader is mounted under.
pub const TAA_MODULE: &str = "package::wxsl::taa";
/// Module path the motion blur's shader is mounted under.
pub const MOTION_BLUR_MODULE: &str = "package::wxsl::motion_blur";

/// TAA: the temporal resolve at the end of a policy'd chain — the
/// velocity stage's reason to exist (plan3 N2). Three inputs, which is
/// exactly why the fixed `pass.screen` socket set cannot carry it: the
/// colour, the velocity, and *last frame's own output*, read a frame
/// back through a persistent resource's history so the pass orders
/// against the scene but never against itself. Writing the history and
/// reading it is the shape `reading_history_is_not_an_ordering_edge` is
/// about.
pub const TAA: Effect = Effect {
    id: "wxsl.taa",
    label: "TAA",
    description: "Blend the frame with its reprojected history: antialias, \
                  at the cost of one ring.",
    kind: EffectKind::Screen {
        vertex_entry: "taa_vs",
        fragment_entry: "taa_fs",
    },
    inputs: &[
        EffectInput {
            name: "color",
            kind: EffectInputKind::Image,
            description: "The frame, as linear radiance.",
            history: 0,
        },
        EffectInput {
            name: "velocity",
            kind: EffectInputKind::Image,
            description: "The velocity stage's screen motion, uv per frame.",
            history: 0,
        },
        EffectInput {
            name: "history",
            kind: EffectInputKind::Image,
            description: "Last frame's resolve — the same resource `into` names.",
            history: 1,
        },
    ],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "blend",
        // How much of the clamped history survives. 1 would never accept
        // new light; 0 is no history at all.
        default: Value::F32(0.9),
    }],
    shader: EffectShader::Source {
        path: TAA_MODULE,
        wxsl: include_str!("../shaders/taa.wxsl"),
    },
};

/// Motion blur: sample back along each fragment's own screen motion — the
/// first consumer of the velocity buffer that is not TAA, and the second
/// proof that a two-image effect cannot sit on the fixed `pass.screen`
/// socket set. Reads linear radiance, before the display transform, which
/// is the only place averaging brightness means anything.
pub const MOTION_BLUR: Effect = Effect {
    id: "wxsl.motion_blur",
    label: "motion blur",
    description: "Smear the frame along its own velocity buffer.",
    kind: EffectKind::Screen {
        vertex_entry: "motion_blur_vs",
        fragment_entry: "motion_blur_fs",
    },
    inputs: &[
        EffectInput {
            name: "color",
            kind: EffectInputKind::Image,
            description: "The frame, as linear radiance.",
            history: 0,
        },
        EffectInput {
            name: "velocity",
            kind: EffectInputKind::Image,
            description: "The velocity stage's screen motion, uv per frame.",
            history: 0,
        },
    ],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "strength",
        // 1 is a full frame's motion smeared across the exposure; 0 is
        // the image exactly as it was.
        default: Value::F32(1.0),
    }],
    shader: EffectShader::Source {
        path: MOTION_BLUR_MODULE,
        wxsl: include_str!("../shaders/motion_blur.wxsl"),
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
    /// display transform every stock chain ends in, bloom, the TAA
    /// resolve, and the motion blur.
    pub fn shipped() -> Self {
        EffectRegistry {
            effects: vec![DEFERRED_LIGHTING, TONEMAP, BLOOM, TAA, MOTION_BLUR],
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
    ///
    /// # Panics
    ///
    /// Panics when the id carries no package segment: effect ids are
    /// `package.name` (`wxsl.bloom`), and an un-namespaced registration is
    /// how two libraries' effects end up unable to share a registry.
    /// Documents may still spell shipped ids bare — [`Self::get`] resolves
    /// `bloom` to `wxsl.bloom` — but a registration says who owns the
    /// name. See [`wxsl_core::identity`].
    pub fn add(&mut self, effect: Effect) {
        assert!(
            identity::is_namespaced(effect.id),
            "effect `{}` has no package segment — ids are `package.name` \
             (the shipped effects live under `wxsl.`); put it in a package of its own",
            effect.id,
        );
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

    /// The effect named by `id`, as documents name them: an id with a
    /// package is taken as written, a bare one resolves against the
    /// shipped package — `bloom` means `wxsl.bloom`, which is what every
    /// document written before namespacing meant.
    pub fn get(&self, id: &str) -> Option<Effect> {
        let resolved = identity::resolve(id);
        self.effects
            .iter()
            .find(|effect| effect.id == resolved.as_ref())
            .cloned()
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

    /// The document-vocabulary rows the compute effects add: one
    /// `pass.compute.<effect>` definition per registered compute effect,
    /// its sockets the effect's declared inputs and outputs (plan3 N3).
    ///
    /// `pass.screen`'s socket set is fixed and maps a declaration onto it
    /// by kind — that works because a *screen* effect's wiring is the
    /// frame: one G-buffer, one image, one write. A compute effect's
    /// wiring is whatever its work needs, so its sockets are derived from
    /// its own declaration rather than mapped onto a fixed set — the
    /// mechanism, one level up, that lets an effect want two writes and
    /// still be a node. Screen effects keep the fixed pass; every shipped
    /// compute effect joins the vocabulary through this method, and an
    /// application's join the same way.
    ///
    /// A declared *output* is an input socket on the node, as `into` is on
    /// a screen pass: the wire names the storage being written, and the
    /// storage — a `resource.color` or `resource.buffer` — is what any
    /// later reader wires from.
    pub fn node_defs(&self) -> Vec<NodeDefinition> {
        self.effects
            .iter()
            .filter(|effect| effect.is_compute())
            .map(|effect| {
                let mut def = NodeDefinition::builder(
                    format!("{}{}", doc::PASS_COMPUTE_PREFIX, effect.id),
                    effect.label,
                )
                .doc(format!(
                    "{} Reads: {}. Writes: {}.",
                    effect.description,
                    declarations(
                        effect
                            .inputs
                            .iter()
                            .map(|input| (input.name, input.description))
                    ),
                    declarations(
                        effect
                            .outputs
                            .iter()
                            .map(|output| (output.name, output.description))
                    ),
                ))
                .setting(policy_setting());
                for input in effect.inputs {
                    def = def.input(
                        Socket::new(input.name, input_socket_type(input.kind))
                            .with_doc(input.description),
                    );
                }
                for output in effect.outputs {
                    def = def.input(
                        Socket::new(output.name, output_socket_type(output.shape))
                            .with_doc(format!("written: {}", output.description)),
                    );
                }
                def.document()
            })
            .chain(
                // The screen pass's own derived rows, for the effects whose
                // wiring the fixed socket set cannot name: one image socket
                // per declared image input, under the input's own name,
                // beside `into` and the colour hand-off. The TAA resolve's
                // `history` socket is where a document says "last frame's"
                // — the history lives on the *input*, and the resource it
                // names is the one `into` writes.
                self.effects
                    .iter()
                    .filter(|effect| !effect.is_compute() && !effect.fits_pass_screen())
                    .map(|effect| {
                        let mut def = NodeDefinition::builder(
                            format!("{}{}", doc::PASS_SCREEN_PREFIX, effect.id),
                            effect.label,
                        )
                        .doc(format!(
                            "{} Reads: {}.",
                            effect.description,
                            declarations(
                                effect
                                    .inputs
                                    .iter()
                                    .map(|input| (input.name, input.description))
                            ),
                        ))
                        .input(
                            Socket::new("into", ValueType::ColorTarget)
                                .optional()
                                .with_doc(
                                    "Where to write. Unconnected means the frame's own target.",
                                ),
                        )
                        .output(Socket::new("color", ValueType::ColorTarget).with_doc(
                            "What the effect wrote — the frame's target when \
                                 `into` is unconnected.",
                        ))
                        .setting(policy_setting());
                        for input in effect.inputs {
                            def = def.input(
                                Socket::new(input.name, input_socket_type(input.kind))
                                    .with_doc(input.description),
                            );
                        }
                        def.document()
                    }),
            )
            .collect()
    }
}

/// The socket type a declared input wires through.
fn input_socket_type(kind: EffectInputKind) -> ValueType {
    match kind {
        EffectInputKind::GBuffer => ValueType::GBuffer,
        EffectInputKind::Image => ValueType::ColorTarget,
        EffectInputKind::Buffer => ValueType::StorageBuffer,
    }
}

/// The socket type a declared output wires through.
fn output_socket_type(shape: EffectOutputShape) -> ValueType {
    match shape {
        EffectOutputShape::Buffer => ValueType::StorageBuffer,
        EffectOutputShape::StorageTexture => ValueType::ColorTarget,
    }
}

/// `` `name` (description)`` — `` `ramp` (the buffer, 256 f32 values)`` —
/// for a compute definition's doc line.
fn declarations<'a>(declared: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let parts: Vec<String> = declared
        .map(|(name, description)| format!("`{name}` ({description})"))
        .collect();
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(", ")
    }
}

/// The `policy` setting every pass node carries.
fn policy_setting() -> SettingDef {
    SettingDef::new(
        SETTING_POLICY,
        "Policy",
        "How often the pass runs: per frame, once, on resize or on demand. Only a \
         pass whose target survives frames may skip.",
    )
    .with_default("per frame")
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
        assert_eq!(
            registry.len(),
            5,
            "lighting, tonemap, bloom — and, since plan3 N2, the TAA resolve \
             and the motion blur"
        );

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
        assert_eq!(registry.len(), 5, "replacing, not appending");
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

    #[test]
    fn bloom_declares_its_knobs_and_the_shader_reads_them_without_stating_them() {
        // The parameter half of the descriptor-vs-shader contract, checked
        // where a change to either side fails in seconds: the descriptor
        // names three f32 knobs with the values the shader's `const`s used
        // to carry, and the file reads them through the generated `params`
        // struct without declaring it — the struct is the declaration's
        // shadow, and a second copy of it in the file is drift.
        assert_eq!(
            BLOOM
                .parameters
                .iter()
                .map(|parameter| (parameter.name, parameter.ty()))
                .collect::<Vec<_>>(),
            [
                ("threshold", ValueType::F32),
                ("knee", ValueType::F32),
                ("strength", ValueType::F32),
            ]
        );
        let EffectShader::Source { path: _, wxsl } = BLOOM.shader else {
            panic!("bloom ships its source");
        };
        for knob in ["params.threshold", "params.knee", "params.strength"] {
            assert!(wxsl.contains(knob), "bloom.wxsl reads `{knob}`");
        }
        assert!(
            !wxsl.contains("var<uniform>"),
            "the file must not state the block the descriptor declares"
        );
        assert!(
            !wxsl.contains("const THRESHOLD"),
            "the const this retired stays retired"
        );
        // Every other shipped effect declares no knobs, so nothing about
        // them moves.
        for effect in [
            DEFERRED_LIGHTING,
            TONEMAP,
            BRDF_LUT,
            LUT_VIEW,
            RAMP_FILL,
            RAMP_VIEW,
        ] {
            assert!(
                effect.parameters.is_empty(),
                "`{}` grew knobs silently",
                effect.id
            );
        }
    }

    #[test]
    fn the_parameter_header_declares_the_struct_at_the_next_binding() {
        // One f32 input, no outputs — so the uniform block lands at
        // binding 1, after the inputs and the outputs, and the struct name
        // is the effect's to own.
        let header = BLOOM.params_header();
        assert!(header.contains("struct wxsl_bloom_params {"), "{header}");
        assert!(
            header.contains("@group(3) @binding(1) var<uniform> params: wxsl_bloom_params;"),
            "{header}"
        );
        // Laid out by the same computer every host-shared buffer follows,
        // so the size is the uniform address space's answer for three
        // f32s.
        assert_eq!(BLOOM.param_layout().size(), 16);
        // And an effect with no parameters generates nothing at all, so
        // its module compiles byte-for-byte as it did.
        assert!(TONEMAP.params_header().is_empty());
        assert!(TONEMAP.param_layout().is_empty());
    }

    #[test]
    fn the_parameter_layout_folds_names_and_types_never_values() {
        // The material parameters' precedent, pass-level: two descriptors
        // that differ only in a default are one layout and therefore one
        // variant — the default is what a fresh *buffer* starts at, not
        // what the shader was compiled with — while a different type is a
        // different shader.
        let retuned = Effect {
            parameters: &[
                EffectParameter {
                    name: "threshold",
                    default: Value::F32(0.5),
                },
                EffectParameter {
                    name: "knee",
                    default: Value::F32(0.6),
                },
                EffectParameter {
                    name: "strength",
                    default: Value::F32(0.85),
                },
            ],
            ..BLOOM
        };
        assert_eq!(BLOOM.param_layout(), retuned.param_layout());
        let widened = Effect {
            parameters: &[
                EffectParameter {
                    name: "threshold",
                    default: Value::Vec2([1.0, 1.0]),
                },
                EffectParameter {
                    name: "knee",
                    default: Value::F32(0.6),
                },
                EffectParameter {
                    name: "strength",
                    default: Value::F32(0.85),
                },
            ],
            ..BLOOM
        };
        assert_ne!(BLOOM.param_layout(), widened.param_layout());
    }

    #[test]
    fn every_compute_effect_grows_a_node_from_its_declaration() {
        // The document vocabulary is open: a compute effect joins it as a
        // `pass.compute.<id>` node whose sockets are the declaration. The
        // LUT bake writes a storage texture through a colour-target
        // socket; the ramp fill writes a buffer through a storage-buffer
        // socket; both carry the policy setting every pass node has.
        let registry = EffectRegistry::empty()
            .with(BRDF_LUT)
            .with(RAMP_FILL)
            .with(TONEMAP);
        let defs = registry.node_defs();
        assert_eq!(
            defs.len(),
            2,
            "the compute effects get nodes; the screen effect does not"
        );

        let bake = defs
            .iter()
            .find(|def| def.id == "pass.compute.wxsl.brdf_lut")
            .expect("the bake's node");
        assert_eq!(bake.category, "pass", "grouped with the other passes");
        let lut_socket = bake.input("lut").expect("the bake's declared output");
        assert_eq!(lut_socket.ty, ValueType::ColorTarget);
        assert!(bake.output("lut").is_none(), "a write is wired in, not out");
        assert!(
            bake.setting(SETTING_POLICY).is_some(),
            "the node carries the policy setting"
        );

        let fill = defs
            .iter()
            .find(|def| def.id == "pass.compute.wxsl.ramp_fill")
            .expect("the ramp fill's node");
        let ramp_socket = fill.input("ramp").expect("the buffer socket");
        assert_eq!(ramp_socket.ty, ValueType::StorageBuffer);
        assert!(
            !ramp_socket.optional,
            "a write declaration is mandatory — the pass must know what it writes"
        );
        assert_eq!(
            fill.inputs.len(),
            1,
            "an effect with no declared inputs declares nothing else"
        );
    }
}
