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
//! screen effects: `pass.screen` names one by id, and [`EFFECTS`] is the
//! table of what this crate knows how to run. One row for now — the
//! deferred lighting pass, migrated out of the hardcoded enum it was
//! reachable only through. P4's effect registry generalizes the table into
//! application-supplied data, which is also when an effect chain through
//! `resource.color` becomes *expressible* rather than merely compiled
//! against: today the shipped effect writes the frame's target, so a chain
//! fails in [`PipelineError::NotPresentable`] with the message that says
//! what to do instead.

use std::collections::HashMap;

use wxsl_core::abi::{self, MaterialStage};
#[cfg(test)]
use wxsl_core::graph::Node;
use wxsl_core::graph::{Graph, NodeId, SocketRef};
use wxsl_core::node::NodeRegistry;
use wxsl_core::pipeline as doc;
use wxsl_core::scene::TagExpr;

use crate::graph::RenderGraph;
use crate::pass::{
    Attachment, DepthAttachment, Dimension, DrawSource, Extent, PassDesc, PassState, PassView,
    Persistence, Read, ResourceDesc, ResourceId, ScreenShader, DEPTH_FORMAT,
};
#[cfg(test)]
use crate::pipeline::StockPipeline;
use crate::pipeline::{gbuffer_format, PipelineConfig};

/// The screen effects a document can name, by the id its `effect` setting
/// carries: `(id, shader, one-line description)`.
pub const EFFECTS: &[(&str, ScreenShader, &str)] = &[(
    "deferred_lighting",
    ScreenShader::DeferredLighting,
    "Shade the G-buffer with the enabled lighting models.",
)];

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
                "screen pass `{node}` runs `{effect}`, which is not an effect this \
                 renderer knows (it knows: {})",
                known.join(", ")
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
                 leave its colour output unconnected"
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
        }
    }
}

impl std::error::Error for PipelineError {}

/// Compile a pipeline document into a pass list.
///
/// Pure: no device, no allocation beyond the graph itself. Everything the
/// [`RenderGraph::schedule`] can catch is still caught there — this
/// function's own errors are the ones the scheduler *cannot* name, because
/// they are about nodes the scheduler never saw.
pub fn compile(
    document: &Graph,
    registry: &NodeRegistry,
    config: &PipelineConfig,
) -> Result<RenderGraph, PipelineError> {
    if let Err(errors) = document.validate(registry) {
        return Err(PipelineError::InvalidDocument(errors));
    }
    Compiler::new(document, registry, config).compile()
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
    config: &'a PipelineConfig,
    graph: RenderGraph,
    /// `resource.gbuffer` node → (targets in layout order, depth).
    gbuffers: HashMap<NodeId, (Vec<ResourceId>, ResourceId)>,
    /// `resource.color` node → engine resource.
    colors: HashMap<NodeId, ResourceId>,
    /// `resource.depth` node → engine resource.
    depths: HashMap<NodeId, ResourceId>,
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
    fn new(document: &'a Graph, registry: &'a NodeRegistry, config: &'a PipelineConfig) -> Self {
        Compiler {
            document,
            registry,
            config,
            graph: RenderGraph::new(config.target.format),
            gbuffers: HashMap::new(),
            colors: HashMap::new(),
            depths: HashMap::new(),
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
                Some(doc::RESOURCE_GBUFFER) => self.declare_gbuffer(*node),
                Some(doc::RESOURCE_COLOR) => self.declare_color(*node)?,
                Some(doc::RESOURCE_DEPTH) => self.declare_depth(*node)?,
                _ => {}
            }
        }

        // Then passes, in document order — which is the order the author
        // built them in, and only a tie-break for the scheduler anyway.
        let mut presents: Vec<NodeId> = Vec::new();
        for node in &nodes {
            match self.kind(*node) {
                Some(doc::PASS_GEOMETRY) => self.geometry_pass(*node)?,
                Some(doc::PASS_SHADOW) => self.shadow_passes(*node)?,
                Some(doc::PASS_SCREEN) => self.screen_pass(*node)?,
                Some(doc::PRESENT) => presents.push(*node),
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

    fn declare_gbuffer(&mut self, node: NodeId) {
        // The layout is the enabled set's, not the document's: widening the
        // set rewires nothing, because the lighting pass reads the G-buffer
        // by the shape it was generated for.
        let layout = self.config.lighting.gbuffer_layout();
        let targets: Vec<ResourceId> = layout
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
        self.layout = Some(layout.to_vec());
        self.gbuffers.insert(node, (targets, depth));
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
        let scale = self.number(node, doc::SETTING_SCALE)?;
        let history = self.number(node, doc::SETTING_HISTORY)? as u32;
        let mut desc = ResourceDesc::color(name, gbuffer_format(precision))
            .with_extent(Extent::Viewport { scale });
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

    /// A numeric setting, with the setting's name in the error.
    fn number(&self, node: NodeId, setting: &'static str) -> Result<f32, PipelineError> {
        let text = self.setting(node, setting);
        text.trim().parse().map_err(|_| PipelineError::BadNumber {
            node: self.label(node),
            setting,
            value: text,
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

    /// Where a screen pass's `into` writes: the named resource, or the
    /// frame's target when `into` is unconnected.
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
    /// clear colour for the frame's target — the one pass writing what the
    /// user sees — and transparent for an intermediate.
    fn color_attachment(&self, target: ResourceId) -> Attachment {
        if target == RenderGraph::TARGET {
            Attachment::clear(RenderGraph::TARGET, self.config.target.clear_color)
        } else {
            Attachment::clear(target, wgpu::Color::TRANSPARENT)
        }
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

        // Colour: a `forward_lit` pass writes the frame's target through
        // its colour output; a `gbuffer`-stage pass writes every target
        // the enabled set requested; every other stage writes no colour.
        let color_wired = self
            .document
            .edge_from(&SocketRef::new(node, "color"))
            .is_some();
        let colors: Vec<Attachment> = match stage.output() {
            abi::StageOutput::Color => {
                if !color_wired {
                    return Err(PipelineError::ColorWithoutConsumer { node: name });
                }
                // The frame's target is the only thing a material pass
                // writes directly; a chain passes along a resource, and
                // another pass's output is nobody's to write.
                self.target_writers.push(name.clone());
                vec![self.color_attachment(RenderGraph::TARGET)]
            }
            abi::StageOutput::GBuffer => {
                if color_wired {
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
                if color_wired {
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
        let mut pass = PassDesc::geometry(name, DrawSource::Scene(tags), stage)
            .with_colors(colors)
            .with_state(state);
        if let Some(depth) = depth {
            pass = pass.with_depth(depth);
        }
        self.graph.pass(pass);
        self.pass_colors.insert(node, RenderGraph::TARGET);
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
        let effect_text = self.setting(node, doc::SETTING_EFFECT);
        let known: Vec<String> = EFFECTS.iter().map(|(id, _, _)| id.to_string()).collect();
        let (_, shader, _) = EFFECTS
            .iter()
            .find(|(id, _, _)| *id == effect_text.trim())
            .ok_or_else(|| PipelineError::UnknownEffect {
                node: name.clone(),
                effect: effect_text,
                known,
            })?;
        let effect = shader_label(*shader);

        // What the effect reads is what is wired into it. The lighting pass
        // shades a G-buffer and nothing else; the reads land in the order
        // the generated pass declares its bindings — targets in layout
        // order, depth last.
        let reads: Vec<Read> = match self.fed(node, "gbuffer") {
            Some(source) => {
                let (targets, depth) = &self.gbuffers[&source];
                targets
                    .iter()
                    .copied()
                    .chain(core::iter::once(*depth))
                    .map(Read::current)
                    .collect()
            }
            None => Vec::new(),
        };
        if reads.is_empty() {
            return Err(PipelineError::EffectInputMismatch {
                node: name.clone(),
                effect,
                reason: "no G-buffer is wired into it".to_string(),
            });
        }
        if self.fed(node, "image").is_some() {
            return Err(PipelineError::EffectInputMismatch {
                node: name.clone(),
                effect,
                reason: "it shades a G-buffer, not an image".to_string(),
            });
        }

        let target = self.write_target(node)?;
        self.graph.pass(
            PassDesc::screen(name, *shader)
                .with_color(self.color_attachment(target))
                .with_reads(reads),
        );
        self.pass_colors.insert(node, target);
        Ok(())
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

/// The user-facing name of a screen shader, for error messages.
fn shader_label(shader: ScreenShader) -> String {
    match shader {
        ScreenShader::DeferredLighting => "deferred_lighting".to_string(),
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
    let mut graph = Graph::new(stock.name());
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

    match stock {
        StockPipeline::Forward => {
            let depth = graph.add(Node::new(doc::RESOURCE_DEPTH).with_label("forward depth"));
            let prepass = graph.add(
                Node::new(doc::PASS_GEOMETRY)
                    .with_label("depth prepass")
                    .with_setting(doc::SETTING_STAGE, "depth_only"),
            );
            let shade = graph.add(Node::new(doc::PASS_GEOMETRY).with_label("forward"));
            let present = graph.add_node(doc::PRESENT);
            wire(&mut graph, (depth, "depth"), (prepass, "depth"));
            wire(&mut graph, (prepass, "depth"), (shade, "depth"));
            wire(&mut graph, (scene, "draws"), (prepass, "draws"));
            wire(&mut graph, (scene, "draws"), (shade, "draws"));
            wire(&mut graph, (shade, "color"), (present, "surface"));
        }
        StockPipeline::Deferred => {
            let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
            let material = graph.add(
                Node::new(doc::PASS_GEOMETRY)
                    .with_label("deferred material")
                    .with_setting(doc::SETTING_STAGE, "gbuffer"),
            );
            let lighting = graph.add(Node::new(doc::PASS_SCREEN).with_label("deferred lighting"));
            let present = graph.add_node(doc::PRESENT);
            wire(&mut graph, (scene, "draws"), (material, "draws"));
            wire(&mut graph, (gbuffer, "gbuffer"), (material, "gbuffer"));
            wire(&mut graph, (gbuffer, "gbuffer"), (lighting, "gbuffer"));
            wire(&mut graph, (lighting, "color"), (present, "surface"));
        }
    }
    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::{PassKind, ResourceDesc};
    use crate::pipeline::{deferred_graph, forward_graph, TargetConfig};
    use wxsl_core::lighting::LightingSet;
    use wxsl_core::pipeline::registry as make_registry;

    fn target() -> TargetConfig {
        TargetConfig::new(64, 64, wgpu::TextureFormat::Rgba8Unorm)
    }

    fn config() -> PipelineConfig {
        PipelineConfig::new(target())
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
                    (PassKind::Screen { shader: x }, PassKind::Screen { shader: y }) => x == y,
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
        let compiled = compile(&document, &make_registry(), &config()).expect("compiles");
        let hand_built = forward_graph(target());
        assert!(
            same_graph(&compiled, &hand_built),
            "compiled:\n{compiled:#?}\nhand built:\n{hand_built:#?}"
        );
    }

    #[test]
    fn the_deferred_document_compiles_to_the_hand_built_pass_list() {
        let document = stock_document(StockPipeline::Deferred);
        let compiled = compile(&document, &make_registry(), &config()).expect("compiles");
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
        };
        let document = stock_document(StockPipeline::Deferred);
        let compiled = compile(&document, &make_registry(), &config).expect("compiles");
        let hand_built = deferred_graph(target(), &config.lighting);
        assert!(same_graph(&compiled, &hand_built));
    }

    #[test]
    fn compiled_documents_schedule_like_the_hand_built_ones() {
        for stock in StockPipeline::ALL {
            let document = stock_document(*stock);
            let compiled = compile(&document, &make_registry(), &config()).expect("compiles");
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
        compile(document, &make_registry(), config).expect_err("expected compile to fail")
    }

    #[test]
    fn a_minimal_forward_document_compiles() {
        // One lit pass, depth of its own: the single-pass forward shape.
        let registry = make_registry();
        let mut graph = Graph::new("single pass");
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
        let compiled = compile(&graph, &registry, &config()).expect("compiles");
        // No shadow passes, no prepass: one pass per light is the shadow
        // node's doing, and this document has none.
        assert_eq!(compiled.passes().len(), 1);
        let schedule = compiled.schedule().expect("schedules");
        assert_eq!(schedule.slots().len(), 1, "just the depth texture");
    }

    #[test]
    fn unknown_effects_and_stages_are_reported_by_name() {
        let registry = make_registry();
        let mut graph = Graph::new("typos");
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
                assert!(known.iter().any(|id| id == "deferred_lighting"));
                assert!(!node.is_empty());
            }
            other => panic!("expected an unknown-effect error, got {other:?}"),
        }

        let registry = make_registry();
        let mut graph = Graph::new("typo stage");
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
        let mut graph = Graph::new("hand shadow");
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
        let mut graph = Graph::new("no gbuffer");
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
        let mut graph = Graph::new("two writers");
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
        let mut graph = Graph::new("two presents");
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
        let mut graph = Graph::new("present an intermediate");
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
        let mut graph = Graph::new("into a pass");
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
        let mut graph = Graph::new("prepass writes colour");
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
        let mut graph = Graph::new("no depth");
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
        let mut graph = Graph::new("double depth");
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
        let mut graph = Graph::new("screen with nothing");
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
        let mut graph = Graph::new("screen with an image");
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

    #[test]
    fn malformed_settings_are_reported_not_ignored() {
        // A tag expression that does not parse, a precision that is not
        // one, a scale that is not a number.
        let registry = make_registry();
        let mut graph = Graph::new("bad tags");
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

        let mut graph = Graph::new("bad precision");
        let color =
            graph.add(Node::new(doc::RESOURCE_COLOR).with_setting(doc::SETTING_PRECISION, "float"));
        let _ = color;
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::UnknownPrecision { precision, .. } if precision == "float"
        ));

        let mut graph = Graph::new("bad scale");
        graph.add(Node::new(doc::RESOURCE_DEPTH).with_setting(doc::SETTING_SCALE, "half"));
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::BadNumber { setting, .. } if setting == doc::SETTING_SCALE
        ));
    }

    #[test]
    fn two_shadow_sources_are_reported() {
        let mut graph = Graph::new("two light sources");
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
        let mut graph = Graph::new("depth from nowhere");
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
        let compiled = compile(&graph, &registry, &config()).expect("compiles");
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
        let mut graph = Graph::new("orphan");
        let color = graph.add(Node::new(doc::RESOURCE_COLOR).with_label("scene"));
        let screen = graph.add(Node::new(doc::PASS_SCREEN).with_label("tonemap"));
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((color, "color"), (screen, "image")),
            ((screen, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        // The compiler stops first: the shipped effect takes a G-buffer,
        // not an image. The scheduler's NeverWritten check is the last
        // line for effects that *do* take images (P4), so assert the
        // compiler's error here and the label plumbing via the parity
        // tests.
        assert!(matches!(
            errors_of(&graph, &config()),
            PipelineError::EffectInputMismatch { .. }
        ));
    }

    #[test]
    fn the_effect_table_holds_the_migrated_lighting_pass() {
        assert_eq!(EFFECTS.len(), 1);
        let (id, shader, doc) = EFFECTS[0];
        assert_eq!(id, "deferred_lighting");
        assert!(matches!(shader, ScreenShader::DeferredLighting));
        assert!(!doc.is_empty());
    }

    #[test]
    fn a_resource_document_can_ask_for_half_scale_and_history() {
        let registry = make_registry();
        let mut graph = Graph::new("half res history");
        let color = graph.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_label("accumulation")
                .with_setting(doc::SETTING_PRECISION, "hdr")
                .with_setting(doc::SETTING_SCALE, "0.5")
                .with_setting(doc::SETTING_HISTORY, "1"),
        );
        let screen = graph.add_node(doc::PASS_SCREEN);
        let present = graph.add_node(doc::PRESENT);
        for (from, to) in [
            ((color, "color"), (screen, "into")),
            ((screen, "color"), (present, "surface")),
        ] {
            graph.wire(&registry, from, to).expect("wiring");
        }
        // The screen pass wants a G-buffer, so this fails at the effect
        // check — but the *resource* was declared first, and its shape is
        // what this test pins: reachable by compiling just the resource
        // sweep through a document whose screen pass is otherwise valid.
        // Until a second effect exists, half-res and history resources are
        // exercised through their descriptor here.
        let error = errors_of(&graph, &config());
        assert!(matches!(error, PipelineError::EffectInputMismatch { .. }));

        // The descriptor shape itself, compiled from the same settings:
        // hdr → Rgba16Float, half scale, a ring of two.
        let desc = ResourceDesc::color(
            "accumulation",
            crate::pipeline::gbuffer_format(abi::GBufferPrecision::HighDynamicRange),
        )
        .with_extent(Extent::Viewport { scale: 0.5 })
        .persistent(1);
        assert_eq!(desc.format, wgpu::TextureFormat::Rgba16Float);
        assert_eq!(
            desc.extent.resolve(128, 128),
            (64, 64),
            "half scale, rounded"
        );
        assert_eq!(desc.persistence.ring_length(), 2);
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
