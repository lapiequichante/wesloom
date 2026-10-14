//! Bake and compute proofs: the BRDF LUT, buffer ramps, baked terms.

use super::super::*;

/// The execution-policy proof as a document (ADR 0035): a compute effect
/// bakes the split-sum environment-BRDF LUT once into a stable target, and
/// a screen effect displays that target every frame. The bake is a
/// `pass.compute.brdf_lut` node — its `lut` socket is the declaration's,
/// the target a 64x64 fixed `resource.color`, the policy a setting.
pub(crate) fn brdf_lut_document() -> Graph {
    let effects = EffectRegistry::default().with(BRDF_LUT).with(LUT_VIEW);
    let registry = wxsl::render::document_registry(&effects);
    let mut document = wxsl_core::pipeline::document("brdf lut");
    let lut = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("brdf lut")
            .with_setting(doc::SETTING_PRECISION, "hdr")
            .with_setting(doc::SETTING_SIZE, "64x64"),
    );
    let bake = document.add(
        wxsl::core::graph::Node::new(format!("{}wxsl.brdf_lut", doc::PASS_COMPUTE_PREFIX))
            .with_label("lut bake")
            .with_setting(doc::SETTING_POLICY, "once"),
    );
    let view = document.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("lut view")
            .with_setting(doc::SETTING_EFFECT, "wxsl.lut_view"),
    );
    let present = document.add_node(doc::PRESENT);
    for (from, to) in [
        ((lut, "color"), (bake, "lut")),
        ((lut, "color"), (view, "image")),
        ((view, "color"), (present, "surface")),
    ] {
        document.wire(&registry, from, to).expect("lut wiring");
    }
    document
}

/// The buffer proof as a document (ADR 0036): a compute effect fills a
/// 256-entry storage buffer, a screen effect reads it as storage and draws
/// it, one value per column. The buffer is a `resource.buffer` both passes
/// wire from — the write declared on the compute node's `ramp` socket, the
/// read on the screen pass's `buffer` socket.
pub(crate) fn buffer_ramp_document() -> Graph {
    let effects = EffectRegistry::default().with(RAMP_FILL).with(RAMP_VIEW);
    let registry = wxsl::render::document_registry(&effects);
    let mut document = wxsl_core::pipeline::document("buffer ramp");
    let ramp = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_BUFFER)
            .with_label("ramp")
            .with_setting(doc::SETTING_BYTES, "1024"),
    );
    let fill = document.add(
        wxsl::core::graph::Node::new(format!("{}wxsl.ramp_fill", doc::PASS_COMPUTE_PREFIX))
            .with_label("fill ramp"),
    );
    let view = document.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("show ramp")
            .with_setting(doc::SETTING_EFFECT, "wxsl.ramp_view"),
    );
    let present = document.add_node(doc::PRESENT);
    for (from, to) in [
        ((ramp, "buffer"), (fill, "ramp")),
        ((ramp, "buffer"), (view, "buffer")),
        ((view, "color"), (present, "surface")),
    ] {
        document.wire(&registry, from, to).expect("ramp wiring");
    }
    document
}

/// The bake demo's material: the graph is a document, `bake_term.wxsl.json`
/// — the same move `pbr_cube` makes, plus the `bakes` declaration that says
/// node 5's value becomes a table ([ADR 0045](../../../docs/adr/0045-a-bake-is-an-effect-over-a-material-subgraph.md)).
/// `Stage` compiles the *same parsed graph* into the effect's shader and
/// the material, which is the point: one authoring, two compilations.
pub(crate) fn bake_material_graph() -> Graph {
    let graph: Graph = serde_json::from_str(include_str!("../../../assets/bake_term.wxsl.json"))
        .expect("the bake material document parses");
    graph
        .validate(&wxsl::stdlib::registry())
        .expect("the bake material document validates");
    graph
}

/// The bake demo's pipeline: the deferred chain, with the bake — an
/// imported colour target labelled with the declaration's texture name and
/// a `pass.compute.<effect>` node writing it under policy `once` — added
/// *before* the material pass. That placement is not style: the material
/// samples the table through its own bind group, which is a dependency the
/// scheduler cannot see, and declaration order is its tie-break. A bake
/// written after the pass that samples it would feed it an empty table for
/// one frame ([ADR 0045](../../../docs/adr/0045-a-bake-is-an-effect-over-a-material-subgraph.md)).
/// The effect itself is `Stage::new`'s, generated from the material
/// subgraph; a document names it by id like any other.
pub(crate) fn bake_document() -> Graph {
    let bake = bake_effect();
    let effects = EffectRegistry::default().with(bake);
    let registry = wxsl::render::document_registry(&effects);
    let mut graph = wxsl_core::pipeline::document("deferred + bake");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let lights = graph.add_node(doc::SOURCE_LIGHTS);
    let shadows = graph.add_node(doc::PASS_SHADOW);
    let table = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("roughness_bake")
            .with_setting(doc::SETTING_PRECISION, "hdr")
            .with_setting(doc::SETTING_IMPORTED, "true"),
    );
    let bake_pass = graph.add(
        wxsl::core::graph::Node::new(format!("{}demo.bake_roughness", doc::PASS_COMPUTE_PREFIX))
            .with_label("roughness bake")
            .with_setting(doc::SETTING_POLICY, "once"),
    );
    let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
    let material = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("deferred material")
            .with_setting(doc::SETTING_STAGE, "gbuffer"),
    );
    let lighting =
        graph.add(wxsl::core::graph::Node::new(doc::PASS_SCREEN).with_label("deferred lighting"));
    let present = graph.add_node(doc::PRESENT);
    let mut wire = |from: (NodeId, &str), to: (NodeId, &str)| {
        graph.wire(&registry, from, to).expect("bake demo wiring");
    };
    wire((scene, "draws"), (shadows, "draws"));
    wire((lights, "shadows"), (shadows, "into"));
    wire((table, "color"), (bake_pass, "bake"));
    wire((scene, "draws"), (material, "draws"));
    wire((gbuffer, "gbuffer"), (material, "gbuffer"));
    wire((gbuffer, "gbuffer"), (lighting, "gbuffer"));
    wire((lighting, "color"), (present, "surface"));
    // The head pass is redirected into an HDR target and presents through
    // the display transform, exactly as every stock chain ends.
    present_through_tonemap(&mut graph, lighting, present);
    graph
}

/// The bake effect, generated from the material subgraph — what
/// `Stage::new` registers on the renderer and `bake_document` names. One
/// generation here, one in `Stage::new`: both cheap, both from the same
/// declaration, and the variant cache keys on the source's hash so there
/// is no question of them disagreeing.
pub(crate) fn bake_effect() -> wxsl::render::effect::Effect {
    wxsl::render::effect::Effect::from_bake(
        "demo.bake_roughness",
        "roughness bake",
        "Evaluate the material's baked term over its 256x256 table.",
        &bake_material_graph(),
        "roughness_bake",
        &wxsl::core::macros::MacroSet::new(),
        &wxsl::stdlib::registry(),
    )
    .expect("the bake material generates its effect")
}
