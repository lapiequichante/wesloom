//! The pipeline compiler: a [`Graph`] of document nodes in, a
//! [`FrameGraph`] out — one pure function, no device.
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
//! checks stay as the last line, and [`FrameGraph::schedule`] is
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

#[cfg(test)]
use crate::types::BufferUsages;
use crate::types::{Color, CompareFunction, TextureFormat, TextureUsages};

use wxsl_core::abi::{self, MaterialStage};
#[cfg(test)]
use wxsl_core::graph::Node;
use wxsl_core::graph::{Graph, NodeId, SocketRef};
use wxsl_core::node::NodeRegistry;
use wxsl_core::pipeline as doc;
use wxsl_core::scene::{self, TagExpr};

use crate::effect::{
    Downlevel, Effect, EffectInputKind, EffectOutputShape, EffectRegistry, PEEL_BOUNDS,
    PEEL_COMPOSITE, PEEL_DEPTH_BOUNDS, PEEL_OVER, PEEL_UNDER,
};
use crate::graph::FrameGraph;
use crate::pass::{
    Attachment, DepthAttachment, DrawSource, Extent, LayerFilter, PassDesc, PassKind, PassState,
    PassView, Persistence, Policy, Read, ResourceDesc, ResourceId, SortOrder, ViewportRect,
    ALPHA_OVER, DEPTH_FORMAT, MAX_BLEND, PREMULTIPLIED_OVER, PREMULTIPLIED_UNDER,
};
#[cfg(test)]
use crate::pipeline::StockPipeline;
use crate::pipeline::{gbuffer_format, PipelineConfig};

mod passes;
mod peel;
mod resources;
#[cfg(test)]
mod tests;

/// What is wrong with a pipeline document.
///
/// Every variant names the document node at fault, by its display label —
/// the string the canvas shows.
#[derive(Clone, Debug, PartialEq)]
pub enum PipelineError {
    /// An environment recipe cannot be resolved from the document.
    Environment {
        /// The authored node.
        node: String,
        /// The violated contract.
        reason: String,
    },
    /// A `pass.bloom` node's pyramid cannot be built from the document.
    BloomPyramid {
        /// The authored node.
        node: String,
        /// The violated contract.
        reason: String,
    },
    /// A `pass.dof` node's chain cannot be built from the document.
    Dof {
        /// The authored node.
        node: String,
        /// The violated contract.
        reason: String,
    },
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
    /// A pass runs an effect the pipeline's device profile cannot run: it
    /// declared itself native-only, or it is a compute effect with no
    /// fragment arm (ADR 0075).
    NativeOnly {
        /// The pass, as the compiled pass list labels it.
        pass: String,
        /// The effect it runs.
        effect: String,
        /// The profile it was compiled for.
        profile: crate::profile::Profile,
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
    /// A pass names a sort order ([`crate::pass::SortOrder`]) that is not
    /// one (plan5 D3).
    UnknownSort {
        /// The pass node.
        node: String,
        /// The sort it named.
        sort: String,
    },
    /// A pass names a layer filter ([`crate::pass::LayerFilter`]) that is
    /// not one of the authorable shapes (plan5 D5).
    UnknownLayers {
        /// The pass node.
        node: String,
        /// The layers it named.
        layers: String,
    },
    /// A pass names a blend mode that is not one of the document's.
    UnknownBlend {
        /// The pass node.
        node: String,
        /// The blend it named.
        blend: String,
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
    /// Two `source.lights` nodes — two shadow atlases, and the frame
    /// group binds exactly one.
    TwoShadowSources {
        /// Both nodes' labels.
        nodes: Vec<String>,
    },
    /// The config's `shadow_atlas_size` is not an atlas edge
    /// (`abi::valid_shadow_atlas_size`), named by the node declaring the
    /// atlas.
    ShadowAtlasSize {
        /// The `source.lights` node.
        node: String,
        /// The edge asked for.
        size: u32,
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
    /// A `pass.peel` is wired to something it cannot sample, or a material
    /// pass names a peel stage. The peel draws the transparent tag; the
    /// stage rows are not a `pass.geometry` setting.
    Peel {
        /// The document node.
        node: String,
        /// What is wrong with it.
        reason: String,
    },
}

impl core::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PipelineError::Environment { node, reason } => {
                write!(f, "environment `{node}`: {reason}")
            }
            PipelineError::BloomPyramid { node, reason } => {
                write!(f, "bloom pyramid `{node}`: {reason}")
            }
            PipelineError::Dof { node, reason } => {
                write!(f, "depth of field `{node}`: {reason}")
            }
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
            PipelineError::NativeOnly {
                pass,
                effect,
                profile,
            } => write!(
                f,
                "pass `{pass}` runs `{effect}`, which is native-only: a device \
                 with the `{profile}` profile cannot run it (ADR 0075)"
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
                     from a `pass.shadow` node, which fills one tile per light"
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
                 one — standard, hdr, float, scalar or pair"
            ),
            PipelineError::UnknownPolicy { node, policy } => write!(
                f,
                "pass `{node}` runs {policy:?}, which is not a policy — per frame, once, \
                 on resize or on demand"
            ),
            PipelineError::UnknownSort { node, sort } => write!(
                f,
                "pass `{node}` sorts {sort:?}, which is not a sort order — none, \
                 front to back or back to front"
            ),
            PipelineError::UnknownLayers { node, layers } => write!(
                f,
                "pass `{node}` draws {layers:?}, which is not a layer tier — all \
                 or sorted"
            ),
            PipelineError::UnknownBlend { node, blend } => write!(
                f,
                "pass `{node}` blends {blend:?}, which is not a blend mode — \
                 opaque or alpha over"
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
                "`{first}` and `{second}` both clear the frame's target — leave `into` \
                 unconnected on the head of the chain only; a later pass that writes \
                 it must load it, as a sorted-tier pass does (plan5 D5)"
            ),
            PipelineError::TwoShadowSources { nodes } => write!(
                f,
                "the pipeline declares shadow maps twice ({}) — the frame group binds \
                 exactly one array",
                nodes.join(", ")
            ),
            PipelineError::ShadowAtlasSize { node, size } => write!(
                f,
                "the shadow atlas of `{node}` cannot be {size}² texels: its edge is a power \
                 of two from {} to {}",
                wxsl_core::abi::MIN_SHADOW_ATLAS_SIZE,
                wxsl_core::abi::MAX_SHADOW_ATLAS_SIZE
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
            PipelineError::Peel { node, reason } => {
                write!(f, "depth peel `{node}`: {reason}")
            }
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
/// [`FrameGraph::schedule`] can catch is still caught there — this
/// function's own errors are the ones the scheduler *cannot* name, because
/// they are about nodes the scheduler never saw.
pub fn compile(
    document: &Graph,
    registry: &NodeRegistry,
    effects: &EffectRegistry,
    config: &PipelineConfig,
) -> Result<FrameGraph, PipelineError> {
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

/// The peel draws this and nothing else. The scene source that feeds the
/// opaque pass keeps its own tag expression.
fn transparent_draws() -> DrawSource {
    DrawSource::Scene(TagExpr::tag(scene::TAG_TRANSPARENT))
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
    graph: FrameGraph,
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
    target_writers: Vec<(String, bool)>,
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
            graph: FrameGraph::new(config.target.format),
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

    fn compile(mut self) -> Result<FrameGraph, PipelineError> {
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

        for node in &nodes {
            if self.kind(*node) == Some(doc::PASS_ENVIRONMENT) {
                self.environment_bake(*node)?;
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
                Some(doc::PASS_PEEL) => self.peel_passes(*node)?,
                Some(doc::PASS_REFRACTION) => self.refraction_passes(*node)?,
                Some(doc::PASS_SHADOW) => self.shadow_passes(*node)?,
                Some(doc::PASS_BLOOM) => self.bloom_pyramid(*node)?,
                Some(doc::PASS_DOF) => self.depth_of_field(*node)?,
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
        self.check_profile()?;
        Ok(self.graph)
    }

    /// Refuse, by pass, every effect the profile cannot run — after every
    /// expansion, so a `pass.peel` or a `pass.dof` is caught by the effect
    /// it expands into rather than by a list of node kinds (ADR 0075).
    fn check_profile(&self) -> Result<(), PipelineError> {
        if self.config.profile.is_full() {
            return Ok(());
        }
        for pass in self.graph.passes() {
            let (PassKind::Screen { effect } | PassKind::Compute { effect }) = &pass.kind else {
                continue;
            };
            if self
                .effects
                .get(effect)
                .is_some_and(|known| known.downlevel == Downlevel::NativeOnly)
            {
                return Err(PipelineError::NativeOnly {
                    pass: pass.label.clone(),
                    effect: effect.clone(),
                    profile: self.config.profile,
                });
            }
        }
        Ok(())
    }
}

impl Compiler<'_> {
    fn finish(&mut self, presents: Vec<NodeId>) -> Result<(), PipelineError> {
        if let Some(layout) = self.layout.take() {
            self.graph =
                std::mem::replace(&mut self.graph, FrameGraph::new(self.config.target.format))
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
                Some(doc::PASS_GEOMETRY)
                    | Some(doc::PASS_SCREEN)
                    | Some(doc::PASS_PEEL)
                    | Some(doc::PASS_REFRACTION)
            ) && self.pass_colors.get(&feeder) == Some(&FrameGraph::TARGET);
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

        // Several passes may write the frame's target in sequence, but at
        // most the *first* of them may clear it: everything after it must
        // load, or it would throw what the pass before it drew.
        let clears = self
            .target_writers
            .iter()
            .filter(|(_, loads)| !loads)
            .count();
        if clears > 1 || clears == 1 && self.target_writers[0].1 {
            return Err(PipelineError::TwoTargetWriters {
                first: self.target_writers[0].0.clone(),
                second: self.target_writers[1].0.clone(),
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
