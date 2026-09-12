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
//! | `resource.color` | a colour target: precision, scale, history | one `ResourceDesc` |
//! | `resource.depth` | a depth target | one `ResourceDesc` |
//! | `pass.geometry` | a material stage over a draw list | one `PassDesc::geometry` |
//! | `pass.shadow` | the shadow passes over a draw list | one `PassDesc` per light slot |
//! | `pass.screen` | a fullscreen effect | one `PassDesc::screen` |
//! | `present` | what reaches the frame's target | the imported target |
//!
//! The fixed shape of the frame is deliberately *not* a knob here: the
//! shadow array is `abi::MAX_LIGHTS` layers at `abi::SHADOW_MAP_RESOLUTION`
//! because the frame group binds it by that shape, and the G-buffer is
//! whatever the enabled lighting set requests because the generated
//! lighting pass reads it by that shape. A document that wants a different
//! frame group is asking for a different ABI, not a different graph.

use crate::node::{NodeDefinition, NodeRegistry, SettingDef, Socket, ValueType};

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
/// Node id of `pass.geometry`.
pub const PASS_GEOMETRY: &str = "pass.geometry";
/// Node id of `pass.shadow`.
pub const PASS_SHADOW: &str = "pass.shadow";
/// Node id of `pass.screen`.
pub const PASS_SCREEN: &str = "pass.screen";
/// Node id of `present`, the document's terminal.
pub const PRESENT: &str = "present";

/// Every node id the shipped vocabulary defines, in registry order.
pub const NODE_IDS: &[&str] = &[
    SOURCE_SCENE,
    SOURCE_LIGHTS,
    RESOURCE_GBUFFER,
    RESOURCE_COLOR,
    RESOURCE_DEPTH,
    PASS_GEOMETRY,
    PASS_SHADOW,
    PASS_SCREEN,
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
                "standard (8-bit), hdr (half float), scalar (one channel) or pair (two).",
                "standard",
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
                "forward_lit, gbuffer or depth_only — the material stage the pass draws.",
                "forward_lit",
            ))
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
                 deferred lighting pass, a colour target for a post effect. With `into` \
                 unconnected it writes the frame's own target.",
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
                "deferred_lighting",
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
    use crate::graph::Graph;

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
