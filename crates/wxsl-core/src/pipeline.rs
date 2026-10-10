//! The pipeline document: a frame's passes, sources and resources as the
//! same kind of data a material graph is.
//!
//! A *pipeline* used to be the one thing in the repo that was still
//! hand-written Rust: `forward_graph` and `deferred_graph` built their pass
//! lists in code, and "add a pass" meant editing a function. This module is
//! the alternative — a pipeline is a [`Graph`] (the same type the material
//! canvas edits) over a *different node registry*, this one. Edges carry
//! render-graph resources ([`ValueType::ColorTarget`] and friends, the same
//! handle trick the texture sockets use), settings carry the knobs (a tag
//! expression, a material stage, a precision), and a compiler — in
//! `wxsl-render`, where `RenderGraph` lives — turns the document into a
//! schedulable pass list
//! ([ADR 0033](../../../docs/adr/0033-pipelines-are-documents.md)).
//!
//! The split of responsibilities is the scene's, again: this crate owns the
//! *vocabulary* and the typing; the compiler owns what it all *means*,
//! because its answer names `wgpu` formats and pass states. What lands here
//! is what a canvas has to be able to draw and a file has to be able to
//! hold.
//!
//! # The nodes
//!
//! | Node | Carries | Compiles to |
//! |---|---|---|
//! | `source.scene` | a draw list, filtered by tags | the frame's draws |
//! | `source.lights` | the shadow-map array | its `ResourceDesc` |
//! | `resource.gbuffer` | the enabled set's G-buffer, depth included | one resource per target, plus depth |
//! | `resource.color` | a colour target: precision, size, history | one `ResourceDesc` |
//! | `resource.depth` | a depth target | one `ResourceDesc` |
//! | `resource.buffer` | a storage buffer: size, history | one `ResourceDesc` |
//! | `pass.geometry` | a material stage over a draw list | one `PassDesc::geometry` |
//! | `pass.shadow` | the shadow passes over a draw list | one `PassDesc` per light slot |
//! | `pass.screen` | a fullscreen effect | one `PassDesc::screen` |
//! | `pass.compute.<effect>` | a compute effect, sockets from its declaration | one `PassDesc::compute` |
//! | `present` | what reaches the frame's target | the imported target |
//!
//! The fixed shape of the frame is deliberately *not* a knob here: the
//! shadow array is `abi::MAX_LIGHTS` layers at `abi::SHADOW_MAP_RESOLUTION`
//! because the frame group binds it by that shape, and the G-buffer is
//! whatever the enabled lighting set requests because the generated
//! lighting pass reads it by that shape. A document that wants a different
//! frame group is asking for a different ABI, not a different graph.

use crate::graph::Graph;
use crate::node::{GraphDomain, NodeDefinition, NodeRegistry, SettingDef, Socket, ValueType};

/// Setting on the pass nodes that takes a tag expression: what a pass
/// draws, as a [`crate::scene::TagExpr`].
pub const SETTING_TAGS: &str = "tags";
/// Setting on `pass.geometry`: which [`abi::MaterialStage`] the pass draws
/// its materials with.
pub const SETTING_STAGE: &str = "stage";
/// Setting on `pass.screen`: which screen effect the pass runs.
pub const SETTING_EFFECT: &str = "effect";
/// Setting on `resource.color`: the target's [`GBufferPrecision`].
pub const SETTING_PRECISION: &str = "precision";
/// Setting on `resource.color` and `resource.depth`: the target's size, as
/// a fraction of the frame target.
pub const SETTING_SCALE: &str = "scale";
/// Setting on `resource.color`: how many previous frames stay readable —
/// `0` is transient, `1` a classic ping-pong.
pub const SETTING_HISTORY: &str = "history";
/// Setting on `resource.buffer`: the buffer's size, in bytes.
pub const SETTING_BYTES: &str = "bytes";
/// Setting on `resource.color`: `viewport` (the default — sized by
/// [`SETTING_SCALE`]) or a fixed `64x64` in pixels, for the things whose
/// size is a fact of their contents, a LUT most of all.
pub const SETTING_SIZE: &str = "size";
/// Setting on `resource.color`: `true` declares the target as *imported* —
/// a texture the host owns (a bake table above all: the scene's materials
/// created it and sample it) that the pass list only writes through. An
/// imported target is sized by whoever created it, so the size settings
/// have nothing to say.
pub const SETTING_IMPORTED: &str = "imported";
/// Setting on the pass nodes: how often the pass runs — `per frame` (the
/// default), `once`, `on resize` or `on demand` (plan2 P10). A pass of
/// any non-default policy must write only targets that keep no history
/// but do survive frames, which the engine's scheduler checks.
pub const SETTING_POLICY: &str = "policy";
/// The `sort` setting of a `pass.geometry` node: how the pass orders its
/// draws, on top of each draw's own render order (plan5 D3/D4).
pub const SETTING_SORT: &str = "sort";
/// The `layers` setting of a `pass.geometry` node: which of its source's
/// draws the pass takes, by the material's `max_layers` (plan5 D5).
/// `all` is every draw; `sorted` is the tier no peel pass reads. The
/// peeled tier is `pass.peel`'s own expansion, never hand-authored.
pub const SETTING_LAYERS: &str = "layers";
/// The `blend` setting of a `pass.geometry` node: how the pass's fragments
/// land in its colour target. `opaque` — the default, no blending — or
/// `alpha over`, the straight-alpha over a sorted transparent pass needs.
pub const SETTING_BLEND: &str = "blend";

/// Node id of `source.scene`.
pub const SOURCE_SCENE: &str = "source.scene";
/// Node id of `source.lights`.
pub const SOURCE_LIGHTS: &str = "source.lights";
/// Node id of `resource.gbuffer`.
pub const RESOURCE_GBUFFER: &str = "resource.gbuffer";
/// Node id of `resource.color`.
pub const RESOURCE_COLOR: &str = "resource.color";
/// Node id of `resource.depth`.
pub const RESOURCE_DEPTH: &str = "resource.depth";
/// Node id of `resource.buffer`.
pub const RESOURCE_BUFFER: &str = "resource.buffer";
/// Node id of `pass.geometry`.
pub const PASS_GEOMETRY: &str = "pass.geometry";
/// Node id of `pass.peel`.
pub const PASS_PEEL: &str = "pass.peel";
/// Node id of `pass.shadow`.
pub const PASS_SHADOW: &str = "pass.shadow";
/// Node id of `pass.screen`.
pub const PASS_SCREEN: &str = "pass.screen";
/// Expand a filtered environment bake and declare the frame's lighting cubes.
pub const PASS_ENVIRONMENT: &str = "pass.environment";
/// Expand a bloom pyramid: one HDR resource per level, a thresholding
/// extract, a box down per deeper level, a fold per shallower one, and
/// the combine that adds the assembled glow back onto the image. The
/// passes the expansion generates — and their stable labels, which is
/// what `set_pass_param` tunes through — are the compiler's business
/// (ADR 0065); the document says *that* there is a pyramid and how many
/// levels, not *which* passes.
pub const PASS_BLOOM: &str = "pass.bloom";
/// Prefix of the `pass.compute.<effect>` ids — one generated definition
/// per registered compute effect, whose sockets are the effect's declared
/// inputs and outputs. A compute effect's wiring cannot sit on a fixed
/// socket set (an effect that wants two writes wants two output sockets),
/// so the definition is derived from the declaration it compiles against.
/// [`crate::node::NodeRegistry`] holds the static vocabulary; the
/// document compiler's side supplies the generated rows beside it.
pub const PASS_COMPUTE_PREFIX: &str = "pass.compute.";
/// Prefix of the `pass.screen.<effect>` ids — the screen pass's own
/// derived rows, for the same reason the compute ones exist: an effect
/// with two image inputs (TAA's colour, velocity and history; a blur's
/// colour and velocity) has no honest home on the fixed `pass.screen`
/// socket set, whose one `image` socket cannot name which wire is which.
/// The declaration is the sockets, exactly as it is for compute.
pub const PASS_SCREEN_PREFIX: &str = "pass.screen.";
/// Node id of `present`, the document's terminal — named into the
/// `output` package like the ABI's own outputs, every registry id carrying
/// a package segment ([`crate::identity`], ADR 0044).
pub const PRESENT: &str = "output.present";

/// Every node id the shipped vocabulary defines, in registry order. The
/// generated `pass.compute.<effect>` rows are not listed: they exist per
/// registered effect, beside this table, not in it.
pub const NODE_IDS: &[&str] = &[
    SOURCE_SCENE,
    SOURCE_LIGHTS,
    RESOURCE_GBUFFER,
    RESOURCE_COLOR,
    RESOURCE_DEPTH,
    RESOURCE_BUFFER,
    PASS_GEOMETRY,
    PASS_PEEL,
    PASS_SHADOW,
    PASS_SCREEN,
    PASS_ENVIRONMENT,
    PASS_BLOOM,
    PRESENT,
];

/// A `tags`-style setting: any text is a document, and the compiler parses
/// it where it is used, because a tag expression is *not* an identifier and
/// the graph model's setting validation would be wrong to demand one.
fn text_setting(name: &str, label: &str, doc: &str, default: &str) -> SettingDef {
    SettingDef::new(name, label, doc).with_default(default)
}

/// The pipeline document's node vocabulary: one reviewable table, the same
/// shape as [`abi::context_node_defs`].
///
/// An application extends a pipeline canvas by registering more
/// `NodeBody::Document` definitions beside these — the same extension story
/// as the material registry, and the reason the ids are plain strings
/// rather than an enum.
pub fn node_defs() -> Vec<NodeDefinition> {
    vec![
        NodeDefinition::builder(PASS_ENVIRONMENT, "environment bake")
            .doc("Equirectangular linear radiance to filtered lighting cubes; one environment per frame.")
            .input(Socket::new("image", ValueType::ColorTarget))
            .output(Socket::new("radiance", ValueType::ColorTarget))
            .setting(text_setting(SETTING_SIZE, "Cube size", "Square cube face side in pixels.", "128"))
            .setting(text_setting("diffuse_size", "Diffuse size", "Diffuse cube face side in pixels.", "16"))
            .setting(text_setting("mips", "Specular mips", "GGX roughness levels, including zero.", "8"))
            .setting(text_setting("radiance_scale", "Radiance scale", "Positive restoration factor for scaled HDR uploads.", "1"))
            .document(),
        NodeDefinition::builder(PASS_BLOOM, "bloom pyramid")
            .doc(
                "A bloom pyramid over one HDR image: a thresholding extract into a \
                 half-resolution first level, a 2x box down per deeper level, a fold \
                 per shallower one, and the combine that adds the assembled glow back \
                 onto the image. Expands to the pyramid's level resources and passes \
                 (ADR 0065) with stable labels — `<label> extract`, `<label> down 1`, \
                 `<label> up 0`, `<label> combine` — which is what `set_pass_param` \
                 tunes through: `threshold` and `knee` on the extract, `strength` on \
                 the combine. The glow resources are HDR transients; thresholding is \
                 linear radiance, so an 8-bit intermediate would clamp the highlight \
                 the pyramid is for.",
            )
            .input(
                Socket::new("image", ValueType::ColorTarget)
                    .with_doc("The image to glow from, as linear radiance."),
            )
            .input(
                Socket::new("into", ValueType::ColorTarget)
                    .optional()
                    .with_doc("Where to write. Unconnected means the frame's own target."),
            )
            .output(
                Socket::new("color", ValueType::ColorTarget)
                    .with_doc("What the combine wrote — the frame's target when `into` is unconnected."),
            )
            .setting(text_setting(
                "levels",
                "Levels",
                "How many pyramid levels, including the first: each is half the one \
                 before, starting at half resolution.",
                "4",
            ))
            .setting(text_setting(
                "threshold",
                "Threshold",
                "Linear radiance where the glow begins; the knee eases it in.",
                "1",
            ))
            .setting(text_setting("knee", "Knee", "Width of the soft threshold knee.", "0.6"))
            .setting(text_setting(
                "strength",
                "Strength",
                "How much of the assembled glow is added back.",
                "0.85",
            ))
            .document(),
        // -- sources -----------------------------------------------------
        NodeDefinition::builder(SOURCE_SCENE, "Scene")
            .doc("The frame's draw list, filtered by a tag expression.")
            .output(
                Socket::new("draws", ValueType::DrawQueue)
                    .with_doc("Everything the scene offers, filtered by `tags`."),
            )
            .setting(text_setting(
                SETTING_TAGS,
                "Tags",
                "Which instances this queue carries, as a tag expression: `*`, `opaque && !outlined`.",
                "*",
            ))
            .document(),
        NodeDefinition::builder(SOURCE_LIGHTS, "shadow maps")
            .doc(
                "The frame's shadow-map array: one slice per light slot, at the ABI's \
                 fixed resolution. Its shape is the frame group's, not a setting — the \
                 lighting pass reads it by that shape.",
            )
            .output(
                Socket::new("shadows", ValueType::ShadowMaps)
                    .with_doc("The array the shadow passes fill, one layer per light."),
            )
            .document(),
        // -- resources ---------------------------------------------------
        NodeDefinition::builder(RESOURCE_GBUFFER, "G-buffer")
            .doc(
                "The G-buffer the enabled lighting set requests: one target per request, \
                 plus the depth the lighting pass reconstructs position from. The set is \
                 a knob of the pipeline's configuration, not of the document — widening \
                 it rewires nothing.",
            )
            .output(
                Socket::new("gbuffer", ValueType::GBuffer)
                    .with_doc("Every target, then depth — the order a screen pass reads them in."),
            )
            .document(),
        NodeDefinition::builder(RESOURCE_COLOR, "colour target")
            .doc("A colour target an effect chain writes and reads.")
            .output(
                Socket::new("color", ValueType::ColorTarget)
                    .with_doc("The target, sized from `scale`."),
            )
            .setting(text_setting(
                SETTING_PRECISION,
                "Precision",
                "standard (8-bit), hdr (half float), float (linear rgba32float import), scalar or pair.",
                "standard",
            ))
            .setting(text_setting(
                SETTING_SIZE,
                "Size",
                "`viewport` (sized by `scale`, the default) or fixed `64x64` pixels — \
                 for the things whose size is a fact of their contents, a LUT most of all.",
                "viewport",
            ))
            .setting(text_setting(
                SETTING_SCALE,
                "Scale",
                "Fraction of the frame target's size: 1.0 full, 0.5 half.",
                "1",
            ))
            .setting(text_setting(
                SETTING_HISTORY,
                "History",
                "Previous frames kept readable: 0 transient, 1 ping-pong, n a ring of n+1.",
                "0",
            ))
            .setting(text_setting(
                SETTING_IMPORTED,
                "Imported",
                "`true` if the host owns the texture — a bake table the scene's \
                 materials created and sample. Its label must match the bake's, \
                 and the size settings say nothing.",
                "false",
            ))
            .document(),
        NodeDefinition::builder(RESOURCE_DEPTH, "depth target")
            .doc(
                "A depth target. The first pass to draw into it clears it; a pass taking \
                 another pass's depth output tests against what is already there.",
            )
            .output(
                Socket::new("depth", ValueType::DepthTarget)
                    .with_doc("The target, sized from `scale`."),
            )
            .setting(text_setting(
                SETTING_SCALE,
                "Scale",
                "Fraction of the frame target's size: 1.0 full, 0.5 half.",
                "1",
            ))
            .document(),
        NodeDefinition::builder(RESOURCE_BUFFER, "storage buffer")
            .doc(
                "A storage buffer: the thing a compute pass writes and any pass's \
                 shader reads back as `var<storage>`. Its slot is never shared — \
                 a buffer aliasing bug corrupts a whole block, not a frame region.",
            )
            .output(
                Socket::new("buffer", ValueType::StorageBuffer)
                    .with_doc("The buffer, of `bytes` bytes."),
            )
            .setting(text_setting(
                SETTING_BYTES,
                "Bytes",
                "The buffer's size, in bytes. A read-back buffer wants a multiple of 4.",
                "1024",
            ))
            .setting(text_setting(
                SETTING_HISTORY,
                "History",
                "Previous frames kept readable: 0 transient, 1 ping-pong, n a ring of n+1.",
                "0",
            ))
            .document(),
        // -- passes ------------------------------------------------------
        NodeDefinition::builder(PASS_GEOMETRY, "material pass")
            .doc(
                "Draws a material stage over a draw list. Wired to a G-buffer it writes \
                 that set's targets; wired to depth it clears it if the depth comes \
                 straight from a `resource.depth` node and tests against it (without \
                 writing) if it comes from another pass's depth output. Its colour \
                 output feeds `present`, or another pass in a chain.",
            )
            .input(
                Socket::new("draws", ValueType::DrawQueue).with_doc("What to draw."),
            )
            .input(
                Socket::new("gbuffer", ValueType::GBuffer)
                    .optional()
                    .with_doc("For a `gbuffer`-stage pass: the G-buffer to write."),
            )
            .input(
                Socket::new("depth", ValueType::DepthTarget)
                    .optional()
                    .with_doc("The depth to clear or test against."),
            )
            .input(
                Socket::new("into", ValueType::ColorTarget)
                    .optional()
                    .with_doc(
                        "Where a colour-writing stage writes. Unconnected means the \
                         frame's own target; wired to a `resource.color`, the pass \
                         starts an effect chain instead of ending one.",
                    ),
            )
            .output(
                Socket::new("color", ValueType::ColorTarget)
                    .with_doc("What the pass wrote — unconnected for a stage that writes no colour."),
            )
            .output(
                Socket::new("depth", ValueType::DepthTarget)
                    .with_doc("The depth as this pass left it, for the next pass to test against."),
            )
            .setting(text_setting(
                SETTING_STAGE,
                "Stage",
                "forward_lit, gbuffer, depth_only or velocity — the material \
                 stage the pass draws.",
                "forward_lit",
            ))
            .setting(text_setting(
                SETTING_POLICY,
                "Policy",
                "How often the pass runs: per frame, once, on resize or on demand. Only a \
                 pass whose target survives frames may skip.",
                "per frame",
            ))
            .setting(text_setting(
                SETTING_SORT,
                "Sort",
                "How the pass orders its draws: none (submission order), front to back, \
                 or back to front. Sorting reorders the draws inside this pass only.",
                "none",
            ))
            .setting(text_setting(
                SETTING_LAYERS,
                "Layers",
                "Which draws the pass takes, by each material's max layers: all \
                 of them, or only the sorted tier (max layers zero) that no \
                 peel pass reads. Peeling itself is a `pass.peel` node.",
                "all",
            ))
            .setting(text_setting(
                SETTING_BLEND,
                "Blend",
                "How fragments land in the target: opaque (no blending) or alpha \
                 over (straight-alpha source over the target), which a sorted \
                 transparent pass composites with.",
                "opaque",
            ))
            .document(),
        NodeDefinition::builder(PASS_PEEL, "depth peel")
            .doc(
                "Order-independent transparency by dual depth peeling. Draws only \
                 instances tagged `transparent`, however the scene source that feeds \
                 other passes is filtered, and composites them over the opaque colour \
                 wired to `scene`. The layer count is the `wxsl_peel_layers` macro on \
                 the pipeline config (1 to 8, default 4), and `wxsl_peel_native` selects \
                 the float-blend path (ADR 0047).",
            )
            .input(
                Socket::new("depth", ValueType::DepthTarget).with_doc(
                    "The opaque pass's depth. Fragments behind it are not peeled.",
                ),
            )
            .input(
                Socket::new("scene", ValueType::ColorTarget).with_doc(
                    "The opaque colour, a `resource.color` the peel can sample. \
                     The frame's own target cannot be both sampled and presented.",
                ),
            )
            .input(
                Socket::new("into", ValueType::ColorTarget)
                    .optional()
                    .with_doc(
                        "Where the composite is written. Unconnected means the frame's \
                         own target.",
                    ),
            )
            .output(
                Socket::new("color", ValueType::ColorTarget)
                    .with_doc("The opaque colour with the transparent layers composited over it."),
            )
            .document(),
        NodeDefinition::builder(PASS_SHADOW, "shadow")
            .doc(
                "One shadow pass per light slot, filling one layer each of the shadow-map \
                 array. A slot whose light casts nothing this frame is cleared and left \
                 alone — the pass list is built once and scheduled against every frame.",
            )
            .input(
                Socket::new("draws", ValueType::DrawQueue).with_doc("What to draw."),
            )
            .input(
                Socket::new("into", ValueType::ShadowMaps).with_doc("The shadow maps to fill."),
            )
            .document(),
        NodeDefinition::builder(PASS_SCREEN, "screen effect")
            .doc(
                "A fullscreen pass over what earlier passes wrote. The effect is named by \
                 `effect`; what it reads is what is wired into it — a G-buffer for the \
                 deferred lighting pass, a colour target for a post effect, a storage \
                 buffer for an effect drawing what compute left. With `into` unconnected \
                 it writes the frame's own target.",
            )
            .input(
                Socket::new("gbuffer", ValueType::GBuffer)
                    .optional()
                    .with_doc("The G-buffer, for an effect that shades one."),
            )
            .input(
                Socket::new("image", ValueType::ColorTarget)
                    .optional()
                    .with_doc("A colour input, for a post effect."),
            )
            .input(
                Socket::new("buffer", ValueType::StorageBuffer)
                    .optional()
                    .with_doc(
                        "A storage buffer, for an effect that reads one as `var<storage>` — \
                         the buffer half of a compute pass's work arriving at the screen.",
                    ),
            )
            .input(
                Socket::new("into", ValueType::ColorTarget)
                    .optional()
                    .with_doc("Where to write. Unconnected means the frame's own target."),
            )
            .output(
                Socket::new("color", ValueType::ColorTarget)
                    .with_doc("What the effect wrote — the frame's target when `into` is unconnected."),
            )
            .setting(text_setting(
                SETTING_EFFECT,
                "Effect",
                "Which screen effect the pass runs.",
                "wxsl.deferred_lighting",
            ))
            .setting(text_setting(
                SETTING_POLICY,
                "Policy",
                "How often the pass runs: per frame, once, on resize or on demand. Only a \
                 pass whose target survives frames may skip.",
                "per frame",
            ))
            .document(),
        // -- terminal ----------------------------------------------------
        NodeDefinition::builder(PRESENT, "Present")
            .doc("What reaches the frame's own target. Exactly one per document.")
            .input(
                Socket::new("surface", ValueType::ColorTarget).with_doc("What to present."),
            )
            .document(),
    ]
}

/// An empty pipeline document: a graph in [`GraphDomain::Document`].
///
/// The one way to start one, so that "a document" is a fact about the graph
/// rather than about which registry someone happened to validate it against
/// (ADR 0040). A `Graph::new` would be a *material* holding pass nodes, and
/// `Graph::validate` says exactly that.
pub fn document(name: impl Into<String>) -> Graph {
    Graph::in_domain(name, GraphDomain::Document)
}

/// The pipeline document's node registry: every definition from
/// [`node_defs`].
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register_all(node_defs());
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vocabulary_registers_without_collisions() {
        let registry = registry();
        assert_eq!(registry.len(), NODE_IDS.len());
        for id in NODE_IDS {
            let def = registry
                .get(id)
                .unwrap_or_else(|| panic!("{id} is listed but not registered"));
            // Every document node is a Document body — that is what makes
            // it a pipeline node rather than a shader node that happens to
            // sit in this registry.
            assert!(def.is_document(), "{id} is not a document node");
            assert_eq!(def.category, id.split('.').next().unwrap_or(id));
        }
    }

    #[test]
    fn handle_types_never_reach_a_shader_socket() {
        // The pipeline handle types are outside `ALL` and outside
        // `RESOURCES`: nothing a material graph can touch, and nothing the
        // node library's sockets can accept.
        for ty in ValueType::PIPELINE_RESOURCES {
            assert!(!ValueType::ALL.contains(ty), "{ty} leaked into ALL");
            assert!(
                !ValueType::RESOURCES.contains(ty),
                "{ty} leaked into RESOURCES"
            );
            assert!(
                ty.is_resource(),
                "{ty} must behave as a resource when typed"
            );
            assert!(ty.zero().is_none(), "{ty} has no zero");
            assert!(ty.splat(1.0).is_none(), "{ty} has no splat");
        }
    }

    #[test]
    fn a_pipeline_document_validates_as_a_graph() {
        // The typing, acyclicity and required-input machinery come free
        // with the graph model — this document is wrong only in that
        // nothing is wired, which the required inputs catch.
        let registry = registry();
        let mut graph = Graph::new("empty");
        graph.add_node(SOURCE_SCENE);
        graph.add_node(PRESENT);
        let errors = graph
            .validate(&registry)
            .expect_err("an unfed mandatory input is a graph error");
        // `present.surface` is mandatory and nothing feeds it.
        assert!(
            errors
                .0
                .iter()
                .any(|error| matches!(error, crate::GraphError::MissingInput { .. })),
            "expected the unfed `surface` input to be reported: {errors}"
        );
    }

    #[test]
    fn handle_types_do_not_combine_as_values() {
        // Arithmetic on a draw queue is meaningless, and the value-typing
        // rules must say so rather than treating the handle as an operand.
        assert_eq!(
            ValueType::DrawQueue.componentwise(ValueType::DrawQueue),
            None
        );
        assert_eq!(ValueType::ColorTarget.product(ValueType::ColorTarget), None);
        assert_eq!(ValueType::DepthTarget.componentwise(ValueType::F32), None);
    }
}
