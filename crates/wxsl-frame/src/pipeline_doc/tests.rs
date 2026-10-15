#[test]
fn environment_settings_and_duplicate_declarations_are_named() {
    use super::*;
    let effects = EffectRegistry::shipped();
    let registry = document_registry(&effects);
    let mut graph = StockPipeline::Forward.document();
    let hdr = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("test HDR")
            .with_setting(doc::SETTING_IMPORTED, "true")
            .with_setting(doc::SETTING_PRECISION, "float"),
    );
    let bake = graph.add(Node::new(doc::PASS_ENVIRONMENT).with_label("test environment"));
    graph
        .wire(&registry, (hdr, "color"), (bake, "image"))
        .unwrap();
    let cfg = config();
    let plan = compile(&graph, &registry, &effects, &cfg).unwrap();
    plan.schedule().unwrap();
    for (setting, value) in [
        (doc::SETTING_SIZE, "0"),
        ("mips", "2.5"),
        ("radiance_scale", "NaN"),
    ] {
        let mut invalid = graph.clone();
        invalid.set_setting(bake, setting, value);
        let error = compile(&invalid, &registry, &effects, &cfg)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("test environment") && error.contains(setting),
            "{error}"
        );
    }
    let duplicate = graph.add(Node::new(doc::PASS_ENVIRONMENT).with_label("duplicate environment"));
    graph
        .wire(&registry, (hdr, "color"), (duplicate, "image"))
        .unwrap();
    assert!(compile(&graph, &registry, &effects, &cfg)
        .unwrap_err()
        .to_string()
        .contains("duplicate environment"));
}
use super::*;
use crate::effect::EffectRegistry;
use crate::graph::RenderGraph;
use crate::pass::{Dimension, PassKind};
use crate::pipeline::{deferred_graph, forward_graph, PipelineConfig, TargetConfig};
use wxsl_core::abi::{self, MaterialStage};
use wxsl_core::lighting::LightingSet;
use wxsl_core::pipeline::registry as make_registry;
use wxsl_core::scene::{self, TagExpr};

fn target() -> TargetConfig {
    TargetConfig::new(64, 64, TextureFormat::Rgba8Unorm)
}

fn config() -> PipelineConfig {
    PipelineConfig::new(target())
}

/// The shipped effects, as every document below compiles against them.
fn effects() -> EffectRegistry {
    EffectRegistry::shipped()
}

/// Compare two compiled graphs field by field. `PassDesc` cannot
/// derive `PartialEq` (`DrawSource::Indirect` names a buffer resource
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
    let compiled = compile(&document, &make_registry(), &effects(), &config()).expect("compiles");
    let hand_built = forward_graph(target());
    assert!(
        same_graph(&compiled, &hand_built),
        "compiled:\n{compiled:#?}\nhand built:\n{hand_built:#?}"
    );
}

#[test]
fn the_deferred_document_compiles_to_the_hand_built_pass_list() {
    let document = stock_document(StockPipeline::Deferred);
    let compiled = compile(&document, &make_registry(), &effects(), &config()).expect("compiles");
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
        macros: wxsl_core::macros::MacroSet::new(),
        profile: Default::default(),
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
        macros: wxsl_core::macros::MacroSet::new(),
        profile: Default::default(),
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
        macros: wxsl_core::macros::MacroSet::new(),
        profile: Default::default(),
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
    compile(document, &make_registry(), &effects(), config).expect_err("expected compile to fail")
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
    let screen = graph.add(Node::new(doc::PASS_SCREEN).with_setting(doc::SETTING_EFFECT, "blur"));
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
    let pass = graph.add(Node::new(doc::PASS_GEOMETRY).with_setting(doc::SETTING_STAGE, "shadow"));
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
    let tonemap =
        graph.add(Node::new(doc::PASS_SCREEN).with_setting(doc::SETTING_EFFECT, "wxsl.tonemap"));
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
    let material =
        graph.add(Node::new(doc::PASS_GEOMETRY).with_setting(doc::SETTING_STAGE, "forward_lit"));
    let color =
        graph.add(Node::new(doc::RESOURCE_COLOR).with_setting(doc::SETTING_PRECISION, "hdr"));
    let velocity =
        graph.add(Node::new(doc::RESOURCE_COLOR).with_setting(doc::SETTING_PRECISION, "standard"));
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
    let taa = graph.add(Node::new(doc::PASS_SCREEN).with_setting(doc::SETTING_EFFECT, "wxsl.taa"));
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
        graph.add(Node::new(doc::RESOURCE_COLOR).with_setting(doc::SETTING_PRECISION, "unknown"));
    let _ = color;
    assert!(matches!(
        errors_of(&graph, &config()),
        PipelineError::UnknownPrecision { precision, .. } if precision == "unknown"
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
        assert_eq!(compiled.passes().len(), abi::MAX_SHADOW_TILES + 3);
        assert_eq!(
            &schedule.order()[abi::MAX_SHADOW_TILES..],
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
        assert!(matches!(&bloom_pass.kind, PassKind::Screen { effect } if effect == "wxsl.bloom"));
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
    let bloom_y = graph
        .add(Node::new(format!("{}wxsl.bloom_y", doc::PASS_SCREEN_PREFIX)).with_label("bloom-y"));
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
    let compiled = compile(&graph, &registry, &effects(), &config()).expect("the chain compiles");
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
    assert!(matches!(&vertical_pass.kind, PassKind::Screen { effect } if effect == "wxsl.bloom_y"));
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
    assert_eq!(format, TextureFormat::Rgba16Float, "hdr");
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
        Node::new(format!("{}wxsl.ramp_fill", doc::PASS_COMPUTE_PREFIX)).with_label("fill ramp"),
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
    let fill = graph
        .add(Node::new(format!("{}wxsl.ramp_fill", doc::PASS_COMPUTE_PREFIX)).with_label("fill"));
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
            usage: BufferUsages::empty(),
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
fn a_sort_setting_reaches_the_pass_list() {
    // plan5 D3/D4: the `sort` setting compiles into the pass, the
    // stock preset — which never mentions it — stays at submission
    // order, and a name that is not one is refused by name.
    let effects = EffectRegistry::shipped();
    let registry = document_registry(&effects);

    let mut graph = StockPipeline::Forward.document();
    let geometry = graph
        .nodes()
        .find(|(_, node)| node.def == doc::PASS_GEOMETRY)
        .map(|(id, _)| id)
        .expect("forward has a geometry pass");
    graph.set_setting(geometry, doc::SETTING_SORT, "back to front");
    let compiled = compile(&graph, &registry, &effects, &config()).expect("compiles");
    assert!(
        compiled
            .passes()
            .iter()
            .any(|pass| pass.sort == SortOrder::BackToFront),
        "the setting reached the pass list"
    );
    compiled.schedule().expect("schedules");

    // Without the setting, the stock behaviour: submission order.
    let compiled = compile(
        &StockPipeline::Forward.document(),
        &registry,
        &effects,
        &config(),
    )
    .expect("the stock document compiles");
    assert!(
        compiled
            .passes()
            .iter()
            .all(|pass| pass.sort == SortOrder::None),
        "no default sorting: submission order is the behaviour"
    );

    // A name that is not one.
    let mut graph = StockPipeline::Forward.document();
    let geometry = graph
        .nodes()
        .find(|(_, node)| node.def == doc::PASS_GEOMETRY)
        .map(|(id, _)| id)
        .expect("forward has a geometry pass");
    graph.set_setting(geometry, doc::SETTING_SORT, "by whimsy");
    match compile(&graph, &registry, &effects, &config()) {
        Err(PipelineError::UnknownSort { node, sort }) => {
            assert_eq!(sort, "by whimsy");
            assert!(!node.is_empty());
        }
        other => panic!("expected a sort error, got {other:?}"),
    }
}

#[test]
fn the_layers_and_blend_settings_reach_the_pass_list() {
    // plan5 D5: the `layers` and `blend` settings compile into the
    // pass, the stock preset — which never mentions either — stays at
    // every draw, unblended, and a name that is not one is refused by
    // name.
    let effects = EffectRegistry::shipped();
    let registry = document_registry(&effects);

    let mut graph = StockPipeline::Forward.document();
    let geometry = graph
        .nodes()
        .find(|(_, node)| node.def == doc::PASS_GEOMETRY)
        .map(|(id, _)| id)
        .expect("forward has a geometry pass");
    graph.set_setting(geometry, doc::SETTING_LAYERS, "sorted");
    graph.set_setting(geometry, doc::SETTING_BLEND, "alpha over");
    let compiled = compile(&graph, &registry, &effects, &config()).expect("compiles");
    let pass = compiled
        .passes()
        .iter()
        .find(|pass| pass.layers != LayerFilter::All)
        .expect("the setting reached the pass list");
    assert_eq!(pass.layers, LayerFilter::Sorted);
    assert!(
        pass.state.blend.is_some(),
        "the blend reached the pass list"
    );
    // A blended pass tests depth and does not write it: the composite
    // is the draws' order, not the nearest draw's.
    assert!(!pass.state.depth_write, "a blended pass leaves depth alone");
    compiled.schedule().expect("schedules");

    // Without the settings, the stock behaviour.
    let compiled = compile(
        &StockPipeline::Forward.document(),
        &registry,
        &effects,
        &config(),
    )
    .expect("the stock document compiles");
    assert!(compiled
        .passes()
        .iter()
        .all(|pass| pass.layers == LayerFilter::All && pass.state.blend.is_none()),);
    // The stock pass still clears its target: loading is the sorted
    // tier's behaviour, not every geometry pass's.
    let stock_geometry = compiled
        .passes()
        .iter()
        .find(|pass| matches!(pass.kind, crate::pass::PassKind::Geometry { .. }))
        .expect("a geometry pass");
    assert!(stock_geometry.color.iter().all(|color| {
        matches!(
            color.load,
            crate::pass::Load::Clear(crate::types::Color { .. })
        )
    }));

    for (setting, value) in [
        (doc::SETTING_LAYERS, "peeled"),
        (doc::SETTING_BLEND, "by whimsy"),
    ] {
        let mut graph = StockPipeline::Forward.document();
        let geometry = graph
            .nodes()
            .find(|(_, node)| node.def == doc::PASS_GEOMETRY)
            .map(|(id, _)| id)
            .expect("forward has a geometry pass");
        graph.set_setting(geometry, setting, value);
        let compiled = compile(&graph, &registry, &effects, &config());
        assert!(
            matches!(
                compiled,
                Err(PipelineError::UnknownLayers { .. } | PipelineError::UnknownBlend { .. })
            ),
            "{setting} = {value:?} should be refused by name"
        );
    }
}

/// The tier filter itself (plan5 D5): who a peel iteration and the
/// sorted pass take, by each draw's `max_layers`.
#[test]
fn the_layer_filter_admits_by_budget() {
    use crate::pass::LayerFilter;
    // The sorted tier: budget zero only.
    assert!(LayerFilter::Sorted.admits(0));
    assert!(!LayerFilter::Sorted.admits(1));
    // Peel iteration `i` takes a draw whose budget is still unspent
    // there — and a budget beyond the pipeline's cap is clamped by
    // the cap bounding which iterations are generated at all, never
    // an error.
    assert!(LayerFilter::Peeled { layer: 0 }.admits(1));
    assert!(!LayerFilter::Peeled { layer: 1 }.admits(1));
    assert!(LayerFilter::Peeled { layer: 3 }.admits(4));
    assert!(!LayerFilter::Peeled { layer: 4 }.admits(4));
    assert!(
        LayerFilter::Peeled { layer: 7 }.admits(9),
        "clamped, not refused"
    );
    assert!(
        LayerFilter::All.admits(-3),
        "everything passes a pass with no filter"
    );
}

#[test]
fn a_fixed_size_target_and_an_once_bake_make_the_lut_demo_a_document() {
    // The brdf-lut gallery demo, as a document: a 64x64 fixed HDR
    // target, a `once` compute bake writing it, a view presenting it.
    // Fixed size is the setting the hand-built list needed
    // `Extent::Fixed` for; `once` is the policy setting.
    let (graph, registry, effects) = lut_document();
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
            mip_levels: 1,
            format: TextureFormat::Rgba16Float,
            usage: TextureUsages::empty(),
        }
    );
    // `once` promoted the target to stable storage — the scheduler's
    // rule for a pass that skips frames, derived by the compiler.
    assert_eq!(desc.persistence, Persistence::Persistent { history: 0 });
    compiled.schedule().expect("schedules");
}

#[test]
fn a_compute_pass_lowers_to_its_fragment_arm_without_compute() {
    // ADR 0075: the same document, compiled for WebGL2. The bake keeps
    // its label, its policy and its target — now as a screen pass's
    // colour attachment — and the view is untouched.
    let (graph, registry, effects) = lut_document();
    let webgl2 = PipelineConfig {
        profile: crate::profile::Profile::WEBGL2,
        ..config()
    };
    let compiled = compile(&graph, &registry, &effects, &webgl2).expect("lowers");
    let bake = compiled
        .passes()
        .iter()
        .find(|pass| pass.label == "lut bake")
        .expect("the bake keeps its label");
    assert!(
        matches!(&bake.kind, PassKind::Screen { effect } if effect == "wxsl.brdf_lut"),
        "{:?}",
        bake.kind
    );
    assert_eq!(bake.policy, Policy::Once);
    let lut = compiled
        .resources()
        .iter()
        .position(|desc| desc.label == "brdf lut")
        .expect("the lut is declared");
    assert_eq!(bake.color.len(), 1);
    assert_eq!(bake.color[0].resource.index(), lut);
    assert!(!compiled
        .passes()
        .iter()
        .any(|pass| matches!(pass.kind, PassKind::Compute { .. })));
    compiled.schedule().expect("schedules");
}

#[test]
fn a_native_only_effect_is_refused_by_name_on_a_downlevel_profile() {
    // The ramp fills a storage buffer: there is nothing to lower it to.
    let effects = EffectRegistry::shipped()
        .with(crate::effect::RAMP_FILL)
        .with(crate::effect::RAMP_VIEW);
    let registry = document_registry(&effects);
    let mut graph = wxsl_core::pipeline::document("ramp");
    let buffer = graph.add(
        Node::new(doc::RESOURCE_BUFFER)
            .with_label("ramp")
            .with_setting(doc::SETTING_BYTES, "1024"),
    );
    let fill = graph
        .add(Node::new(format!("{}wxsl.ramp_fill", doc::PASS_COMPUTE_PREFIX)).with_label("fill"));
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
        graph.wire(&registry, from, to).expect("ramp wiring");
    }
    // Compiles for a full device: the refusal below is the profile's.
    compile(&graph, &registry, &effects, &config()).expect("compiles on a full device");
    let webgl2 = PipelineConfig {
        profile: crate::profile::Profile::WEBGL2,
        ..config()
    };
    match compile(&graph, &registry, &effects, &webgl2) {
        Err(error @ PipelineError::NativeOnly { .. }) => {
            let PipelineError::NativeOnly { pass, effect, .. } = &error else {
                unreachable!()
            };
            assert_eq!(pass, "fill");
            assert_eq!(effect, "wxsl.ramp_fill");
            assert!(error.to_string().contains("webgl2"), "{error}");
        }
        other => panic!("expected the ramp refused by name, got {other:?}"),
    }
}

/// The brdf-lut gallery demo as a document, with its registries.
fn lut_document() -> (
    wxsl_core::graph::Graph,
    wxsl_core::node::NodeRegistry,
    EffectRegistry,
) {
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
    (graph, registry, effects)
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
    let bake = graph
        .add(Node::new(format!("{}wxsl.brdf_lut", doc::PASS_COMPUTE_PREFIX)).with_label("bake"));
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

/// A peel document: opaque shade into a colour target, then `pass.peel`.
fn peel_document() -> (wxsl_core::graph::Graph, wxsl_core::node::NodeRegistry) {
    let registry = make_registry();
    let mut graph = wxsl_core::pipeline::document("peel");
    let scene = graph.add(
        wxsl_core::graph::Node::new(doc::SOURCE_SCENE).with_setting(doc::SETTING_TAGS, "opaque"),
    );
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let color = graph.add(
        wxsl_core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let shade = graph.add(wxsl_core::graph::Node::new(doc::PASS_GEOMETRY).with_label("opaque"));
    let peel = graph.add(wxsl_core::graph::Node::new(doc::PASS_PEEL).with_label("peel"));
    let present = graph.add_node(doc::PRESENT);
    for (from, to) in [
        ((scene, "draws"), (shade, "draws")),
        ((depth, "depth"), (shade, "depth")),
        ((color, "color"), (shade, "into")),
        ((shade, "depth"), (peel, "depth")),
        ((shade, "color"), (peel, "scene")),
        ((peel, "color"), (present, "surface")),
    ] {
        graph
            .wire(&registry, from, to)
            .expect("the peel document wires");
    }
    (graph, registry)
}

fn peel_config(layers: i32, native: bool) -> PipelineConfig {
    use wxsl_core::macros::MacroValue;
    let mut config = config();
    config
        .macros
        .set(abi::PEEL_LAYERS_MACRO, MacroValue::Int(layers));
    if native {
        config
            .macros
            .set(abi::PEEL_NATIVE_MACRO, MacroValue::Flag(true));
    }
    config
}

/// Geometry passes that actually draw transparent instances.
fn peel_draws(graph: &RenderGraph) -> Vec<MaterialStage> {
    graph
        .passes()
        .iter()
        .filter_map(|pass| match &pass.kind {
            PassKind::Geometry { source, stage } => match source {
                crate::pass::DrawSource::Scene(TagExpr::Tag(tag))
                    if tag == scene::TAG_TRANSPARENT =>
                {
                    Some(*stage)
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The geometry stages a peel of `layers` draws, after the clamp.
///
/// Baseline: depth then shade, two passes a layer, at most four layers
/// (eight passes). Native: a depth/resolve pair peels two layers; an
/// odd leftover is one front layer, still depth then shade.
fn expected_peel_stages(layers: i32, native: bool) -> Vec<MaterialStage> {
    let layers = layers.clamp(1, abi::PEEL_LAYERS_MAX) as u32;
    match native {
        true => {
            let mut stages = Vec::new();
            let mut layer = 0;
            while layer < layers {
                if layer + 1 == layers {
                    stages.push(MaterialStage::PEEL_DEPTH);
                    stages.push(MaterialStage::PEEL_FRONT);
                    break;
                }
                stages.push(MaterialStage::PEEL_DEPTH);
                stages.push(MaterialStage::PEEL_RESOLVE);
                layer += 2;
            }
            stages
        }
        false => (0..layers.min(abi::PEEL_LAYERS_MAX as u32 / 2))
            .flat_map(|layer| {
                [
                    MaterialStage::PEEL_DEPTH,
                    match layer % 2 {
                        0 => MaterialStage::PEEL_FRONT,
                        _ => MaterialStage::PEEL_BACK,
                    },
                ]
            })
            .collect(),
    }
}

#[test]
fn peel_draws_only_the_transparent_tag_and_stays_within_eight_passes() {
    let (document, registry) = peel_document();
    for native in [false, true] {
        for layers in [0, 1, 4, 7, 8, 100] {
            let compiled = compile(
                &document,
                &registry,
                &effects(),
                &peel_config(layers, native),
            )
            .unwrap_or_else(|error| panic!("native {native} layers {layers}: {error}"));
            compiled
                .schedule()
                .unwrap_or_else(|error| panic!("native {native} layers {layers}: {error}"));
            let draws = peel_draws(&compiled);
            let expected = expected_peel_stages(layers, native);
            assert_eq!(draws, expected, "native {native} layers {layers}");
            assert!(
                draws.len() <= abi::PEEL_LAYERS_MAX as usize,
                "native {native} layers {layers}: {} geometry passes",
                draws.len()
            );
        }
    }
}

#[test]
fn a_material_pass_cannot_name_a_peel_stage() {
    let registry = make_registry();
    let mut graph = wxsl_core::pipeline::document("not a peel");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let pass = graph.add(
        wxsl_core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("shade")
            .with_setting(doc::SETTING_STAGE, "peel_front"),
    );
    graph
        .wire(&registry, (scene, "draws"), (pass, "draws"))
        .expect("draws wire");
    match compile(&graph, &registry, &effects(), &config()) {
        Err(PipelineError::Peel { node, .. }) => assert_eq!(node, "shade"),
        other => panic!("expected a peel error, got {other:?}"),
    }
}

/// A forward document with the pyramid between the material pass and
/// the tonemap: material → `scene` → bloom → `post bloom` → tonemap —
/// the gallery demo's shape, so the tests and the demo cannot drift.
fn bloom_document() -> (Graph, NodeId, NodeId) {
    let effects = EffectRegistry::shipped();
    let registry = document_registry(&effects);
    let mut graph = StockPipeline::Forward.document();
    let scene = graph
        .nodes()
        .find(|(_, node)| node.def == doc::RESOURCE_COLOR)
        .map(|(id, _)| id)
        .expect("forward has a colour target");
    let tonemap = graph
        .nodes()
        .find(|(_, node)| node.def == doc::PASS_SCREEN)
        .map(|(id, _)| id)
        .expect("forward ends in tonemap");
    let bloom = graph.add(Node::new(doc::PASS_BLOOM).with_label("bloom"));
    let post = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("post bloom")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    graph
        .disconnect(&registry, &SocketRef::new(tonemap, "image"))
        .expect("tonemap was fed");
    for (from, to) in [
        ((scene, "color"), (bloom, "image")),
        ((post, "color"), (bloom, "into")),
        ((post, "color"), (tonemap, "image")),
    ] {
        graph.wire(&registry, from, to).expect("bloom wiring");
    }
    (graph, bloom, tonemap)
}

#[test]
fn the_pyramid_expands_to_levels_a_descent_an_ascent_and_a_combine() {
    let effects = EffectRegistry::shipped();
    let registry = document_registry(&effects);
    let (graph, _, _) = bloom_document();
    let compiled = compile(&graph, &registry, &effects, &config()).expect("compiles");
    compiled.schedule().expect("schedules");

    // The shape: extract, one down per deeper level, one up per
    // shallower one (deepest first), and the combine.
    fn effect(pass: &PassDesc) -> &str {
        match &pass.kind {
            PassKind::Screen { effect } => effect.as_str(),
            other => panic!("{other:?} is not a screen pass"),
        }
    }
    let index_of = |label: &str| {
        compiled
            .passes()
            .iter()
            .position(|pass| pass.label == label)
            .unwrap_or_else(|| panic!("no pass labelled `{label}`"))
    };
    let order: [(&str, &str); 8] = [
        ("bloom extract", "wxsl.bloom_extract"),
        ("bloom down 1", "wxsl.bloom_down"),
        ("bloom down 2", "wxsl.bloom_down"),
        ("bloom down 3", "wxsl.bloom_down"),
        ("bloom up 2", "wxsl.bloom_up"),
        ("bloom up 1", "wxsl.bloom_up"),
        ("bloom up 0", "wxsl.bloom_up"),
        ("bloom combine", "wxsl.bloom_combine"),
    ];
    let mut last = 0;
    for (label, id) in order {
        let at = index_of(label);
        assert!(at > last, "`{label}` is out of order");
        last = at;
        assert_eq!(effect(&compiled.passes()[at]), id, "`{label}` effect");
    }

    // The generated passes carry the node's settings as seeded
    // parameters, under their stable labels.
    let extract = &compiled.passes()[index_of("bloom extract")];
    assert_eq!(
        extract.parameters.get("threshold"),
        Some(&wxsl_core::node::Value::F32(1.0))
    );
    assert_eq!(
        extract.parameters.get("knee"),
        Some(&wxsl_core::node::Value::F32(0.6))
    );
    let combine = &compiled.passes()[index_of("bloom combine")];
    assert_eq!(
        combine.parameters.get("strength"),
        Some(&wxsl_core::node::Value::F32(0.85))
    );

    // The glow chain: HDR transients, each half its parent, none of
    // them document-visible. The extract reads the scene and writes
    // level 0; the combine reads the scene and the topmost fold and
    // writes what the document wired.
    let hdr = gbuffer_format(abi::GBufferPrecision::HighDynamicRange);
    let resource = |label: &str| {
        let id = compiled
            .resource_by_label(label)
            .unwrap_or_else(|| panic!("no resource labelled `{label}`"));
        compiled.resource_desc(id).unwrap()
    };
    for (label, scale) in [
        ("bloom level 0", 0.5),
        ("bloom level 1", 0.25),
        ("bloom level 2", 0.125),
        ("bloom level 3", 0.0625),
        ("bloom up 0", 0.5),
        ("bloom up 1", 0.25),
        ("bloom up 2", 0.125),
    ] {
        let desc = resource(label);
        let crate::pass::ResourceShape::Texture { extent, format, .. } = desc.shape else {
            panic!("`{label}` is not a texture");
        };
        assert_eq!(extent, Extent::Viewport { scale }, "`{label}` extent");
        assert_eq!(format, hdr, "`{label}` carries linear radiance");
        assert_eq!(
            desc.persistence,
            Persistence::Transient,
            "`{label}` is a transient"
        );
    }
    let scene = compiled
        .resource_by_label("scene")
        .expect("forward's colour target");
    let level0 = compiled
        .resource_by_label("bloom level 0")
        .expect("level 0");
    let top = compiled.resource_by_label("bloom up 0").expect("top fold");
    let post = compiled
        .resource_by_label("post bloom")
        .expect("the wired target");
    assert_eq!(extract.reads, vec![Read::current(scene)]);
    assert_eq!(extract.color[0].resource, level0);
    assert_eq!(
        combine.reads,
        vec![Read::current(scene), Read::current(top)]
    );
    assert_eq!(combine.color[0].resource, post);

    // And the scheduler orders the pyramid before the tonemap that
    // reads what it wrote, whatever the document order said.
    let scheduled = compiled.schedule().expect("schedules");
    let scheduled = scheduled.order();
    let (combine_at, tonemap_at) = (
        index_of("bloom combine"),
        compiled
            .passes()
            .iter()
            .position(|pass| pass.label == "tonemap")
            .expect("tonemap"),
    );
    let combine_run = scheduled[combine_at];
    let tonemap_run = scheduled[tonemap_at];
    assert!(
        combine_run < tonemap_run,
        "the combine must precede the tonemap that reads its target"
    );
}

#[test]
fn one_level_is_an_extract_and_a_combine() {
    let effects = EffectRegistry::shipped();
    let registry = document_registry(&effects);
    let (mut graph, bloom, _) = bloom_document();
    graph.set_setting(bloom, "levels", "1");
    let compiled = compile(&graph, &registry, &effects, &config()).expect("compiles");
    let labels: Vec<&str> = compiled
        .passes()
        .iter()
        .filter_map(|pass| match &pass.kind {
            PassKind::Screen { effect } if effect.starts_with("wxsl.bloom_") => {
                Some(match effect.as_str() {
                    "wxsl.bloom_extract" => "extract",
                    "wxsl.bloom_combine" => "combine",
                    other => panic!("pyramid of one level grew a {other}"),
                })
            }
            _ => None,
        })
        .collect();
    assert_eq!(labels, ["extract", "combine"]);
    // With no folds, the combine reads the extract's own level.
    let level0 = compiled
        .resource_by_label("bloom level 0")
        .expect("level 0");
    let combine = compiled
        .passes()
        .iter()
        .find(|pass| pass.label == "bloom combine")
        .expect("combine");
    assert!(combine.reads.contains(&Read::current(level0)));
    compiled.schedule().expect("schedules");
}

#[test]
fn the_dof_chain_expands_to_circles_a_far_blur_and_a_near_composite() {
    let effects = EffectRegistry::shipped();
    let registry = document_registry(&effects);
    let mut graph = StockPipeline::Forward.document();
    let scene = graph
        .nodes()
        .find(|(_, node)| node.def == doc::RESOURCE_COLOR)
        .map(|(id, _)| id)
        .expect("forward has a colour target");
    let depth = graph
        .nodes()
        .find(|(_, node)| node.def == doc::RESOURCE_DEPTH)
        .map(|(id, _)| id)
        .expect("forward has a depth target");
    let tonemap = graph
        .nodes()
        .find(|(_, node)| node.def == doc::PASS_SCREEN)
        .map(|(id, _)| id)
        .expect("forward ends in tonemap");
    let dof = graph.add(Node::new(doc::PASS_DOF).with_label("dof"));
    let post = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("post dof")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    graph
        .disconnect(&registry, &SocketRef::new(tonemap, "image"))
        .expect("tonemap was fed");
    for (from, to) in [
        ((scene, "color"), (dof, "image")),
        ((depth, "depth"), (dof, "depth")),
        ((post, "color"), (dof, "into")),
        ((post, "color"), (tonemap, "image")),
    ] {
        graph.wire(&registry, from, to).expect("dof wiring");
    }
    let compiled = compile(&graph, &registry, &effects, &config()).expect("compiles");
    compiled.schedule().expect("schedules");

    // The shape: circles, far blur, near composite, in that order,
    // with the lens' numbers seeded from the node's settings.
    let index_of = |label: &str| {
        compiled
            .passes()
            .iter()
            .position(|pass| pass.label == label)
            .unwrap_or_else(|| panic!("no pass labelled `{label}`"))
    };
    let circles_at = index_of("dof circles");
    let far_at = index_of("dof far blur");
    let near_at = index_of("dof near blur");
    assert!(circles_at < far_at && far_at < near_at, "chain order");
    let effect = |pass: &PassDesc| match &pass.kind {
        PassKind::Screen { effect } => effect.clone(),
        other => panic!("{other:?} is not a screen pass"),
    };
    let circles = &compiled.passes()[circles_at];
    let near = &compiled.passes()[near_at];
    assert_eq!(effect(circles), "wxsl.dof_coc");
    assert_eq!(effect(&compiled.passes()[far_at]), "wxsl.dof_far");
    assert_eq!(effect(near), "wxsl.dof_near");
    assert_eq!(
        circles.parameters.get("focus"),
        Some(&wxsl_core::node::Value::F32(10.0))
    );
    assert_eq!(
        circles.parameters.get("f_number"),
        Some(&wxsl_core::node::Value::F32(2.8))
    );
    assert_eq!(
        near.parameters.get("max_coc"),
        Some(&wxsl_core::node::Value::F32(16.0))
    );
    assert_eq!(
        compiled.passes()[far_at].parameters.get("max_coc"),
        near.parameters.get("max_coc")
    );

    // The wiring: the discs measure the frame's own depth, and the
    // composite writes what the document wired.
    let forward_depth = compiled
        .resource_by_label("forward depth")
        .expect("forward's depth target");
    assert_eq!(circles.reads, vec![Read::current(forward_depth)]);
    let scene_id = compiled.resource_by_label("scene").expect("scene");
    let coc = compiled
        .resource_by_label("dof coc")
        .expect("the CoC transient");
    let far = compiled
        .resource_by_label("dof far")
        .expect("the far transient");
    assert_eq!(
        compiled.passes()[far_at].reads,
        vec![Read::current(scene_id), Read::current(coc)]
    );
    let post = compiled
        .resource_by_label("post dof")
        .expect("the wired target");
    assert_eq!(
        near.reads,
        vec![
            Read::current(scene_id),
            Read::current(coc),
            Read::current(far)
        ]
    );
    assert_eq!(near.color[0].resource, post);

    // The intermediates are the expansion's own HDR transients — the
    // discs are per-pixel data, so they are full resolution.
    let hdr = gbuffer_format(abi::GBufferPrecision::HighDynamicRange);
    for transient in ["dof coc", "dof far"] {
        let id = compiled
            .resource_by_label(transient)
            .unwrap_or_else(|| panic!("no `{transient}`"));
        let desc = compiled.resource_desc(id).unwrap();
        let crate::pass::ResourceShape::Texture { extent, format, .. } = desc.shape else {
            panic!("`{transient}` is not a texture");
        };
        assert_eq!(extent, Extent::Viewport { scale: 1.0 });
        assert_eq!(format, hdr);
        assert_eq!(desc.persistence, Persistence::Transient);
    }
}

#[test]
fn a_bad_dof_disc_cap_is_a_named_error() {
    let effects = EffectRegistry::shipped();
    let registry = document_registry(&effects);
    let mut graph = StockPipeline::Forward.document();
    let scene = graph
        .nodes()
        .find(|(_, node)| node.def == doc::RESOURCE_COLOR)
        .map(|(id, _)| id)
        .expect("forward has a colour target");
    let depth = graph
        .nodes()
        .find(|(_, node)| node.def == doc::RESOURCE_DEPTH)
        .map(|(id, _)| id)
        .expect("forward has a depth target");
    let dof = graph.add(Node::new(doc::PASS_DOF).with_label("dof"));
    for (from, to) in [
        ((scene, "color"), (dof, "image")),
        ((depth, "depth"), (dof, "depth")),
    ] {
        graph.wire(&registry, from, to).expect("dof wiring");
    }
    for value in ["0", "-4"] {
        let mut invalid = graph.clone();
        invalid.set_setting(dof, "max_coc", value);
        let error = compile(&invalid, &registry, &effects, &config())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("depth of field `dof`") && error.contains("max_coc"),
            "`{value}`: {error}"
        );
    }
    // A setting that is not a number is the shared BadNumber error,
    // which names the setting rather than the recipe.
    let mut invalid = graph.clone();
    invalid.set_setting(dof, "max_coc", "wide");
    let error = compile(&invalid, &registry, &effects, &config())
        .unwrap_err()
        .to_string();
    assert!(error.contains("`max_coc`"), "{error}");
}

#[test]
fn a_bad_level_count_is_a_named_error() {
    let effects = EffectRegistry::shipped();
    let registry = document_registry(&effects);
    for value in ["0", "13", "many"] {
        let (mut graph, bloom, _) = bloom_document();
        graph.set_setting(bloom, "levels", value);
        let error = compile(&graph, &registry, &effects, &config())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("bloom pyramid `bloom`") && error.contains("levels"),
            "`{value}`: {error}"
        );
    }
}

#[test]
fn an_unwired_pyramid_output_is_nobody_target_to_sample() {
    let effects = EffectRegistry::shipped();
    let registry = document_registry(&effects);
    let (mut graph, bloom, tonemap) = bloom_document();
    // `into` unconnected: the combine writes the frame's own target,
    // and reading the pyramid's output is the un-compilable chain
    // shape every other pass names.
    graph
        .disconnect(&registry, &SocketRef::new(tonemap, "image"))
        .expect("tonemap was fed");
    graph
        .wire(&registry, (bloom, "color"), (tonemap, "image"))
        .expect("wire");
    let error = compile(&graph, &registry, &effects, &config()).unwrap_err();
    assert!(
        matches!(error, PipelineError::ImageFromPass { .. }),
        "{error}"
    );
}

#[cfg(test)]
mod preset_dump {
    //! Writes the preset files from [`super::stock_document`]. Run once
    //! with `cargo test -p wxsl-frame --lib preset_dump -- --ignored`,
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
