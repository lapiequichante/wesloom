//! The pipeline compiler: a [`Graph`] of document nodes in, a
//! [`RenderGraph`] out — one pure function, no device.
//!
//! This is the layer between "a pipeline the user edits as a graph" and "a
//! pass list the engine schedules"
//! ([ADR 0033](../../../docs/adr/0033-pipelines-are-documents.md)). The
//! document ([`wxsl_core::pipeline`]) is typed and acyclic by the graph
//! model; what this module adds is everything the *engine* needs that the
//! graph model cannot say: that a `gbuffer`-stage pass must be wired to a
//! G-buffer, that exactly one pass may write the frame's target, that
//! `pair` means `Rg16Float`. Every failure names the document node,
//! because the reader is a user looking at a canvas — the scheduler's own
//! checks stay as the last line, and [`RenderGraph::schedule`] is
//! untouched.
//!
//! # What is derived, and from what
//!
//! Documents carry less than pass lists on purpose. The compiler derives
//! the rest from the shape of the wiring, so the document says *what*, not
//! *how*:
//!
//! * **Depth clear vs. load.** A pass taking depth straight from a
//!   `resource.depth` node *clears* it; a pass taking another pass's depth
//!   output *tests against what is there* without writing — the whole
//!   depth-prepass idiom, expressed as a wire.
//! * **Attachment clears.** An intermediate target clears to transparent;
//!   the pass writing the frame's target clears to the config's clear
//!   colour, because that is the one pass writing what the user sees.
//! * **Depth state.** A pass that loads depth runs `LessEqual` and writes
//!   none — tolerating the one-ulp difference between two pipelines' clip
//!   positions (see `forward_graph`); a pass that clears depth runs the
//!   opaque defaults.
//! * **Attachment counts.** A stage's wiring *is* its attachment list:
//!   `forward_lit` writes the frame's target through its colour output,
//!   `gbuffer` writes the G-buffer it is wired to, `depth_only` writes no
//!   colour at all — and the scheduler re-checks every count against the
//!   stage table, as always.
//!
//! # Precisions and effects are names, not formats
//!
//! The document never spells a `wgpu` format — `resource.color` says
//! `standard` or `hdr`, and [`gbuffer_format`] is the mirror. The same for
//! screen effects: `pass.screen` names one by id, and the
//! [`EffectRegistry`] is the table of what this crate knows how to run —
//! the migrated lighting pass and bloom, plus whatever an application
//! registers (plan2 P4). The effect's declared inputs are the contract:
//! what is wired into the pass must match them, binding for binding. An
//! effect chain through `resource.color` is how a pipeline composes —
//! lighting writes a colour target, a post effect reads it and writes the
//! frame's target by leaving `into` unconnected — and a pass that tries to
//! feed its *output* (the presented target) into another effect's image
//! input is the named error [`PipelineError::ImageFromPass`].

use std::collections::HashMap;

use wxsl_core::abi::{self, MaterialStage};
#[cfg(test)]
use wxsl_core::graph::Node;
use wxsl_core::graph::{Graph, NodeId, SocketRef};
use wxsl_core::node::NodeRegistry;
use wxsl_core::pipeline as doc;
use wxsl_core::scene::TagExpr;

use crate::effect::{Effect, EffectInputKind, EffectRegistry};
use crate::graph::RenderGraph;
use crate::pass::{
    Attachment, DepthAttachment, Dimension, DrawSource, Extent, PassDesc, PassState, PassView,
    Persistence, Policy, Read, ResourceDesc, ResourceId, DEPTH_FORMAT,
};
#[cfg(test)]
use crate::pipeline::StockPipeline;
use crate::pipeline::{gbuffer_format, PipelineConfig};

/// What is wrong with a pipeline document.
///
/// Every variant names the document node at fault, by its display label —
/// the string the canvas shows.
#[derive(Clone, Debug, PartialEq)]
pub enum PipelineError {
    /// The document did not validate as a graph: an unfed mandatory input,
    /// a cycle, a type mismatch.
    InvalidDocument(wxsl_core::GraphErrors),
    /// `pass.screen` names an effect this crate does not know.
    UnknownEffect {
        /// The pass node.
        node: String,
        /// The effect id it named.
        effect: String,
        /// The ids that exist.
        known: Vec<String>,
    },
    /// `resource.color` names a size that is neither `viewport` nor
    /// `WxH` pixels.
    UnknownSize {
        /// The resource node.
        node: String,
        /// The size it named.
        size: String,
    },
    /// An imported `resource.color` — a texture the host owns — was also
    /// given a size. Whoever created the texture sized it; a document
    /// saying both would only ever be obeyed on one.
    ImportedTargetSized {
        /// The resource node.
        node: String,
        /// The size setting that says nothing here.
        setting: String,
        /// The value it carries.
        value: String,
    },
    /// `resource.buffer` names a size that is not a positive number of
    /// bytes.
    BadBufferSize {
        /// The resource node.
        node: String,
        /// The value that would not do.
        value: String,
    },
    /// `pass.geometry` names a stage that is not a material stage a
    /// document can draw. The shadow stage is among these on purpose: the
    /// shadow passes come from `pass.shadow`, which expands to one pass
    /// per light slot.
    UnknownStage {
        /// The pass node.
        node: String,
        /// The stage name it named.
        stage: String,
    },
    /// `resource.color` names a precision that is not one.
    UnknownPrecision {
        /// The resource node.
        node: String,
        /// The precision it named.
        precision: String,
    },
    /// A pass names a [`crate::pass::Policy`] that is not one.
    UnknownPolicy {
        /// The pass node.
        node: String,
        /// The policy it named.
        policy: String,
    },
    /// A numeric setting is not a number.
    BadNumber {
        /// The node.
        node: String,
        /// Which setting.
        setting: &'static str,
        /// The value that would not parse.
        value: String,
    },
    /// A `gbuffer`-stage pass is not wired to a G-buffer.
    GbufferWithoutGbuffer {
        /// The pass node.
        node: String,
    },
    /// A pass is wired to a G-buffer *and* to a separate depth target.
    /// The G-buffer carries its own depth — one or the other.
    DepthConflict {
        /// The pass node.
        node: String,
    },
    /// A `forward_lit` pass writes its colour nowhere.
    ColorWithoutConsumer {
        /// The pass node.
        node: String,
    },
    /// A pass of a stage that writes no colour has its colour output
    /// wired anyway.
    ColorFromColorlessStage {
        /// The pass node.
        node: String,
        /// The stage it runs.
        stage: String,
    },
    /// A stage that needs depth — `forward_lit` or `depth_only` — has no
    /// depth wired.
    PassWithoutDepth {
        /// The pass node.
        node: String,
    },
    /// A scene source's `tags` setting is not a tag expression.
    BadTags {
        /// The source node.
        node: String,
        /// The text that would not parse.
        value: String,
        /// Why it would not.
        reason: String,
    },
    /// A pass's depth output is consumed, but the pass attaches no depth.
    DepthOutputWithoutDepth {
        /// The pass whose depth output was read.
        node: String,
    },
    /// A screen effect's declared inputs do not match what is wired into
    /// the pass.
    EffectInputMismatch {
        /// The pass node.
        node: String,
        /// The effect id.
        effect: String,
        /// What is wrong.
        reason: String,
    },
    /// `pass.screen`'s write target is another pass's output rather than a
    /// resource — a pass writes where it is told, and another pass's
    /// output is not a target anybody owns.
    IntoFromPass {
        /// The pass doing the writing.
        node: String,
    },
    /// A screen effect's `image` input is fed by a pass whose colour
    /// output is the frame's target. The target is what is *on screen* —
    /// it cannot be presented and sampled in the same frame — so a chain
    /// passes along a `resource.color` instead.
    ImageFromPass {
        /// The pass doing the reading.
        node: String,
        /// The pass whose output was wired in.
        fed_by: String,
    },
    /// The document has no `present` node, so nothing reaches the frame.
    NoPresent,
    /// The document has more than one `present` node.
    TwoPresents {
        /// Every `present` node's label.
        nodes: Vec<String>,
    },
    /// `present` is fed by something no pass wrote this frame — an
    /// intermediate resource. Present shows the frame's target, and the
    /// pass that writes it feeds present directly.
    NotPresentable {
        /// The present node.
        node: String,
        /// What was wired into it.
        fed_by: String,
        /// What that feeder wrote, when it is a pass: the intermediate
        /// that cannot be presented.
        wrote: Option<String>,
    },
    /// Two passes write the frame's target. One picture per frame.
    TwoTargetWriters {
        /// The first writer's label.
        first: String,
        /// The second writer's label.
        second: String,
    },
    /// Two `source.lights` nodes — two shadow-map arrays, and the frame
    /// group binds exactly one.
    TwoShadowSources {
        /// Both nodes' labels.
        nodes: Vec<String>,
    },
    /// The config's channel requests collide — a feature and a model (or
    /// two features) claiming one G-buffer field. The pipeline compiler
    /// needs one plan to declare resources from, so the collision is
    /// named here, by the node that made the document notice it.
    ChannelCollision {
        /// The `resource.gbuffer` node being declared.
        node: String,
        /// The rendered collision.
        error: String,
    },
    /// A pass writes into another pass's output. A pass writes where it
    /// is told, and another pass's output is not a target anybody owns —
    /// the compute spelling of the screen rule.
    WriteIntoPassOutput {
        /// The pass doing the writing.
        node: String,
    },
    /// A velocity-stage pass writes a target that is not a pair of signed
    /// half floats. Motion is a difference — negative, and worth half
    /// floats — so `precision: "pair"` is the one `resource.color` the
    /// fragment's `vec2f` output agrees with, and the document names its
    /// mistake rather than leaving it to a `wgpu` validation error about
    /// formats.
    VelocityTargetFormat {
        /// The pass doing the writing.
        node: String,
        /// The format the wired target actually is.
        format: String,
    },
}

impl core::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PipelineError::InvalidDocument(errors) => write!(f, "{errors}"),
            PipelineError::UnknownEffect {
                node,
                effect,
                known,
            } => write!(
                f,
                "pass `{node}` runs `{effect}`, which is not an effect this \
                 renderer knows (it knows: {})",
                known.join(", ")
            ),
            PipelineError::UnknownSize { node, size } => write!(
                f,
                "colour target `{node}` asks for size `{size}`, which is not one — \
                 `viewport`, or fixed pixels as `64x64`"
            ),
            PipelineError::ImportedTargetSized {
                node,
                setting,
                value,
            } => write!(
                f,
                "colour target `{node}` is imported — the host owns the texture — \
                 so its `{setting}` setting (`{value}`) says nothing; whoever \
                 created the texture sized it"
            ),
            PipelineError::BadBufferSize { node, value } => write!(
                f,
                "buffer `{node}` asks for `{value}` bytes, which is not a size — \
                 a storage buffer needs a positive number of bytes"
            ),
            PipelineError::UnknownStage { node, stage }
                if stage == MaterialStage::SHADOW.name() =>
            {
                write!(
                    f,
                    "material pass `{node}` draws stage `{stage}`; the shadow passes come \
                     from a `pass.shadow` node, which fills one slice per light"
                )
            }
            PipelineError::UnknownStage { node, stage } => write!(
                f,
                "material pass `{node}` draws stage `{stage}`, which is not one — \
                 forward_lit, gbuffer or depth_only"
            ),
            PipelineError::UnknownPrecision { node, precision } => write!(
                f,
                "colour target `{node}` asks for precision `{precision}`, which is not \
                 one — standard, hdr, scalar or pair"
            ),
            PipelineError::UnknownPolicy { node, policy } => write!(
                f,
                "pass `{node}` runs {policy:?}, which is not a policy — per frame, once, \
                 on resize or on demand"
            ),
            PipelineError::BadNumber {
                node,
                setting,
                value,
            } => {
                write!(
                    f,
                    "node `{node}` has `{setting}` = `{value}`, which is not a number"
                )
            }
            PipelineError::GbufferWithoutGbuffer { node } => write!(
                f,
                "material pass `{node}` writes a G-buffer, but no G-buffer is wired into it"
            ),
            PipelineError::DepthConflict { node } => write!(
                f,
                "material pass `{node}` is wired both to a G-buffer and to a separate \
                 depth target — the G-buffer carries its own depth, so one or the other"
            ),
            PipelineError::ColorWithoutConsumer { node } => write!(
                f,
                "material pass `{node}` shades lit geometry, but its colour output goes \
                 nowhere — wire it into `present`"
            ),
            PipelineError::ColorFromColorlessStage { node, stage } => write!(
                f,
                "material pass `{node}` runs stage `{stage}`, which writes no colour — \
                 leave its colour output and its `into` unconnected"
            ),
            PipelineError::PassWithoutDepth { node } => write!(
                f,
                "material pass `{node}` needs a depth target — wire one from \
                 `resource.depth`, or from the pass that wrote it"
            ),
            PipelineError::BadTags {
                node,
                value,
                reason,
            } => write!(
                f,
                "scene source `{node}` filters `{value}`, which is not a tag \
                 expression: {reason}"
            ),
            PipelineError::DepthOutputWithoutDepth { node } => write!(
                f,
                "material pass `{node}` attaches no depth, so its depth output carries \
                 nothing — wire depth into it before wiring depth out"
            ),
            PipelineError::EffectInputMismatch {
                node,
                effect,
                reason,
            } => write!(f, "screen pass `{node}` runs `{effect}`, but {reason}"),
            PipelineError::IntoFromPass { node } => write!(
                f,
                "screen pass `{node}` writes into another pass's output; wire a \
                 `resource.color` into `into` instead — a resource is the thing a \
                 chain passes along"
            ),
            PipelineError::ImageFromPass { node, fed_by } => write!(
                f,
                "screen pass `{node}` reads `{fed_by}`'s colour output, which is the \
                 frame's target — what is on screen cannot also be sampled; wire a \
                 `resource.color` between the two passes and have `{fed_by}` write \
                 into that"
            ),
            PipelineError::NoPresent => {
                f.write_str("the pipeline has no `present` node, so nothing reaches the frame")
            }
            PipelineError::TwoPresents { nodes } => write!(
                f,
                "the pipeline has {} `present` nodes ({}) — one picture per frame",
                nodes.len(),
                nodes.join(", ")
            ),
            PipelineError::NotPresentable {
                node,
                fed_by,
                wrote,
            } => {
                write!(f, "`present` on `{node}` is fed by `{fed_by}`")?;
                if let Some(wrote) = wrote {
                    write!(f, ", which wrote `{wrote}` — an intermediate")?;
                }
                write!(
                    f,
                    "; present is fed by the pass that writes the frame's target \
                     (its `into` left unconnected)"
                )
            }
            PipelineError::TwoTargetWriters { first, second } => write!(
                f,
                "`{first}` and `{second}` both write the frame's target — leave `into` \
                 unconnected on the last pass in the chain only"
            ),
            PipelineError::TwoShadowSources { nodes } => write!(
                f,
                "the pipeline declares shadow maps twice ({}) — the frame group binds \
                 exactly one array",
                nodes.join(", ")
            ),
            PipelineError::ChannelCollision { node, error } => {
                write!(f, "the G-buffer of `{node}` cannot be built: {error}")
            }
            PipelineError::WriteIntoPassOutput { node } => write!(
                f,
                "pass `{node}` writes into another pass's output; wire a \
                 `resource.color` or `resource.buffer` into it instead — a \
                 resource is the thing a chain passes along"
            ),
            PipelineError::VelocityTargetFormat { node, format } => write!(
                f,
                "pass `{node}` is a velocity pass, and writes {format} — motion \
                 is a pair of signed half floats, so its target is a \
                 `resource.color` at precision `pair`"
            ),
        }
    }
}

impl std::error::Error for PipelineError {}

/// The node registry a pipeline document validates against: the shipped
/// vocabulary plus one `pass.compute.<effect>` row per registered compute
/// effect (plan3 N3).
///
/// A document that names a compute node has to have been *validated*
/// against that effect's derived sockets — the graph model's typing is
/// what keeps a `resource.buffer` out of an image socket — so the rows an
/// [`EffectRegistry`] implies belong in the registry the document is
/// checked with, and this is how they get there.
pub fn document_registry(effects: &EffectRegistry) -> NodeRegistry {
    let mut registry = wxsl_core::pipeline::registry();
    registry.register_all(effects.node_defs());
    registry
}

/// Compile a pipeline document into a pass list.
///
/// Pure: no device, no allocation beyond the graph itself. Everything the
/// [`RenderGraph::schedule`] can catch is still caught there — this
/// function's own errors are the ones the scheduler *cannot* name, because
/// they are about nodes the scheduler never saw.
pub fn compile(
    document: &Graph,
    registry: &NodeRegistry,
    effects: &EffectRegistry,
    config: &PipelineConfig,
) -> Result<RenderGraph, PipelineError> {
    if let Err(errors) = document.validate(registry) {
        return Err(PipelineError::InvalidDocument(errors));
    }
    Compiler::new(document, registry, effects, config).compile()
}

/// The label a document node carries in errors and engine labels: the
/// display name its author gave it, or its definition's.
fn label(document: &Graph, registry: &NodeRegistry, node: NodeId) -> String {
    let Some(node) = document.node(node) else {
        return node.to_string();
    };
    node.label
        .clone()
        .unwrap_or_else(|| def_label(registry, &node.def))
}

fn def_label(registry: &NodeRegistry, id: &str) -> String {
    registry
        .get(id)
        .map(|def| def.label.clone())
        .unwrap_or_else(|| id.to_string())
}

/// One document, mid-compilation.
struct Compiler<'a> {
    document: &'a Graph,
    registry: &'a NodeRegistry,
    /// The effects a `pass.screen` node may name, and whose declared
    /// inputs are what its wiring is validated against.
    effects: &'a EffectRegistry,
    config: &'a PipelineConfig,
    graph: RenderGraph,
    /// `resource.gbuffer` node → (targets in layout order, depth).
    gbuffers: HashMap<NodeId, (Vec<ResourceId>, ResourceId)>,
    /// `resource.color` node → engine resource.
    colors: HashMap<NodeId, ResourceId>,
    /// `resource.depth` node → engine resource.
    depths: HashMap<NodeId, ResourceId>,
    /// `resource.buffer` node → engine resource.
    buffers: HashMap<NodeId, ResourceId>,
    /// What each pass node's `color` output stands for.
    pass_colors: HashMap<NodeId, ResourceId>,
    /// What each pass node's `depth` output stands for.
    pass_depths: HashMap<NodeId, ResourceId>,
    /// The `source.lights` node and its resource, if the document has one.
    shadow_source: Option<(NodeId, ResourceId)>,
    /// The G-buffer layout a `resource.gbuffer` node asked for, applied to
    /// the compiled graph once its shape is known.
    layout: Option<Vec<abi::GBufferTarget>>,
    /// Labels of every pass writing the frame's target, in document order.
    target_writers: Vec<String>,
}

impl<'a> Compiler<'a> {
    fn new(
        document: &'a Graph,
        registry: &'a NodeRegistry,
        effects: &'a EffectRegistry,
        config: &'a PipelineConfig,
    ) -> Self {
        Compiler {
            document,
            registry,
            effects,
            config,
            graph: RenderGraph::new(config.target.format),
            gbuffers: HashMap::new(),
            colors: HashMap::new(),
            depths: HashMap::new(),
            buffers: HashMap::new(),
            pass_colors: HashMap::new(),
            pass_depths: HashMap::new(),
            shadow_source: None,
            layout: None,
            target_writers: Vec::new(),
        }
    }

    fn label(&self, node: NodeId) -> String {
        label(self.document, self.registry, node)
    }

    /// The node's definition id.
    fn kind(&self, node: NodeId) -> Option<&str> {
        self.document.node(node).map(|node| node.def.as_str())
    }

    fn setting(&self, node: NodeId, name: &str) -> String {
        self.document
            .setting(self.registry, node, name)
            .map(str::to_string)
            .unwrap_or_default()
    }

    fn compile(mut self) -> Result<RenderGraph, PipelineError> {
        // Resources first: passes refer to them by id, and declaration
        // order (document order, by node id) is the scheduler's tie-break.
        let nodes: Vec<NodeId> = self.document.nodes().map(|(id, _)| id).collect();
        for node in &nodes {
            match self.kind(*node) {
                Some(doc::SOURCE_LIGHTS) => self.declare_shadow_maps(*node)?,
                Some(doc::RESOURCE_GBUFFER) => self.declare_gbuffer(*node)?,
                Some(doc::RESOURCE_COLOR) => self.declare_color(*node)?,
                Some(doc::RESOURCE_DEPTH) => self.declare_depth(*node)?,
                Some(doc::RESOURCE_BUFFER) => self.declare_buffer(*node)?,
                _ => {}
            }
        }

        // Then passes, in document order — which is the order the author
        // built them in, and only a tie-break for the scheduler anyway.
        let mut presents: Vec<NodeId> = Vec::new();
        for node in &nodes {
            // Copied out: the compute arm names the pass to `self`, and a
            // `&str` out of `kind` would still be holding the borrow.
            let kind = self.kind(*node).map(str::to_string);
            match kind.as_deref() {
                Some(doc::PASS_GEOMETRY) => self.geometry_pass(*node)?,
                Some(doc::PASS_SHADOW) => self.shadow_passes(*node)?,
                Some(doc::PASS_SCREEN) => self.screen_pass(*node)?,
                Some(doc::PRESENT) => presents.push(*node),
                Some(other) if other.starts_with(doc::PASS_COMPUTE_PREFIX) => {
                    self.compute_pass(*node, other)?;
                }
                // The screen pass's own derived rows: same dispatch shape,
                // and the effect named by the row rather than by a setting.
                // (`pass.screen` itself does not match the prefix — the dot
                // sees to that.)
                Some(other) if other.starts_with(doc::PASS_SCREEN_PREFIX) => {
                    self.derived_screen_pass(*node, other)?;
                }
                _ => {}
            }
        }
        self.finish(presents)?;
        Ok(self.graph)
    }

    // -- resources --------------------------------------------------------

    fn declare_shadow_maps(&mut self, node: NodeId) -> Result<(), PipelineError> {
        if let Some((existing, _)) = self.shadow_source {
            return Err(PipelineError::TwoShadowSources {
                nodes: vec![
                    label(self.document, self.registry, existing),
                    self.label(node),
                ],
            });
        }
        let maps = self.graph.declare_shadow_maps(
            ResourceDesc::color("shadow maps", DEPTH_FORMAT)
                .with_extent(Extent::Fixed {
                    width: abi::SHADOW_MAP_RESOLUTION,
                    height: abi::SHADOW_MAP_RESOLUTION,
                })
                .with_dimension(Dimension::D2Array, abi::MAX_LIGHTS as u32)
                // Both because nothing in the pass list reads this resource:
                // it is sampled through the frame group, so the graph infers
                // neither the usage nor the lifetime and is told both.
                .with_usage(wgpu::TextureUsages::TEXTURE_BINDING)
                .persistent(0),
        );
        self.shadow_source = Some((node, maps));
        Ok(())
    }

    fn declare_gbuffer(&mut self, node: NodeId) -> Result<(), PipelineError> {
        // The layout is the config's plan — the enabled set's requests
        // plus any feature channels — not the document's: widening either
        // rewires nothing, because the lighting pass reads the G-buffer by
        // the shape it was generated for.
        let layout = self
            .config
            .plan()
            .map_err(|error| PipelineError::ChannelCollision {
                node: self.label(node),
                error: error.to_string(),
            })?;
        let targets: Vec<ResourceId> = layout
            .layout()
            .iter()
            .map(|entry| {
                self.graph.resource(ResourceDesc::color(
                    format!("gbuffer {}", entry.field),
                    gbuffer_format(entry.precision),
                ))
            })
            .collect();
        let depth = self
            .graph
            .resource(ResourceDesc::color("gbuffer depth", DEPTH_FORMAT));
        self.layout = Some(layout.layout().to_vec());
        self.gbuffers.insert(node, (targets, depth));
        Ok(())
    }

    fn declare_color(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let name = self.label(node);
        let precision_text = self.setting(node, doc::SETTING_PRECISION);
        let precision = abi::GBufferPrecision::parse(&precision_text).ok_or_else(|| {
            PipelineError::UnknownPrecision {
                node: name.clone(),
                precision: precision_text,
            }
        })?;

        // An imported target is the host's: sized by whoever created it,
        // so the size settings have nothing to say and are refused rather
        // than ignored — a document that says both would only ever be
        // obeyed on one of them.
        if self
            .setting(node, doc::SETTING_IMPORTED)
            .trim()
            .eq_ignore_ascii_case("true")
        {
            let size_text = self.setting(node, doc::SETTING_SIZE);
            let scale = self.setting(node, doc::SETTING_SCALE);
            let history = self.setting(node, doc::SETTING_HISTORY);
            let conflicted = [
                (
                    !size_text.trim().eq_ignore_ascii_case("viewport"),
                    ("size", size_text),
                ),
                (scale.trim() != "1", ("scale", scale)),
                (history.trim() != "0", ("history", history)),
            ];
            for (conflict, (setting, value)) in conflicted {
                if conflict {
                    return Err(PipelineError::ImportedTargetSized {
                        node: name.clone(),
                        setting: setting.to_string(),
                        value: value.trim().to_string(),
                    });
                }
            }
            let id = self
                .graph
                .resource(ResourceDesc::imported(name, gbuffer_format(precision)));
            self.colors.insert(node, id);
            return Ok(());
        }

        // `viewport` sizes with the frame (the `scale` setting); anything
        // spelled `WxH` is fixed pixels — the LUTs and atlases whose size
        // is a fact of their contents, not of the window.
        let size_text = self.setting(node, doc::SETTING_SIZE);
        let extent = if size_text.trim().eq_ignore_ascii_case("viewport") {
            Extent::Viewport {
                scale: self.number(node, doc::SETTING_SCALE)?,
            }
        } else {
            let (width, height) = size_text.trim().split_once(['x', 'X']).ok_or_else(|| {
                PipelineError::UnknownSize {
                    node: name.clone(),
                    size: size_text.clone(),
                }
            })?;
            let (width, height) = (width.trim(), height.trim());
            let (width, height): (u32, u32) = match (width.parse(), height.parse()) {
                (Ok(width), Ok(height)) => (width, height),
                _ => {
                    return Err(PipelineError::UnknownSize {
                        node: name.clone(),
                        size: size_text.clone(),
                    })
                }
            };
            Extent::Fixed { width, height }
        };
        let history = self.number(node, doc::SETTING_HISTORY)? as u32;
        let mut desc = ResourceDesc::color(name, gbuffer_format(precision)).with_extent(extent);
        desc.persistence = if history == 0 {
            Persistence::Transient
        } else {
            Persistence::Persistent { history }
        };
        let id = self.graph.resource(desc);
        self.colors.insert(node, id);
        Ok(())
    }

    fn declare_depth(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let scale = self.number(node, doc::SETTING_SCALE)?;
        let id = self.graph.resource(
            ResourceDesc::color(self.label(node), DEPTH_FORMAT)
                .with_extent(Extent::Viewport { scale }),
        );
        self.depths.insert(node, id);
        Ok(())
    }

    /// A storage buffer: `bytes` big, persistent when `history` says so.
    /// The scheduler treats it as a resource that never aliases and orders
    /// the passes around it, exactly as it does for attachments — the
    /// document adds nothing the engine did not already reason about.
    fn declare_buffer(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let name = self.label(node);
        let bytes_text = self.setting(node, doc::SETTING_BYTES);
        let bytes: u64 = bytes_text
            .trim()
            .parse()
            .map_err(|_| PipelineError::BadBufferSize {
                node: name.clone(),
                value: bytes_text.clone(),
            })?;
        if bytes == 0 {
            return Err(PipelineError::BadBufferSize {
                node: name.clone(),
                value: bytes_text,
            });
        }
        let history = self.number(node, doc::SETTING_HISTORY)? as u32;
        let mut desc = ResourceDesc::buffer(name, bytes);
        desc.persistence = if history == 0 {
            Persistence::Transient
        } else {
            Persistence::Persistent { history }
        };
        let id = self.graph.resource(desc);
        self.buffers.insert(node, id);
        Ok(())
    }

    /// A numeric setting, with the setting's name in the error.
    fn number(&self, node: NodeId, setting: &'static str) -> Result<f32, PipelineError> {
        let text = self.setting(node, setting);
        text.trim().parse().map_err(|_| PipelineError::BadNumber {
            node: self.label(node),
            setting,
            value: text,
        })
    }

    /// The pass's execution policy, from the `policy` setting. The
    /// default setting text parses to `per frame`, so documents that never
    /// mention it compile exactly as they always did.
    fn policy(&self, node: NodeId) -> Result<Policy, PipelineError> {
        let text = self.setting(node, doc::SETTING_POLICY);
        Policy::parse(&text).ok_or_else(|| PipelineError::UnknownPolicy {
            node: self.label(node),
            policy: text,
        })
    }

    // -- passes -----------------------------------------------------------

    /// The tag expression a pass draws: the `tags` setting of the
    /// `source.scene` node feeding its `draws` input. The filter belongs
    /// to the draw queue, not to the pass — one source filters once, and
    /// two passes reading it draw the same things.
    fn tags(&self, pass: NodeId) -> Result<TagExpr, PipelineError> {
        let source = self
            .fed(pass, "draws")
            .expect("a validated document's pass has a draw source");
        let text = self.setting(source, doc::SETTING_TAGS);
        TagExpr::parse(&text).map_err(|reason| PipelineError::BadTags {
            node: self.label(source),
            value: text,
            reason: reason.to_string(),
        })
    }

    /// The node an input is fed from, or `None` if unfed.
    fn fed(&self, node: NodeId, socket: &str) -> Option<NodeId> {
        self.document
            .edge_into(&SocketRef::new(node, socket))
            .map(|edge| edge.from.node)
    }

    /// The node an input is fed from *and* the output socket it is fed
    /// through — the socket name is what identifies one of a compute
    /// pass's several declared outputs.
    fn fed_via(&self, node: NodeId, socket: &str) -> Option<(NodeId, String)> {
        self.document
            .edge_into(&SocketRef::new(node, socket))
            .map(|edge| (edge.from.node, edge.from.socket.clone()))
    }

    /// Where a pass's `into` writes: the named resource, or the frame's
    /// target when `into` is unconnected.
    ///
    /// Shared by screen and colour-writing material passes, so that the
    /// rule "a chain passes along a resource, and another pass's output is
    /// nobody's to write" is one rule rather than two.
    fn write_target(&mut self, node: NodeId) -> Result<ResourceId, PipelineError> {
        match self.fed(node, "into") {
            Some(source) => {
                if self.kind(source) != Some(doc::RESOURCE_COLOR) {
                    return Err(PipelineError::IntoFromPass {
                        node: self.label(node),
                    });
                }
                Ok(self.colors[&source])
            }
            None => {
                self.target_writers.push(self.label(node));
                Ok(RenderGraph::TARGET)
            }
        }
    }

    /// The colour attachment for a target a pass writes: the config's
    /// clear colour, wherever in the chain the colour is being written.
    ///
    /// The clear colour belongs to the *frame*, not to one resource. Since
    /// the display transform became a pass of its own
    /// ([ADR 0039](../../../docs/adr/0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md))
    /// what the user sees as the background is whatever the head of the
    /// chain left where nothing was drawn — the lighting pass `discard`s
    /// there, and a forward pass never covers the whole screen — so an
    /// intermediate cleared to transparent would make every chained
    /// pipeline's background black regardless of the configuration. It is
    /// read as linear radiance now, like everything else in a chain.
    fn color_attachment(&self, target: ResourceId) -> Attachment {
        Attachment::clear(target, self.config.target.clear_color)
    }

    /// Where a pass's depth comes from, what it does to it, and whether it
    /// *loads* — depth straight from a `resource.depth` node is cleared;
    /// depth from another pass's output is tested against and left alone,
    /// which is also a state change: `LessEqual`, and no depth write. The
    /// prepass idiom is one wire and it costs the pipeline nothing.
    fn depth_input(&self, node: NodeId) -> Result<Option<(DepthAttachment, bool)>, PipelineError> {
        let Some(source) = self.fed(node, "depth") else {
            return Ok(None);
        };
        match self.kind(source) {
            Some(doc::RESOURCE_DEPTH) => Ok(Some((
                DepthAttachment::clear(self.depths[&source], 1.0),
                false,
            ))),
            Some(doc::PASS_GEOMETRY) => {
                let resource = *self.pass_depths.get(&source).ok_or_else(|| {
                    PipelineError::DepthOutputWithoutDepth {
                        node: label(self.document, self.registry, source),
                    }
                })?;
                Ok(Some((
                    DepthAttachment {
                        resource,
                        clear: None,
                        store: true,
                        layer: 0,
                    },
                    true,
                )))
            }
            other => unreachable!(
                "typing only lets a `resource.depth` or a material pass produce a \
                 depth target, not {other:?}"
            ),
        }
    }

    fn geometry_pass(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let name = self.label(node);
        let stage_text = self.setting(node, doc::SETTING_STAGE);
        let stage =
            MaterialStage::parse(&stage_text).ok_or_else(|| PipelineError::UnknownStage {
                node: name.clone(),
                stage: stage_text,
            })?;
        if stage == MaterialStage::SHADOW {
            return Err(PipelineError::UnknownStage {
                node: name,
                stage: MaterialStage::SHADOW.name().to_string(),
            });
        }

        // Depth first: a `gbuffer`-stage pass takes the G-buffer's, and may
        // not also wire a separate one.
        let gbuffer = self.fed(node, "gbuffer");
        if stage == MaterialStage::GBUFFER && gbuffer.is_none() {
            return Err(PipelineError::GbufferWithoutGbuffer { node: name });
        }
        if gbuffer.is_some() && self.fed(node, "depth").is_some() {
            return Err(PipelineError::DepthConflict { node: name });
        }
        // Copied out rather than borrowed: the arms below mutate other
        // fields of `self`.
        let gbuffer_depth = gbuffer.map(|source| self.gbuffers[&source].clone());
        let (depth, loads_depth) = match gbuffer_depth {
            Some((_, gbuffer_depth)) => {
                self.pass_depths.insert(node, gbuffer_depth);
                (Some(DepthAttachment::clear(gbuffer_depth, 1.0)), false)
            }
            None => match self.depth_input(node)? {
                None => return Err(PipelineError::PassWithoutDepth { node: name }),
                Some((depth, loads)) => {
                    self.pass_depths.insert(node, depth.resource);
                    (Some(depth), loads)
                }
            },
        };

        // Colour: a `forward_lit` pass writes where its `into` says — the
        // frame's target, or a `resource.color` it starts a chain into; a
        // `gbuffer`-stage pass writes every target the enabled set
        // requested; every other stage writes no colour.
        let color_wired = self
            .document
            .edge_from(&SocketRef::new(node, "color"))
            .is_some();
        let into_wired = self.fed(node, "into").is_some();
        let mut wrote = RenderGraph::TARGET;
        let colors: Vec<Attachment> = match stage.output() {
            abi::StageOutput::Color | abi::StageOutput::Velocity => {
                // A pass whose colour reaches nothing: neither handed on
                // through `color` nor written into a named target.
                if !color_wired && !into_wired {
                    return Err(PipelineError::ColorWithoutConsumer { node: name });
                }
                wrote = self.write_target(node)?;
                // A velocity target holds a pair of *signed* half floats —
                // motion is a difference, and negative — so the one
                // precision that spells that is the only one the fragment
                // output type agrees with. Everything else would fail at
                // pipeline creation with a format complaint; this names
                // the document node instead.
                if stage.output() == abi::StageOutput::Velocity {
                    let format = self
                        .graph
                        .resource_desc(wrote)
                        .and_then(|desc| desc.texture())
                        .map(|shape| match shape {
                            crate::pass::ResourceShape::Texture { format, .. } => *format,
                            crate::pass::ResourceShape::Buffer { .. } => {
                                unreachable!("typing only lets a colour target feed an `into`")
                            }
                        });
                    if format != Some(gbuffer_format(abi::GBufferPrecision::HighDynamicRangePair)) {
                        return Err(PipelineError::VelocityTargetFormat {
                            node: name,
                            format: format
                                .map(|format| format!("{format:?}"))
                                .unwrap_or_else(|| "the frame's own target".to_string()),
                        });
                    }
                    // And an unwritten texel of a velocity target means
                    // *no motion*, not the frame's radiance clear: a
                    // background that claimed the clear colour as motion
                    // would reproject every empty pixel a step sideways
                    // and bleed the history across silhouettes. The stage
                    // owns what its output means where nothing drew.
                    vec![Attachment::clear(wrote, wgpu::Color::TRANSPARENT)]
                } else {
                    vec![self.color_attachment(wrote)]
                }
            }
            abi::StageOutput::GBuffer => {
                if color_wired || into_wired {
                    return Err(PipelineError::ColorFromColorlessStage {
                        node: name,
                        stage: stage.name().to_string(),
                    });
                }
                let (targets, _) = &self.gbuffers[&gbuffer.expect(
                    "checked above: the stage \
                 writes a G-buffer, so it must be wired to one",
                )];
                targets
                    .iter()
                    .map(|target| Attachment::clear(*target, wgpu::Color::TRANSPARENT))
                    .collect()
            }
            abi::StageOutput::Nothing => {
                if color_wired || into_wired {
                    return Err(PipelineError::ColorFromColorlessStage {
                        node: name,
                        stage: stage.name().to_string(),
                    });
                }
                Vec::new()
            }
        };

        // A pass that loads depth never writes it — the prepass already
        // did — and tests `LessEqual` rather than `Equal`, because WGSL
        // makes no promise that two pipelines running the same vertex code
        // produce bit-identical clip positions, and `Equal` turns a one-ulp
        // difference into a hole in the surface.
        let state = if loads_depth {
            PassState::OPAQUE.with_depth_test(wgpu::CompareFunction::LessEqual, false)
        } else {
            PassState::OPAQUE
        };

        let tags = self.tags(node)?;
        let policy = self.policy(node)?;
        // The G-buffer a policy'd pass writes is stable storage for the
        // same reason a chain's colour target is.
        if policy != Policy::PerFrame {
            if let Some((targets, depth)) = gbuffer.as_ref().and_then(|s| self.gbuffers.get(s)) {
                for target in targets {
                    self.graph.make_stable_storage(*target);
                }
                self.graph.make_stable_storage(*depth);
            } else if let Some((depth, _)) = self.depth_input(node)? {
                self.graph.make_stable_storage(depth.resource);
            }
        }
        let mut pass = PassDesc::geometry(name, DrawSource::Scene(tags), stage)
            .with_colors(colors)
            .with_state(state)
            .with_policy(policy);
        if let Some(depth) = depth {
            pass = pass.with_depth(depth);
        }
        self.graph.pass(pass);
        self.pass_colors.insert(node, wrote);
        Ok(())
    }

    fn shadow_passes(&mut self, node: NodeId) -> Result<(), PipelineError> {
        // Typing already guarantees `into` is fed and that only
        // `source.lights` feeds it, and `declare_shadow_maps` has already
        // rejected a second source — so the maps are the recorded ones.
        let (_, maps) = self
            .shadow_source
            .expect("a validated document's shadow pass names declared shadow maps");
        let tags = self.tags(node)?;
        let name = self.label(node);
        // One pass per light slot, always — not one per light that happens
        // to be casting this frame. Which lights cast is the environment's
        // business and changes whenever the application says so.
        for light in 0..abi::MAX_LIGHTS as u32 {
            self.graph.pass(
                PassDesc::geometry(
                    format!("{name} {light}"),
                    DrawSource::Scene(tags.clone()),
                    MaterialStage::SHADOW,
                )
                .with_view(PassView::Light { index: light })
                .with_depth(DepthAttachment::clear(maps, 1.0).with_layer(light))
                // Two-sided: the usual cases are an alpha-tested leaf and a
                // displaced surface, neither of them closed.
                .with_state(PassState::OPAQUE.with_cull_mode(None)),
            );
        }
        Ok(())
    }

    fn screen_pass(&mut self, node: NodeId) -> Result<(), PipelineError> {
        let name = self.label(node);
        let effect_text = self.setting(node, doc::SETTING_EFFECT).trim().to_string();
        let known: Vec<String> = self.effects.ids();
        let effect = self
            .effects
            .get(&effect_text)
            .ok_or_else(|| PipelineError::UnknownEffect {
                node: name.clone(),
                effect: effect_text,
                known,
            })?
            .clone();
        // An effect whose wiring the fixed socket set cannot name — two
        // image inputs and up — has a derived row of its own, whose
        // sockets are the declaration. Naming it here would bind two
        // declared inputs to the one `image` socket and pretend that was
        // wiring; the mistake is named instead.
        if !effect.fits_pass_screen() {
            return Err(PipelineError::EffectInputMismatch {
                node: name.clone(),
                effect: effect.id.to_string(),
                reason: format!(
                    "it declares more images than the fixed socket set can name — \
                     place `{}{}` instead, whose sockets are the declaration",
                    doc::PASS_SCREEN_PREFIX,
                    effect.id
                ),
            });
        }

        // The reads are the effect's declared inputs, in declaration order
        // — the order the shader declares its pass-group bindings in —
        // each through the fixed socket set's one name for its kind.
        let sockets: Vec<&'static str> = effect
            .inputs
            .iter()
            .map(|input| match input.kind {
                EffectInputKind::GBuffer => "gbuffer",
                EffectInputKind::Image => "image",
                EffectInputKind::Buffer => "buffer",
            })
            .collect();
        let reads = self.screen_reads(node, &effect, &sockets)?;
        for (socket, wired) in [("gbuffer", "a G-buffer"), ("image", "an image")] {
            if effect.declares(socket) {
                continue;
            }
            if self.fed(node, socket).is_some() {
                return Err(PipelineError::EffectInputMismatch {
                    node: name.clone(),
                    effect: effect.id.to_string(),
                    reason: format!(
                        "{wired} is wired into it, which `{}` does not take",
                        effect.id
                    ),
                });
            }
        }
        if !effect
            .inputs
            .iter()
            .any(|input| input.kind == EffectInputKind::Buffer)
            && self.fed(node, "buffer").is_some()
        {
            return Err(PipelineError::EffectInputMismatch {
                node: name.clone(),
                effect: effect.id.to_string(),
                reason: format!(
                    "a storage buffer is wired into it, which `{}` does not take",
                    effect.id
                ),
            });
        }
        self.screen_pass_tail(node, &effect, reads)
    }

    /// The screen pass's derived row: same tail as [`Self::screen_pass`],
    /// but the effect named by the row itself and every declared input
    /// wired through a socket of its own — TAA's `color`, `velocity` and
    /// `history` are three sockets, not one `image` said three times.
    fn derived_screen_pass(&mut self, node: NodeId, def_id: &str) -> Result<(), PipelineError> {
        let name = self.label(node);
        let effect_id = &def_id[doc::PASS_SCREEN_PREFIX.len()..];
        let known: Vec<String> = self.effects.ids();
        let effect = self
            .effects
            .get(effect_id)
            .ok_or_else(|| PipelineError::UnknownEffect {
                node: name.clone(),
                effect: effect_id.to_string(),
                known,
            })?
            .clone();
        let sockets: Vec<&'static str> = effect.inputs.iter().map(|input| input.name).collect();
        let reads = self.screen_reads(node, &effect, &sockets)?;
        self.screen_pass_tail(node, &effect, reads)
    }

    /// The pass-group reads one screen effect's declared inputs become, in
    /// declaration order — the order the shader declares its `@group(3)`
    /// bindings in. `sockets[i]` is the document socket inputs[i] is wired
    /// through: the fixed set's name for its kind on the generic
    /// `pass.screen`, the input's own name on a derived row. An image
    /// input carrying a `history` reads a resource's ring at that depth,
    /// which orders nothing — the property a temporal resolve needs to
    /// stay schedulable against its own previous output.
    fn screen_reads(
        &self,
        node: NodeId,
        effect: &Effect,
        sockets: &[&'static str],
    ) -> Result<Vec<Read>, PipelineError> {
        let name = self.label(node);
        let mut reads: Vec<Read> = Vec::new();
        let mut socket_taken: Vec<&'static str> = Vec::new();
        for (input, socket) in effect.inputs.iter().zip(sockets) {
            // Two declared inputs through one socket is the fixed set's
            // own limit speaking: on the generic pass a second buffer has
            // no socket to take, and on a derived row the sockets are the
            // inputs' own names, so this stays quiet.
            if socket_taken.contains(socket) {
                return Err(PipelineError::EffectInputMismatch {
                    node: name.clone(),
                    effect: effect.id.to_string(),
                    reason: format!(
                        "{} — but a screen pass has one `{socket}` socket, \
                         and an earlier declared input took it",
                        input.description
                    ),
                });
            }
            socket_taken.push(socket);
            match input.kind {
                EffectInputKind::GBuffer => match self.fed(node, socket) {
                    Some(source) => {
                        let (targets, depth) = &self.gbuffers[&source];
                        reads.extend(
                            targets
                                .iter()
                                .copied()
                                .chain(core::iter::once(*depth))
                                .map(Read::current),
                        );
                    }
                    None => {
                        return Err(PipelineError::EffectInputMismatch {
                            node: name.clone(),
                            effect: effect.id.to_string(),
                            reason: format!(
                                "{} — but no G-buffer is wired into it",
                                input.description
                            ),
                        });
                    }
                },
                EffectInputKind::Image => match self.fed(node, socket) {
                    Some(source) => reads.push(Read {
                        resource: self.image_source(source, node)?,
                        history: input.history,
                    }),
                    None => {
                        return Err(PipelineError::EffectInputMismatch {
                            node: name.clone(),
                            effect: effect.id.to_string(),
                            reason: format!(
                                "{} — but nothing is wired into `{socket}`",
                                input.description
                            ),
                        });
                    }
                },
                EffectInputKind::Buffer => {
                    let Some(source) = self.fed(node, socket) else {
                        return Err(PipelineError::EffectInputMismatch {
                            node: name.clone(),
                            effect: effect.id.to_string(),
                            reason: format!(
                                "{} — but nothing is wired into `{socket}`",
                                input.description
                            ),
                        });
                    };
                    reads.push(Read::current(self.storage_source(source, effect)?));
                }
            }
        }
        Ok(reads)
    }

    /// Everything a screen pass shares once its reads are known: the write
    /// target, the policy, the promotion a skipped frame needs, and the
    /// pass itself.
    fn screen_pass_tail(
        &mut self,
        node: NodeId,
        effect: &Effect,
        reads: Vec<Read>,
    ) -> Result<(), PipelineError> {
        let name = self.label(node);
        let target = self.write_target(node)?;
        let policy = self.policy(node)?;
        // A pass that skips frames writes stable storage, or the
        // scheduler names the mistake: a chain's colour target is
        // promoted here, so `once`-into-`resource.color` just works.
        if policy != Policy::PerFrame && target != RenderGraph::TARGET {
            self.graph.make_stable_storage(target);
        }
        self.graph.pass(
            PassDesc::screen(name, effect.id)
                .with_color(self.color_attachment(target))
                .with_reads(reads)
                .with_policy(policy),
        );
        self.pass_colors.insert(node, target);
        Ok(())
    }

    /// A compute pass: dispatch an effect that writes storage.
    ///
    /// The node is a `pass.compute.<effect>` definition derived from the
    /// effect's declaration, so its sockets *are* the contract — what the
    /// graph model typed as fed or unfed is exactly what the effect
    /// declared. Reads and writes resolve socket by socket, in declaration
    /// order, which is the order the shader declares its `@group(3)`
    /// bindings in and the order the pass group binds.
    ///
    /// A declared *output* is an input socket on the node, as `into` is on
    /// a screen pass: the wire names the storage being written, and the
    /// storage — the resource — is what any reader wires from. Compute to
    /// screen through a buffer is one `resource.buffer` feeding two passes,
    /// which is the same shape as a colour chain through `resource.color`.
    fn compute_pass(&mut self, node: NodeId, def_id: &str) -> Result<(), PipelineError> {
        let name = self.label(node);
        let effect_id = &def_id[doc::PASS_COMPUTE_PREFIX.len()..];
        let known: Vec<String> = self.effects.ids();
        let effect = self
            .effects
            .get(effect_id)
            .ok_or_else(|| PipelineError::UnknownEffect {
                node: name.clone(),
                effect: effect_id.to_string(),
                known,
            })?;

        // Reads first: one pass-group binding per declared input, in the
        // declared order. The generated definition made every input
        // mandatory, so a validated document has each socket fed.
        let mut reads: Vec<Read> = Vec::new();
        for input in effect.inputs {
            let (source, _) = self
                .fed_via(node, input.name)
                .expect("a validated document's compute pass has every declared input fed");
            reads.push(match input.kind {
                EffectInputKind::GBuffer => {
                    let (targets, depth) = &self.gbuffers[&source];
                    reads.extend(
                        targets
                            .iter()
                            .copied()
                            .chain(core::iter::once(*depth))
                            .map(Read::current),
                    );
                    continue;
                }
                EffectInputKind::Image => Read::current(self.image_source(source, node)?),
                EffectInputKind::Buffer => Read::current(self.storage_source(source, &effect)?),
            });
        }

        // Then writes: one storage resource per declared output, a
        // `resource.color` for a storage texture or a `resource.buffer`
        // for a buffer. Another pass's output is nobody's to write — the
        // screen rule, at the compute pass's other end.
        let mut writes: Vec<ResourceId> = Vec::new();
        for output in effect.outputs {
            let Some((source, _)) = self.fed_via(node, output.name) else {
                return Err(PipelineError::EffectInputMismatch {
                    node: name.clone(),
                    effect: effect.id.to_string(),
                    reason: format!(
                        "{} — but nothing is wired into its `{}` output",
                        output.description, output.name
                    ),
                });
            };
            let resource = match self.kind(source) {
                Some(doc::RESOURCE_COLOR) => self.colors[&source],
                Some(doc::RESOURCE_BUFFER) => self.buffers[&source],
                Some(_) => return Err(PipelineError::WriteIntoPassOutput { node: name }),
                None => unreachable!("the edge's producer is in the document"),
            };
            writes.push(resource);
        }

        let policy = self.policy(node)?;
        // A pass that skips frames writes stable storage, or the scheduler
        // names the mistake — a `once` bake into a `resource.color` or
        // `resource.buffer` is promoted here, just as a chain's colour
        // target is for a screen pass.
        if policy != Policy::PerFrame {
            for resource in &writes {
                self.graph.make_stable_storage(*resource);
            }
        }
        let mut pass = PassDesc::compute(name, effect.id).with_reads(reads);
        for resource in writes {
            pass = pass.with_write(resource);
        }
        self.graph.pass(pass.with_policy(policy));
        Ok(())
    }

    /// The resource a screen effect's `image` input stands for: a
    /// `resource.color` node, or a pass's colour output — which is an
    /// intermediate when that pass wrote into one, and the *frame's
    /// target* when it did not. Sampling the target is the one
    /// un-compilable chain shape, and the error says what to wire instead.
    fn image_source(&self, source: NodeId, reader: NodeId) -> Result<ResourceId, PipelineError> {
        match self.kind(source) {
            Some(doc::RESOURCE_COLOR) => Ok(self.colors[&source]),
            Some(doc::PASS_GEOMETRY) | Some(doc::PASS_SCREEN) => {
                match self.pass_colors.get(&source) {
                    Some(&resource) if resource != RenderGraph::TARGET => Ok(resource),
                    _ => Err(PipelineError::ImageFromPass {
                        node: self.label(reader),
                        fed_by: label(self.document, self.registry, source),
                    }),
                }
            }
            other => unreachable!(
                "typing only lets a `resource.color` or a material pass produce a \
                 colour target, not {other:?}"
            ),
        }
    }

    /// The resource a buffer input stands for: a `resource.buffer`. The
    /// only producer of a storage buffer in the document vocabulary —
    /// storage is the resource's, and every reader wires from it, compute
    /// or screen.
    fn storage_source(
        &self,
        source: NodeId,
        effect: &crate::effect::Effect,
    ) -> Result<ResourceId, PipelineError> {
        match self.kind(source) {
            Some(doc::RESOURCE_BUFFER) => Ok(self.buffers[&source]),
            other => unreachable!(
                "typing only lets a `resource.buffer` feed a buffer socket, not \
                 {other:?} — and `{}` was wired from one",
                effect.id
            ),
        }
    }

    fn finish(&mut self, presents: Vec<NodeId>) -> Result<(), PipelineError> {
        if let Some(layout) = self.layout.take() {
            self.graph =
                std::mem::replace(&mut self.graph, RenderGraph::new(self.config.target.format))
                    .with_gbuffer_layout(layout);
        }

        let present = match presents.len() {
            0 => return Err(PipelineError::NoPresent),
            1 => presents[0],
            _ => {
                return Err(PipelineError::TwoPresents {
                    nodes: presents
                        .iter()
                        .map(|node| self.label(*node))
                        .collect::<Vec<_>>(),
                })
            }
        };

        // What feeds present must be a pass output standing for the frame's
        // target: present shows the target, and the pass that writes it
        // feeds present directly.
        if let Some(feeder) = self.fed(present, "surface") {
            let stands_for_target = matches!(
                self.kind(feeder),
                Some(doc::PASS_GEOMETRY) | Some(doc::PASS_SCREEN)
            ) && self.pass_colors.get(&feeder)
                == Some(&RenderGraph::TARGET);
            if !stands_for_target {
                let wrote = self.pass_colors.get(&feeder).and_then(|resource| {
                    self.graph
                        .resource_desc(*resource)
                        .map(|desc| desc.label.clone())
                });
                return Err(PipelineError::NotPresentable {
                    node: self.label(present),
                    fed_by: label(self.document, self.registry, feeder),
                    wrote,
                });
            }
        }

        if self.target_writers.len() > 1 {
            return Err(PipelineError::TwoTargetWriters {
                first: self.target_writers[0].clone(),
                second: self.target_writers[1].clone(),
            });
        }
        Ok(())
    }
}

/// The shipped stock pipelines, as the documents the preset files hold.
///
/// The preset *files* are the shipped truth (`assets/presets/`); this is
/// the same document built through the API, kept beside the hand-built
/// pass lists as the third spelling of "forward" and "deferred" — and the
/// test that all three agree is what keeps them one pipeline
/// ([ADR 0033](../../../docs/adr/0033-pipelines-are-documents.md)).
#[cfg(test)]
pub(crate) fn stock_document(stock: StockPipeline) -> Graph {
    let registry = wxsl_core::pipeline::registry();
    let mut graph = wxsl_core::pipeline::document(stock.name());
    let wire = |graph: &mut Graph, from: (NodeId, &str), to: (NodeId, &str)| {
        graph
            .wire(&registry, from, to)
            .expect("a stock document's own wiring");
    };

    // Node order is declaration order in the compiled graph, so sources
    // first, then the shadow pass, then everything else — the same order
    // the hand-built pass lists declare in.
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let lights = graph.add_node(doc::SOURCE_LIGHTS);
    let shadows = graph.add_node(doc::PASS_SHADOW);
    wire(&mut graph, (scene, "draws"), (shadows, "draws"));
    wire(&mut graph, (lights, "shadows"), (shadows, "into"));

    let head = match stock {
        StockPipeline::Forward => {
            let depth = graph.add(Node::new(doc::RESOURCE_DEPTH).with_label("forward depth"));
            let prepass = graph.add(
                Node::new(doc::PASS_GEOMETRY)
                    .with_label("depth prepass")
                    .with_setting(doc::SETTING_STAGE, "depth_only"),
            );
            let shade = graph.add(Node::new(doc::PASS_GEOMETRY).with_label("forward"));
            wire(&mut graph, (depth, "depth"), (prepass, "depth"));
            wire(&mut graph, (prepass, "depth"), (shade, "depth"));
            wire(&mut graph, (scene, "draws"), (prepass, "draws"));
            wire(&mut graph, (scene, "draws"), (shade, "draws"));
            shade
        }
        StockPipeline::Deferred => {
            let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
            let material = graph.add(
                Node::new(doc::PASS_GEOMETRY)
                    .with_label("deferred material")
                    .with_setting(doc::SETTING_STAGE, "gbuffer"),
            );
            let lighting = graph.add(Node::new(doc::PASS_SCREEN).with_label("deferred lighting"));
            wire(&mut graph, (scene, "draws"), (material, "draws"));
            wire(&mut graph, (gbuffer, "gbuffer"), (material, "gbuffer"));
            wire(&mut graph, (gbuffer, "gbuffer"), (lighting, "gbuffer"));
            lighting
        }
    };
    // Every stock pipeline ends in the display transform, so the head of
    // the chain shades into this rather than into the frame's target
    // (ADR 0039). Declared after the chain's own targets, because resource
    // order is what the hand-built reference is compared against.
    let hdr = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let tonemap = graph.add(
        Node::new(doc::PASS_SCREEN)
            .with_label("tonemap")
            .with_setting(doc::SETTING_EFFECT, "wxsl.tonemap"),
    );
    let present = graph.add_node(doc::PRESENT);
    wire(&mut graph, (hdr, "color"), (head, "into"));
    wire(&mut graph, (hdr, "color"), (tonemap, "image"));
    wire(&mut graph, (tonemap, "color"), (present, "surface"));
    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::EffectRegistry;
    use crate::pass::PassKind;
    use crate::pipeline::{deferred_graph, forward_graph, TargetConfig};
    use wxsl_core::lighting::LightingSet;
    use wxsl_core::pipeline::registry as make_registry;

    fn target() -> TargetConfig {
        TargetConfig::new(64, 64, wgpu::TextureFormat::Rgba8Unorm)
    }

    fn config() -> PipelineConfig {
        PipelineConfig::new(target())
    }

    /// The shipped effects, as every document below compiles against them.
    fn effects() -> EffectRegistry {
        EffectRegistry::shipped()
    }

    /// Compare two compiled graphs field by field. `PassDesc` cannot
    /// derive `PartialEq` (`DrawSource::Indirect` carries an `Arc` buffer
    /// a document can never name), so the comparison is hand-written and
    /// covers exactly what documents can produce — which is the whole
    /// point: if it ever fails to compare a field, a document that drifts
    /// from the hand-built list fails here rather than rendering subtly
    /// differently.
    fn same_graph(a: &RenderGraph, b: &RenderGraph) -> bool {
        fn same_pass(a: &PassDesc, b: &PassDesc) -> bool {
            fn same_kind(a: &PassKind, b: &PassKind) -> bool {
                match (a, b) {
                    (
                        PassKind::Geometry {
                            source: sa,
                            stage: stage_a,
                        },
                        PassKind::Geometry {
                            source: sb,
                            stage: stage_b,
                        },
                    ) => {
                        stage_a == stage_b
                            && match (sa, sb) {
                                (DrawSource::Scene(x), DrawSource::Scene(y)) => x == y,
                                _ => false,
                            }
                    }
                    (PassKind::Screen { effect: x }, PassKind::Screen { effect: y }) => x == y,
                    (PassKind::Compute { effect: x }, PassKind::Compute { effect: y }) => x == y,
                    _ => false,
                }
            }
            a.label == b.label
                && same_kind(&a.kind, &b.kind)
                && a.view == b.view
                && a.color == b.color
                && a.depth == b.depth
                && a.state == b.state
                && a.reads == b.reads
                && a.writes == b.writes
        }

        a.resources() == b.resources()
            && a.gbuffer_layout() == b.gbuffer_layout()
            && a.shadow_maps() == b.shadow_maps()
            && a.passes().len() == b.passes().len()
            && a.passes()
                .iter()
                .zip(b.passes())
                .all(|(x, y)| same_pass(x, y))
    }

    #[test]
    fn the_forward_document_compiles_to_the_hand_built_pass_list() {
        let document = stock_document(StockPipeline::Forward);
        let compiled =
            compile(&document, &make_registry(), &effects(), &config()).expect("compiles");
        let hand_built = forward_graph(target());
        assert!(
            same_graph(&compiled, &hand_built),
            "compiled:\n{compiled:#?}\nhand built:\n{hand_built:#?}"
        );
    }

    #[test]
    fn the_deferred_document_compiles_to_the_hand_built_pass_list() {
        let document = stock_document(StockPipeline::Deferred);
        let compiled =
            compile(&document, &make_registry(), &effects(), &config()).expect("compiles");
        let hand_built = deferred_graph(target(), &LightingSet::default());
        assert!(
            same_graph(&compiled, &hand_built),
            "compiled:\n{compiled:#?}\nhand built:\n{hand_built:#?}"
        );
    }

    #[test]
    fn the_deferred_document_follows_the_lighting_set_like_the_hand_built_one() {
        // Widening the set is a *config* change: the document names the
        // G-buffer, the config decides its shape, and both spellings must
        // follow together.
        let full = wxsl_core::lighting::default_set().expect("the default set");
        let config = PipelineConfig {
            target: target(),
            lighting: full,
            features: Vec::new(),
        };
        let document = stock_document(StockPipeline::Deferred);
        let compiled = compile(&document, &make_registry(), &effects(), &config).expect("compiles");
        let hand_built = deferred_graph(target(), &config.lighting);
        assert!(same_graph(&compiled, &hand_built));
    }

    #[test]
    fn a_feature_channel_joins_the_compiled_gbuffer() {
        // Enabling a feature is a *config* change too: the document never
        // names the channel, but the compiled G-buffer carries it — the
        // second source of requests, declared by the same compiler path
        // (plan2 P12).
        let features = wxsl_core::lighting::feature_requests(&["subsurface"]).unwrap();
        let config = PipelineConfig {
            target: target(),
            lighting: LightingSet::default(),
            features,
        };
        let document = stock_document(StockPipeline::Deferred);
        let compiled = compile(&document, &make_registry(), &effects(), &config).expect("compiles");
        let names: Vec<&str> = compiled
            .gbuffer_layout()
            .iter()
            .map(|target| target.field)
            .collect();
        assert_eq!(
            names,
            vec!["base_color", "normal", "emissive", "subsurface"],
            "the base targets, then the feature's channel"
        );
        // The scheduler's attachment-count check ran against the widened
        // layout — this compiling is the proof it agreed.
        compiled.schedule().expect("the widened graph schedules");

        // And a colliding config is named at compile, not at the device.
        let clashing = wxsl_core::lighting::ChannelRequest {
            source: wxsl_core::lighting::ChannelSource::Feature { name: "subsurface" },
            target: wxsl_core::abi::GBufferTarget {
                field: "base_color",
                doc: "",
                precision: wxsl_core::abi::GBufferPrecision::Normalized,
            },
        };
        let config = PipelineConfig {
            target: target(),
            lighting: LightingSet::default(),
            features: vec![clashing],
        };
        match compile(
            &stock_document(StockPipeline::Deferred),
            &make_registry(),
            &effects(),
            &config,
        ) {
            Err(PipelineError::ChannelCollision { error, .. }) => {
                assert!(error.contains("base_color"), "{error}");
            }
            other => panic!("expected a channel collision, got {other:?}"),
        }
    }

    #[test]
    fn compiled_documents_schedule_like_the_hand_built_ones() {
        for stock in StockPipeline::ALL {
            let document = stock_document(*stock);
            let compiled =
                compile(&document, &make_registry(), &effects(), &config()).expect("compiles");
            let hand_built = stock.graph(&config());
            let a = compiled.schedule().expect("the compiled list schedules");
            let b = hand_built
                .schedule()
                .expect("the hand-built list schedules");
            assert_eq!(a.order(), b.order(), "{stock}");
            assert_eq!(a.slots(), b.slots(), "{stock}");
        }
    }

    fn errors_of(document: &Graph, config: &PipelineConfig) -> PipelineError {
        compile(document, &make_registry(), &effects(), config)
            .expect_err("expected compile to fail")
    }

    #[test]
    fn a_minimal_forward_document_compiles() {
        // One lit pass, depth of its own: the single-pass forward shape.
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("single pass");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let depth = graph.add_node(doc::RESOURCE_DEPTH);
        let pass = graph.add_node(doc::PASS_GEOMETRY);
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((scene, "draws"), (pass, "draws")),
            ((depth, "depth"), (pass, "depth")),
            ((pass, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        let compiled = compile(&graph, &registry, &effects(), &config()).expect("compiles");
        // No shadow passes, no prepass: one pass per light is the shadow
        // node's doing, and this document has none.
        assert_eq!(compiled.passes().len(), 1);
        let schedule = compiled.schedule().expect("schedules");
        assert_eq!(schedule.slots().len(), 1, "just the depth texture");
    }

    #[test]
    fn unknown_effects_and_stages_are_reported_by_name() {
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("typos");
        let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
        let screen =
            graph.add(Node::new(doc::PASS_SCREEN).with_setting(doc::SETTING_EFFECT, "blur"));
        let present = graph.add_node(doc::PRESENT);
        graph
            .wire(&registry, (gbuffer, "gbuffer"), (screen, "gbuffer"))
            .unwrap();
        graph
            .wire(&registry, (screen, "color"), (present, "surface"))
            .unwrap();
        match errors_of(&graph, &config()) {
            PipelineError::UnknownEffect {
                node,
                effect,
                known,
            } => {
                assert_eq!(effect, "blur");
                assert!(known.iter().any(|id| id == "wxsl.deferred_lighting"));
                assert!(!node.is_empty());
            }
            other => panic!("expected an unknown-effect error, got {other:?}"),
        }

        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("typo stage");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let depth = graph.add_node(doc::RESOURCE_DEPTH);
        let pass = graph.add(Node::new(doc::PASS_GEOMETRY).with_setting(doc::SETTING_STAGE, "lit"));
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((scene, "draws"), (pass, "draws")),
            ((depth, "depth"), (pass, "depth")),
            ((pass, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::UnknownStage { stage, .. } if stage == "lit"
        ));

        // The shadow stage is special: it exists, but documents do not
        // name it — the shadow *node* expands to those passes.
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("hand shadow");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let depth = graph.add_node(doc::RESOURCE_DEPTH);
        let pass =
            graph.add(Node::new(doc::PASS_GEOMETRY).with_setting(doc::SETTING_STAGE, "shadow"));
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((scene, "draws"), (pass, "draws")),
            ((depth, "depth"), (pass, "depth")),
            // Wiring the colour output would be wrong for a shadow stage,
            // but the stage check fires before the colour one — and the
            // graph-level check (present must be fed) fires before both.
            ((pass, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        let error = errors_of(&graph, &config());
        assert!(
            error.to_string().contains("pass.shadow"),
            "the error should point at the shadow node: {error}"
        );
    }

    #[test]
    fn a_gbuffer_stage_pass_must_be_wired_to_a_gbuffer() {
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("no gbuffer");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let material =
            graph.add(Node::new(doc::PASS_GEOMETRY).with_setting(doc::SETTING_STAGE, "gbuffer"));
        // Nothing feeds `material.gbuffer`, and nothing else reaches
        // present — the compile must stop before scheduling.
        graph
            .wire(&registry, (scene, "draws"), (material, "draws"))
            .unwrap();
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::GbufferWithoutGbuffer { .. }
        ));
    }

    #[test]
    fn one_picture_per_frame() {
        // Two passes both writing the frame's target — two `forward_lit`
        // passes with `into`-less colour outputs feeding one present, or
        // two presents — are each a named error.
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("two writers");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let depth = graph.add_node(doc::RESOURCE_DEPTH);
        let first = graph.add_node(doc::PASS_GEOMETRY);
        let second = graph.add_node(doc::PASS_GEOMETRY);
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((scene, "draws"), (first, "draws")),
            ((scene, "draws"), (second, "draws")),
            ((depth, "depth"), (first, "depth")),
            ((depth, "depth"), (second, "depth")),
            ((first, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        // `second` writes the target with its colour output unconnected —
        // but a forward pass must consume its colour, so it fails earlier,
        // by design: the check that names the real mistake fires first.
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::ColorWithoutConsumer { .. }
        ));

        // Two presents, on an otherwise fine document.
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("two presents");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let depth = graph.add_node(doc::RESOURCE_DEPTH);
        let pass = graph.add_node(doc::PASS_GEOMETRY);
        let a = graph.add_node(doc::PRESENT);
        let b = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((scene, "draws"), (pass, "draws")),
            ((depth, "depth"), (pass, "depth")),
            ((pass, "color"), (a, "surface")),
            ((pass, "color"), (b, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::TwoPresents { .. }
        ));
    }

    #[test]
    fn presenting_an_intermediate_is_reported_not_silently_accepted() {
        // A screen pass writing into a resource, with present fed from
        // that resource: present shows the frame's target, and no pass
        // wrote it.
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("present an intermediate");
        let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
        let scene_color = graph.add(Node::new(doc::RESOURCE_COLOR).with_label("scene"));
        let screen = graph.add_node(doc::PASS_SCREEN);
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((gbuffer, "gbuffer"), (screen, "gbuffer")),
            ((scene_color, "color"), (screen, "into")),
            ((screen, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        match errors_of(&graph, &config()) {
            PipelineError::NotPresentable {
                node,
                fed_by,
                wrote,
            } => {
                assert_eq!(wrote.as_deref(), Some("scene"), "{fed_by}");
                assert!(!node.is_empty());
            }
            other => panic!("expected a not-presentable error, got {other:?}"),
        }
    }

    #[test]
    fn writing_into_another_pass_output_is_reported() {
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("into a pass");
        let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
        let first = graph.add_node(doc::PASS_SCREEN);
        let second = graph.add_node(doc::PASS_SCREEN);
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((gbuffer, "gbuffer"), (first, "gbuffer")),
            ((gbuffer, "gbuffer"), (second, "gbuffer")),
            ((first, "color"), (second, "into")),
            ((second, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::IntoFromPass { .. }
        ));
    }

    #[test]
    fn a_colorless_stage_may_not_claim_a_colour_output() {
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("prepass writes colour");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let depth = graph.add_node(doc::RESOURCE_DEPTH);
        let prepass =
            graph.add(Node::new(doc::PASS_GEOMETRY).with_setting(doc::SETTING_STAGE, "depth_only"));
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((scene, "draws"), (prepass, "draws")),
            ((depth, "depth"), (prepass, "depth")),
            ((prepass, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::ColorFromColorlessStage { stage, .. } if stage == "depth_only"
        ));
    }

    #[test]
    fn lit_geometry_without_depth_is_reported() {
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("no depth");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let pass = graph.add_node(doc::PASS_GEOMETRY);
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((scene, "draws"), (pass, "draws")),
            ((pass, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::PassWithoutDepth { .. }
        ));
    }

    #[test]
    fn wiring_both_a_gbuffer_and_a_depth_target_is_a_conflict() {
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("double depth");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
        let depth = graph.add_node(doc::RESOURCE_DEPTH);
        let material =
            graph.add(Node::new(doc::PASS_GEOMETRY).with_setting(doc::SETTING_STAGE, "gbuffer"));
        for (from, to) in [
            ((scene, "draws"), (material, "draws")),
            ((gbuffer, "gbuffer"), (material, "gbuffer")),
            ((depth, "depth"), (material, "depth")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::DepthConflict { .. }
        ));
    }

    #[test]
    fn a_screen_effect_declares_what_it_reads() {
        // The lighting effect shades a G-buffer: no gbuffer wired, or an
        // image wired where it does not take one, are both named errors.
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("screen with nothing");
        let screen = graph.add_node(doc::PASS_SCREEN);
        let present = graph.add_node(doc::PRESENT);
        graph
            .wire(&registry, (screen, "color"), (present, "surface"))
            .unwrap();
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::EffectInputMismatch { .. }
        ));

        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("screen with an image");
        let color = graph.add_node(doc::RESOURCE_COLOR);
        let screen = graph.add_node(doc::PASS_SCREEN);
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((color, "color"), (screen, "image")),
            ((screen, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        let error = errors_of(&graph, &config());
        assert!(
            matches!(error, PipelineError::EffectInputMismatch { .. }),
            "{error}"
        );
    }

    /// The motion chain a TAA resolve sits at the end of, as one document:
    /// shade into a colour target, a velocity pass over the same depth,
    /// the resolve reading colour, velocity and its own last frame, and
    /// the display transform after. Every node is the shipped vocabulary's;
    /// the registry is `document_registry`'s, because the resolve is a
    /// derived row (plan3 N2).
    fn taa_document() -> Graph {
        let effects = effects();
        let registry = document_registry(&effects);
        let mut graph = wxsl_core::pipeline::document("forward + taa");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let depth = graph.add_node(doc::RESOURCE_DEPTH);
        let material = graph.add(
            Node::new(doc::PASS_GEOMETRY)
                .with_label("forward_lit")
                .with_setting(doc::SETTING_STAGE, "forward_lit"),
        );
        let color = graph.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_label("scene")
                .with_setting(doc::SETTING_PRECISION, "hdr"),
        );
        let velocity = graph.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_label("velocity")
                .with_setting(doc::SETTING_PRECISION, "pair"),
        );
        let history = graph.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_label("taa")
                .with_setting(doc::SETTING_PRECISION, "hdr")
                .with_setting(doc::SETTING_HISTORY, "1"),
        );
        let motion = graph.add(
            Node::new(doc::PASS_GEOMETRY)
                .with_label("velocity")
                .with_setting(doc::SETTING_STAGE, "velocity"),
        );
        let taa =
            graph.add(Node::new(format!("{}wxsl.taa", doc::PASS_SCREEN_PREFIX)).with_label("taa"));
        let tonemap = graph
            .add(Node::new(doc::PASS_SCREEN).with_setting(doc::SETTING_EFFECT, "wxsl.tonemap"));
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((scene, "draws"), (material, "draws")),
            ((depth, "depth"), (material, "depth")),
            ((color, "color"), (material, "into")),
            ((scene, "draws"), (motion, "draws")),
            ((material, "depth"), (motion, "depth")),
            ((velocity, "color"), (motion, "into")),
            ((color, "color"), (taa, "color")),
            ((velocity, "color"), (taa, "velocity")),
            ((history, "color"), (taa, "history")),
            ((history, "color"), (taa, "into")),
            ((history, "color"), (tonemap, "image")),
            ((tonemap, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        graph
    }

    #[test]
    fn a_taa_chain_compiles_from_a_document() {
        // The whole of plan3 N2's chain, as nodes: a velocity pass, and a
        // resolve whose history input reads last frame's own output
        // through the ring it writes.
        let compiled = compile(
            &taa_document(),
            &document_registry(&effects()),
            &effects(),
            &config(),
        )
        .expect("the chain compiles");
        compiled.schedule().expect("the chain schedules");

        let motion = compiled
            .passes()
            .iter()
            .find(|pass| pass.label == "velocity")
            .expect("the velocity pass is in the list");
        let crate::pass::PassKind::Geometry { stage, .. } = &motion.kind else {
            panic!("the velocity pass is geometry");
        };
        assert_eq!(*stage, abi::MaterialStage::VELOCITY);

        let resolve = compiled
            .passes()
            .iter()
            .find(|pass| pass.label == "taa")
            .expect("the resolve is in the list");
        let history_id = compiled
            .resources()
            .iter()
            .position(|desc| desc.label == "taa")
            .map(|index| ResourceId(index as u32))
            .expect("the history resource exists");
        assert!(
            resolve
                .reads
                .iter()
                .any(|read| read.resource == history_id && read.history == 1),
            "the resolve reads its own ring one frame back: {:?}",
            resolve.reads
        );
        // And the history read is exactly the read that orders nothing —
        // the resolve still writes the resource, which without the
        // history exemption would be a cycle.
        assert!(!resolve
            .read_this_frame()
            .any(|resource| resource == history_id));
    }

    #[test]
    fn a_velocity_pass_orders_after_the_depth_it_tests_against() {
        let compiled = compile(
            &taa_document(),
            &document_registry(&effects()),
            &effects(),
            &config(),
        )
        .expect("the chain compiles");
        let schedule = compiled.schedule().expect("schedules");
        let order: Vec<&str> = schedule
            .order()
            .iter()
            .map(|index| compiled.passes()[*index].label.as_str())
            .collect();
        let material = order
            .iter()
            .position(|label| *label == "forward_lit")
            .expect("the material pass");
        let motion = order
            .iter()
            .position(|label| *label == "velocity")
            .expect("the velocity pass");
        let resolve = order
            .iter()
            .position(|label| *label == "taa")
            .expect("the resolve");
        assert!(
            material < motion,
            "the velocity pass loads the depth the material pass wrote"
        );
        assert!(
            motion < resolve,
            "the resolve reads the velocity the velocity pass wrote"
        );
    }

    #[test]
    fn a_velocity_target_that_cannot_hold_motion_is_named() {
        // Motion is a difference — negative, half float — so `standard`
        // (rgba8unorm, unsigned) has no agreement with the fragment's
        // `vec2f`, and the document is told instead of the pipeline
        // creation failing on a format.
        let effects = effects();
        let registry = document_registry(&effects);
        let mut graph = wxsl_core::pipeline::document("velocity into 8-bit");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let depth = graph.add_node(doc::RESOURCE_DEPTH);
        let material = graph
            .add(Node::new(doc::PASS_GEOMETRY).with_setting(doc::SETTING_STAGE, "forward_lit"));
        let color =
            graph.add(Node::new(doc::RESOURCE_COLOR).with_setting(doc::SETTING_PRECISION, "hdr"));
        let velocity = graph
            .add(Node::new(doc::RESOURCE_COLOR).with_setting(doc::SETTING_PRECISION, "standard"));
        let motion =
            graph.add(Node::new(doc::PASS_GEOMETRY).with_setting(doc::SETTING_STAGE, "velocity"));
        for (from, to) in [
            ((scene, "draws"), (material, "draws")),
            ((depth, "depth"), (material, "depth")),
            ((color, "color"), (material, "into")),
            ((scene, "draws"), (motion, "draws")),
            ((material, "depth"), (motion, "depth")),
            ((velocity, "color"), (motion, "into")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        let error = compile(&graph, &registry, &effects, &config())
            .expect_err("8-bit unsigned is not a motion format");
        assert!(
            matches!(error, PipelineError::VelocityTargetFormat { .. }),
            "{error}"
        );
    }

    #[test]
    fn a_multi_image_effect_on_the_generic_pass_is_reported_with_its_row() {
        // Naming `wxsl.taa` on the fixed `pass.screen` would bind three
        // declared inputs to one `image` socket and call it wiring; the
        // error names the derived row that exists for exactly this.
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("taa on the fixed set");
        let color = graph.add_node(doc::RESOURCE_COLOR);
        let taa =
            graph.add(Node::new(doc::PASS_SCREEN).with_setting(doc::SETTING_EFFECT, "wxsl.taa"));
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((color, "color"), (taa, "image")),
            ((taa, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        let error = errors_of(&graph, &config());
        assert!(
            matches!(error, PipelineError::EffectInputMismatch { .. }),
            "{error}"
        );
        assert!(
            error.to_string().contains("pass.screen.wxsl.taa"),
            "the error names the derived row: {error}"
        );
    }

    #[test]
    fn malformed_settings_are_reported_not_ignored() {
        // A tag expression that does not parse, a precision that is not
        // one, a scale that is not a number.
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("bad tags");
        let scene =
            graph.add(Node::new(doc::SOURCE_SCENE).with_setting(doc::SETTING_TAGS, "opaque &&"));
        let depth = graph.add_node(doc::RESOURCE_DEPTH);
        let pass = graph.add_node(doc::PASS_GEOMETRY);
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((scene, "draws"), (pass, "draws")),
            ((depth, "depth"), (pass, "depth")),
            ((pass, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        let error = errors_of(&graph, &config());
        assert!(
            matches!(error, PipelineError::BadTags { .. }),
            "a broken tag expression is named, not silently `*`: {error}"
        );

        let mut graph = wxsl_core::pipeline::document("bad precision");
        let color =
            graph.add(Node::new(doc::RESOURCE_COLOR).with_setting(doc::SETTING_PRECISION, "float"));
        let _ = color;
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::UnknownPrecision { precision, .. } if precision == "float"
        ));

        let mut graph = wxsl_core::pipeline::document("bad scale");
        graph.add(Node::new(doc::RESOURCE_DEPTH).with_setting(doc::SETTING_SCALE, "half"));
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::BadNumber { setting, .. } if setting == doc::SETTING_SCALE
        ));
    }

    #[test]
    fn two_shadow_sources_are_reported() {
        let mut graph = wxsl_core::pipeline::document("two light sources");
        graph.add_node(doc::SOURCE_LIGHTS);
        graph.add_node(doc::SOURCE_LIGHTS);
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::TwoShadowSources { .. }
        ));
    }

    #[test]
    fn a_depth_chain_orders_the_passes_by_itself() {
        // The forward idiom in miniature: a depth-only pass clearing the
        // depth, a lit pass chaining off the first pass's depth output —
        // which the compiler turns into a load attachment, so the
        // scheduler orders them without the document ever saying "after".
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("depth from nowhere");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let depth = graph.add_node(doc::RESOURCE_DEPTH);
        let broken =
            graph.add(Node::new(doc::PASS_GEOMETRY).with_setting(doc::SETTING_STAGE, "depth_only"));
        let shade = graph.add_node(doc::PASS_GEOMETRY);
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((scene, "draws"), (broken, "draws")),
            ((scene, "draws"), (shade, "draws")),
            ((depth, "depth"), (broken, "depth")),
            // `shade` reads the *broken* pass's depth — fine — but the
            // colour rule fails first, so break depth instead: a pass
            // whose depth comes from a pass that itself has none is only
            // expressible by wiring pass→pass with the first pass's depth
            // input unfed, which `depth_only` forbids. The reachable
            // error is the missing-depth one, and it names the right node.
            ((broken, "depth"), (shade, "depth")),
            ((shade, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        // `broken` has depth (from the resource), so `shade` chains off it
        // legally — this document actually compiles, and schedules in the
        // prepass order.
        let compiled = compile(&graph, &registry, &effects(), &config()).expect("compiles");
        let schedule = compiled.schedule().expect("schedules");
        assert_eq!(schedule.order(), &[0, 1]);
    }

    #[test]
    fn resources_nobody_writes_are_caught_by_the_scheduler_not_lost() {
        // A screen pass reading a colour target no pass wrote: the
        // compiler has no opinion (it cannot know an effect might not
        // read), the scheduler does — and by the resource's document
        // label, because that is the label the resource was declared
        // with.
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("orphan");
        let color = graph.add(Node::new(doc::RESOURCE_COLOR).with_label("scene"));
        let screen = graph.add(Node::new(doc::PASS_SCREEN).with_label("tonemap"));
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((color, "color"), (screen, "image")),
            ((screen, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        // The compiler stops first: the default effect takes a G-buffer,
        // not an image. The scheduler's NeverWritten check stays the last
        // line for effects that *do* take images — bloom wired to an
        // orphan target gets exactly that — so the label plumbing is
        // asserted by the scheduler tests over a hand-built list.
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::EffectInputMismatch { .. }
        ));
    }

    /// The chain P4 was for: deferred lighting writes a `resource.color`,
    /// bloom reads it and writes the frame's target — expressed as
    /// document edits, compiled and ordered with nothing bespoke.
    fn bloom_chain_document(bloom_from_pass_output: bool) -> Graph {
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("deferred bloom");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let lights = graph.add_node(doc::SOURCE_LIGHTS);
        let shadows = graph.add_node(doc::PASS_SHADOW);
        let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
        let material = graph.add(
            Node::new(doc::PASS_GEOMETRY)
                .with_label("deferred material")
                .with_setting(doc::SETTING_STAGE, "gbuffer"),
        );
        let scene_color = graph.add(Node::new(doc::RESOURCE_COLOR).with_label("scene"));
        let lighting = graph.add(
            Node::new(doc::PASS_SCREEN)
                .with_label("deferred lighting")
                .with_setting(doc::SETTING_EFFECT, "wxsl.deferred_lighting"),
        );
        let bloom = graph.add(
            Node::new(doc::PASS_SCREEN)
                .with_label("bloom")
                .with_setting(doc::SETTING_EFFECT, "wxsl.bloom"),
        );
        let present = graph.add_node(doc::PRESENT);
        let wire = |graph: &mut Graph, from: (NodeId, &str), to: (NodeId, &str)| {
            graph.wire(&registry, from, to).expect("wiring");
        };
        wire(&mut graph, (scene, "draws"), (shadows, "draws"));
        wire(&mut graph, (lights, "shadows"), (shadows, "into"));
        wire(&mut graph, (scene, "draws"), (material, "draws"));
        wire(&mut graph, (gbuffer, "gbuffer"), (material, "gbuffer"));
        wire(&mut graph, (gbuffer, "gbuffer"), (lighting, "gbuffer"));
        // The lighting pass writes *into* the colour target instead of the
        // frame's; bloom takes the image from the resource — or, in the
        // second spelling, straight from the lighting pass's output,
        // which stands for the same thing.
        wire(&mut graph, (scene_color, "color"), (lighting, "into"));
        if bloom_from_pass_output {
            wire(&mut graph, (lighting, "color"), (bloom, "image"));
        } else {
            wire(&mut graph, (scene_color, "color"), (bloom, "image"));
        }
        wire(&mut graph, (bloom, "color"), (present, "surface"));
        graph
    }

    #[test]
    fn a_deferred_lighting_bloom_chain_compiles_and_schedules() {
        for from_pass_output in [false, true] {
            let document = bloom_chain_document(from_pass_output);
            let compiled = compile(&document, &make_registry(), &effects(), &config())
                .expect("the chain compiles");
            let schedule = compiled.schedule().expect("and schedules");

            let position = |label: &str| {
                compiled
                    .passes()
                    .iter()
                    .position(|pass| pass.label == label)
                    .unwrap_or_else(|| panic!("no pass labelled `{label}`"))
            };
            let material = position("deferred material");
            let lighting = position("deferred lighting");
            let bloom = position("bloom");

            // Shadow passes, the material pass, then the two effects.
            assert_eq!(compiled.passes().len(), abi::MAX_LIGHTS + 3);
            assert_eq!(
                &schedule.order()[abi::MAX_LIGHTS..],
                &[material, lighting, bloom],
                "the chain orders by what it reads and writes"
            );

            // The lighting pass writes the intermediate; bloom reads it as
            // its one input and writes the frame's target, because its
            // `into` is unconnected.
            let scene_color = compiled.passes()[lighting]
                .color
                .first()
                .expect("the lighting pass writes the chain's resource")
                .resource;
            assert_ne!(scene_color, RenderGraph::TARGET);
            assert_eq!(
                compiled.passes()[lighting].reads.len(),
                abi::GBUFFER_BASE_TARGETS.len() + 1
            );
            let bloom_pass = &compiled.passes()[bloom];
            assert!(
                matches!(&bloom_pass.kind, PassKind::Screen { effect } if effect == "wxsl.bloom")
            );
            assert_eq!(bloom_pass.reads.len(), 1, "one image input, one read");
            assert_eq!(bloom_pass.reads[0].resource, scene_color);
            assert_eq!(bloom_pass.color.len(), 1);
            assert_eq!(bloom_pass.color[0].resource, RenderGraph::TARGET);
        }
    }

    #[test]
    fn a_separable_bloom_chains_horizontal_then_vertical() {
        // The one-pass kernel samples four texels apart and bands. The
        // replacement is two passes: extract and blur along x into a
        // resource, then blur along y and composite onto the unblurred
        // image. The vertical pass has two images, so it is the derived row.
        let registry = document_registry(&effects());
        let mut graph = wxsl_core::pipeline::document("separable bloom");
        let scene = graph.add_node(doc::SOURCE_SCENE);
        let lights = graph.add_node(doc::SOURCE_LIGHTS);
        let shadows = graph.add_node(doc::PASS_SHADOW);
        let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
        let material = graph.add(
            Node::new(doc::PASS_GEOMETRY)
                .with_label("deferred material")
                .with_setting(doc::SETTING_STAGE, "gbuffer"),
        );
        let scene_color = graph.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_label("scene")
                .with_setting(doc::SETTING_PRECISION, "hdr"),
        );
        let lighting = graph.add(
            Node::new(doc::PASS_SCREEN)
                .with_label("deferred lighting")
                .with_setting(doc::SETTING_EFFECT, "wxsl.deferred_lighting"),
        );
        let glow = graph.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_label("glow")
                .with_setting(doc::SETTING_PRECISION, "hdr"),
        );
        let bloom_x = graph.add(
            Node::new(doc::PASS_SCREEN)
                .with_label("bloom")
                .with_setting(doc::SETTING_EFFECT, "wxsl.bloom_x"),
        );
        let bloom_y = graph.add(
            Node::new(format!("{}wxsl.bloom_y", doc::PASS_SCREEN_PREFIX)).with_label("bloom-y"),
        );
        // The vertical pass is a derived row, so it cannot feed `present`
        // directly — present wants a `pass.screen` or a geometry pass. The
        // tonemap is that pass, which is also where the chain belongs.
        let linear = graph.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_label("linear")
                .with_setting(doc::SETTING_PRECISION, "hdr"),
        );
        let tonemap = graph.add(
            Node::new(doc::PASS_SCREEN)
                .with_label("tonemap")
                .with_setting(doc::SETTING_EFFECT, "wxsl.tonemap"),
        );
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((scene, "draws"), (shadows, "draws")),
            ((lights, "shadows"), (shadows, "into")),
            ((scene, "draws"), (material, "draws")),
            ((gbuffer, "gbuffer"), (material, "gbuffer")),
            ((gbuffer, "gbuffer"), (lighting, "gbuffer")),
            ((scene_color, "color"), (lighting, "into")),
            ((scene_color, "color"), (bloom_x, "image")),
            ((glow, "color"), (bloom_x, "into")),
            ((scene_color, "color"), (bloom_y, "color")),
            ((glow, "color"), (bloom_y, "glow")),
            ((linear, "color"), (bloom_y, "into")),
            ((linear, "color"), (tonemap, "image")),
            ((tonemap, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        let compiled =
            compile(&graph, &registry, &effects(), &config()).expect("the chain compiles");
        let schedule = compiled.schedule().expect("and schedules");
        let position = |label: &str| {
            compiled
                .passes()
                .iter()
                .position(|pass| pass.label == label)
                .unwrap_or_else(|| panic!("no pass labelled `{label}`"))
        };
        let horizontal = position("bloom");
        let vertical = position("bloom-y");
        assert!(
            schedule
                .order()
                .iter()
                .position(|index| *index == horizontal)
                < schedule.order().iter().position(|index| *index == vertical),
            "the vertical pass reads the horizontal pass's resource"
        );
        let horizontal_pass = &compiled.passes()[horizontal];
        assert!(
            matches!(&horizontal_pass.kind, PassKind::Screen { effect } if effect == "wxsl.bloom_x")
        );
        assert_eq!(horizontal_pass.reads.len(), 1);
        let vertical_pass = &compiled.passes()[vertical];
        assert!(
            matches!(&vertical_pass.kind, PassKind::Screen { effect } if effect == "wxsl.bloom_y")
        );
        assert_eq!(
            vertical_pass.reads.len(),
            2,
            "the unblurred image and the glow"
        );
        let glow_read = vertical_pass.reads[1].resource;
        let vertical_wrote = vertical_pass.color[0].resource;
        let horizontal_wrote = horizontal_pass.color[0].resource;
        assert_ne!(vertical_wrote, RenderGraph::TARGET);
        assert_eq!(
            horizontal_wrote, glow_read,
            "the glow the horizontal pass writes is the vertical pass's second read"
        );
        let tonemap_at = position("tonemap");
        assert_eq!(
            compiled.passes()[tonemap_at].color[0].resource,
            RenderGraph::TARGET
        );
        assert!(
            schedule.order().iter().position(|index| *index == vertical)
                < schedule
                    .order()
                    .iter()
                    .position(|index| *index == tonemap_at),
            "tonemap reads what the vertical pass wrote"
        );
    }

    #[test]
    fn reading_the_presented_target_through_an_image_input_is_reported() {
        // The one un-compilable chain shape: the lighting pass writes the
        // frame's target and bloom's image input is wired to its output.
        // What is on screen cannot also be sampled; the error says what to
        // wire instead.
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("read the target");
        let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
        let lighting = graph.add_node(doc::PASS_SCREEN);
        let bloom =
            graph.add(Node::new(doc::PASS_SCREEN).with_setting(doc::SETTING_EFFECT, "wxsl.bloom"));
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((gbuffer, "gbuffer"), (lighting, "gbuffer")),
            ((lighting, "color"), (bloom, "image")),
            ((bloom, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        match errors_of(&graph, &config()) {
            PipelineError::ImageFromPass { node, fed_by } => {
                assert_eq!(fed_by, "screen effect", "the lighting pass's default label");
                assert!(!node.is_empty());
            }
            other => panic!("expected an image-from-pass error, got {other:?}"),
        }
    }

    #[test]
    fn an_input_the_effect_does_not_declare_is_reported() {
        // Bloom takes an image, not a G-buffer — wiring one is a named
        // mismatch, not a bind group that happens to line up.
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("bloom with a gbuffer");
        let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
        let color = graph.add_node(doc::RESOURCE_COLOR);
        let bloom =
            graph.add(Node::new(doc::PASS_SCREEN).with_setting(doc::SETTING_EFFECT, "wxsl.bloom"));
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((gbuffer, "gbuffer"), (bloom, "gbuffer")),
            ((color, "color"), (bloom, "image")),
            ((bloom, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        match errors_of(&graph, &config()) {
            PipelineError::EffectInputMismatch { effect, reason, .. } => {
                assert_eq!(effect, "wxsl.bloom");
                assert!(reason.contains("G-buffer"), "{reason}");
            }
            other => panic!("expected an input mismatch, got {other:?}"),
        }
    }

    #[test]
    fn a_resource_document_can_ask_for_half_scale_and_history() {
        let registry = make_registry();
        let mut graph = wxsl_core::pipeline::document("half res history");
        let color = graph.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_label("accumulation")
                .with_setting(doc::SETTING_PRECISION, "hdr")
                .with_setting(doc::SETTING_SCALE, "0.5")
                .with_setting(doc::SETTING_HISTORY, "1"),
        );
        // Bloom takes images, so a document naming a half-res history
        // target in a chain actually compiles since P4 — and the compiled
        // descriptor is the assertion, not a hand-mirrored one.
        let bloom =
            graph.add(Node::new(doc::PASS_SCREEN).with_setting(doc::SETTING_EFFECT, "wxsl.bloom"));
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((color, "color"), (bloom, "image")),
            ((bloom, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        let compiled = compile(&graph, &registry, &effects(), &config()).expect("compiles");
        let accumulation = compiled
            .resources()
            .iter()
            .find(|resource| resource.label == "accumulation")
            .expect("the document's colour target is declared");
        let crate::pass::ResourceShape::Texture { format, extent, .. } = accumulation.shape else {
            panic!("a colour target is a texture");
        };
        assert_eq!(format, wgpu::TextureFormat::Rgba16Float, "hdr");
        assert_eq!(extent.resolve(128, 128), (64, 64), "half scale, rounded");
        assert_eq!(
            accumulation.persistence.ring_length(),
            2,
            "history: 1 is a ring of two"
        );

        // Nothing in this document writes the target, which the compiler
        // has no opinion on — an effect might not read its whole input —
        // and the scheduler names it, by the document label.
        let error = compiled.schedule().expect_err("nobody writes the target");
        assert!(
            error.to_string().contains("accumulation"),
            "the orphan is named by its document label: {error}"
        );
    }
    // -- compute passes and buffers (plan3 N3) ----------------------------

    use crate::effect::{BRDF_LUT, LUT_VIEW, RAMP_FILL, RAMP_VIEW};
    use crate::pass::ResourceShape;

    /// The effects the buffer-ramp documents below compile against.
    fn ramp_effects() -> EffectRegistry {
        EffectRegistry::shipped().with(RAMP_FILL).with(RAMP_VIEW)
    }

    /// The buffer proof as a document: a `resource.buffer`, a
    /// `pass.compute.ramp_fill` writing it, a `pass.screen` running
    /// `ramp_view` reading it back through its `buffer` socket, present.
    /// Both passes wire from the resource — the write is a declaration on
    /// the compute node, the read a wire on the screen one.
    fn ramp_document() -> Graph {
        let registry = document_registry(&ramp_effects());
        let mut graph = wxsl_core::pipeline::document("buffer ramp");
        let buffer = graph.add(
            Node::new(doc::RESOURCE_BUFFER)
                .with_label("ramp")
                .with_setting(doc::SETTING_BYTES, "1024"),
        );
        let fill = graph.add(
            Node::new(format!("{}wxsl.ramp_fill", doc::PASS_COMPUTE_PREFIX))
                .with_label("fill ramp"),
        );
        let view = graph.add(
            Node::new(doc::PASS_SCREEN)
                .with_label("show ramp")
                .with_setting(doc::SETTING_EFFECT, "wxsl.ramp_view"),
        );
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((buffer, "buffer"), (fill, "ramp")),
            ((buffer, "buffer"), (view, "buffer")),
            ((view, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("ramp wiring");
        }
        graph
    }

    /// The same proof as a hand-built pass list — the reference the
    /// document must compile to. The clear colour is the config's, because
    /// the screen pass writes the frame's target and the clear belongs to
    /// the frame (ADR 0039).
    fn ramp_graph() -> RenderGraph {
        let mut graph = RenderGraph::new(target().format);
        let ramp = graph.resource(ResourceDesc::buffer("ramp", 1024));
        graph.pass(PassDesc::compute("fill ramp", "wxsl.ramp_fill").with_write(ramp));
        graph.pass(
            PassDesc::screen("show ramp", "wxsl.ramp_view")
                .with_color(Attachment::clear(
                    RenderGraph::TARGET,
                    config().target.clear_color,
                ))
                .with_reads([Read::current(ramp)]),
        );
        graph
    }

    #[test]
    fn the_ramp_document_compiles_to_the_hand_built_pass_list() {
        let document = ramp_document();
        let compiled = compile(
            &document,
            &document_registry(&ramp_effects()),
            &ramp_effects(),
            &config(),
        )
        .expect("compiles");
        let hand_built = ramp_graph();
        assert!(
            same_graph(&compiled, &hand_built),
            "compiled:\n{compiled:#?}\nhand built:\n{hand_built:#?}"
        );
        // And it schedules like it: the compute pass orders before the
        // screen pass through the buffer, the never-aliased resource.
        let a = compiled.schedule().expect("the compiled list schedules");
        let b = hand_built
            .schedule()
            .expect("the hand-built list schedules");
        assert_eq!(a.order(), b.order());
        assert_eq!(a.slots(), b.slots());
    }

    #[test]
    fn a_compute_pass_by_an_unknown_effect_is_a_named_error() {
        // The node definition exists — the registry was built from a
        // registry that had the effect — but the compile is handed an
        // effect registry that does not: the document outlived the
        // registration that made it.
        let document = ramp_document();
        let registry = document_registry(&ramp_effects());
        let known = crate::effect::EffectRegistry::shipped();
        match compile(&document, &registry, &known, &config()) {
            Err(PipelineError::UnknownEffect { effect, .. }) => {
                assert_eq!(effect, "wxsl.ramp_fill");
            }
            other => panic!("expected an unknown effect, got {other:?}"),
        }
    }

    #[test]
    fn a_buffer_declares_its_bytes_and_its_history() {
        let effects = ramp_effects();
        let registry = document_registry(&effects);
        let mut graph = wxsl_core::pipeline::document("ring");
        let buffer = graph.add(
            Node::new(doc::RESOURCE_BUFFER)
                .with_label("ring")
                .with_setting(doc::SETTING_BYTES, "4096")
                .with_setting(doc::SETTING_HISTORY, "1"),
        );
        let fill = graph.add(
            Node::new(format!("{}wxsl.ramp_fill", doc::PASS_COMPUTE_PREFIX)).with_label("fill"),
        );
        let view = graph.add(
            Node::new(doc::PASS_SCREEN)
                .with_label("view")
                .with_setting(doc::SETTING_EFFECT, "wxsl.ramp_view"),
        );
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((buffer, "buffer"), (fill, "ramp")),
            ((buffer, "buffer"), (view, "buffer")),
            ((view, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("ring wiring");
        }
        let compiled = compile(&graph, &registry, &effects, &config()).expect("compiles");
        let ring = compiled
            .resources()
            .iter()
            .find(|desc| desc.label == "ring")
            .expect("the buffer is declared");
        assert_eq!(
            ring.shape,
            ResourceShape::Buffer {
                size: 4096,
                usage: wgpu::BufferUsages::empty(),
            }
        );
        assert_eq!(ring.persistence, Persistence::Persistent { history: 1 });
    }

    #[test]
    fn a_buffer_of_no_bytes_is_a_named_error() {
        let registry = document_registry(&ramp_effects());
        let mut graph = ramp_document();
        let buffer = graph
            .nodes()
            .find(|(_, node)| node.def == doc::RESOURCE_BUFFER)
            .map(|(id, _)| id)
            .expect("the ramp has a buffer");
        graph.set_setting(buffer, doc::SETTING_BYTES, "0");
        match compile(&graph, &registry, &ramp_effects(), &config()) {
            Err(PipelineError::BadBufferSize { node, value }) => {
                assert_eq!(node, "ramp");
                assert_eq!(value, "0");
            }
            other => panic!("expected a buffer size error, got {other:?}"),
        }
    }

    #[test]
    fn a_fixed_size_target_and_an_once_bake_make_the_lut_demo_a_document() {
        // The brdf-lut gallery demo, as a document: a 64x64 fixed HDR
        // target, a `once` compute bake writing it, a view presenting it.
        // Fixed size is the setting the hand-built list needed
        // `Extent::Fixed` for; `once` is the policy setting.
        let effects = EffectRegistry::shipped()
            .with(crate::effect::BRDF_LUT)
            .with(LUT_VIEW);
        let registry = document_registry(&effects);
        let mut graph = wxsl_core::pipeline::document("lut");
        let lut = graph.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_label("brdf lut")
                .with_setting(doc::SETTING_PRECISION, "hdr")
                .with_setting(doc::SETTING_SIZE, "64x64"),
        );
        let bake = graph.add(
            Node::new(format!("{}wxsl.brdf_lut", doc::PASS_COMPUTE_PREFIX))
                .with_label("lut bake")
                .with_setting(doc::SETTING_POLICY, "once"),
        );
        let view = graph.add(
            Node::new(doc::PASS_SCREEN)
                .with_label("lut view")
                .with_setting(doc::SETTING_EFFECT, "wxsl.lut_view"),
        );
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((lut, "color"), (bake, "lut")),
            ((lut, "color"), (view, "image")),
            ((view, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("lut wiring");
        }
        let compiled = compile(&graph, &registry, &effects, &config()).expect("compiles");
        let desc = compiled
            .resources()
            .iter()
            .find(|desc| desc.label == "brdf lut")
            .expect("the lut is declared");
        assert_eq!(
            desc.shape,
            ResourceShape::Texture {
                extent: Extent::Fixed {
                    width: 64,
                    height: 64
                },
                dimension: Dimension::D2,
                layers: 1,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::empty(),
            }
        );
        // `once` promoted the target to stable storage — the scheduler's
        // rule for a pass that skips frames, derived by the compiler.
        assert_eq!(desc.persistence, Persistence::Persistent { history: 0 });
        compiled.schedule().expect("schedules");
    }

    #[test]
    fn an_imported_target_is_the_hosts_and_refuses_a_size() {
        // A bake table: the host (the scene's material) created the
        // texture and samples it; the pass list only writes through it.
        // `imported` says so — the table is nobody's to allocate here —
        // and the size settings, which would size a texture this document
        // never sees created, are refused rather than ignored.
        let effects = EffectRegistry::shipped().with(crate::effect::BRDF_LUT);

        let document = |sized: bool| {
            let registry = document_registry(&effects);
            let mut graph = wxsl_core::pipeline::document("imported table");
            let table = graph.add(
                Node::new(doc::RESOURCE_COLOR)
                    .with_label("roughness_bake")
                    .with_setting(doc::SETTING_PRECISION, "hdr")
                    .with_setting(doc::SETTING_IMPORTED, "true"),
            );
            if sized {
                graph.set_setting(table, doc::SETTING_SIZE, "64x64");
            }
            let bake = graph.add(
                Node::new(format!("{}wxsl.brdf_lut", doc::PASS_COMPUTE_PREFIX))
                    .with_label("table bake")
                    .with_setting(doc::SETTING_POLICY, "once"),
            );
            let view = graph.add(
                Node::new(doc::PASS_SCREEN)
                    .with_label("view")
                    .with_setting(doc::SETTING_EFFECT, "wxsl.tonemap"),
            );
            let present = graph.add_node(doc::PRESENT);
            for (from, to) in [
                ((table, "color"), (bake, "lut")),
                ((table, "color"), (view, "image")),
                ((view, "color"), (present, "surface")),
            ] {
                graph.wire(&registry, from, to).expect("table wiring");
            }
            (registry, graph)
        };

        let (registry, graph) = document(false);
        let compiled = compile(&graph, &registry, &effects, &config()).expect("compiles");
        let desc = compiled
            .resources()
            .iter()
            .find(|desc| desc.label == "roughness_bake")
            .expect("the table is declared");
        assert!(desc.imported, "the table is the host's, not the pool's");
        // The scheduler's stable-storage rule holds by ownership: a `once`
        // pass writing the host's texture is fine, because the texture
        // cannot have moved under it.
        compiled.schedule().expect("schedules");

        let (registry, sized) = document(true);
        let error = compile(&sized, &registry, &effects, &config())
            .expect_err("an imported target is not this document's to size");
        assert!(
            error.to_string().contains("roughness_bake") && error.to_string().contains("`size`"),
            "{error}"
        );
    }

    #[test]
    fn a_compute_pass_writing_another_passs_output_is_a_named_error() {
        // The bake's `lut` socket takes a colour target, and a screen
        // pass's colour output *is* one as far as typing can tell — so
        // the graph model passes it and the compiler names it: another
        // pass's output is not storage anybody owns.
        let effects = EffectRegistry::shipped().with(BRDF_LUT).with(LUT_VIEW);
        let registry = document_registry(&effects);
        let mut graph = wxsl_core::pipeline::document("wrong end");
        let lut = graph.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_label("brdf lut")
                .with_setting(doc::SETTING_PRECISION, "hdr"),
        );
        let view = graph.add(
            Node::new(doc::PASS_SCREEN)
                .with_label("view")
                .with_setting(doc::SETTING_EFFECT, "wxsl.lut_view"),
        );
        let bake = graph.add(
            Node::new(format!("{}wxsl.brdf_lut", doc::PASS_COMPUTE_PREFIX)).with_label("bake"),
        );
        let present = graph.add_node(doc::PRESENT);
        // The view reads the lut and presents; the bake's `lut` write is
        // wired from the *screen pass's* colour output.
        for (from, to) in [
            ((lut, "color"), (view, "image")),
            ((view, "color"), (bake, "lut")),
            ((view, "color"), (present, "surface")),
        ] {
            graph
                .wire(&registry, from, to)
                .expect("typing allows it; the compiler is what refuses");
        }
        match compile(&graph, &registry, &effects, &config()) {
            Err(PipelineError::WriteIntoPassOutput { node }) => {
                assert_eq!(node, "bake");
            }
            other => panic!("expected a write-into-pass error, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod preset_dump {
    //! Writes the preset files from [`super::stock_document`]. Run once
    //! with `cargo test -p wxsl-render --lib preset_dump -- --ignored`,
    //! then removed — the permanent round-trip test in `pipeline::tests`
    //! asserts the shipped file parses back to exactly this document.
    use super::*;
    use std::fs;
    use std::path::Path;

    #[test]
    #[ignore = "writes the shipped preset files; run by hand"]
    fn dump() {
        for stock in StockPipeline::ALL {
            let document = stock_document(*stock);
            let json = serde_json::to_string_pretty(&document).expect("serializes");
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/presets")
                .join(format!("{}.pipeline.json", stock.name()));
            fs::create_dir_all(path.parent().unwrap()).expect("dir");
            fs::write(&path, json + "\n").expect("write");
            println!("wrote {}", path.display());
        }
    }
}
