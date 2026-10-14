//! Minimal forward, bloom and pyramid documents.

use super::super::*;

/// The minimal pipeline there is: a scene, one lit material pass with a
/// depth target of its own, present. Three nodes and a resource — what a
/// pipeline document looks like with everything optional left out.
pub(crate) fn minimal_forward_document() -> Graph {
    let registry = wxsl::core::pipeline::registry();
    let mut graph = wxsl::core::pipeline::document("single pass");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let pass = graph.add_node(doc::PASS_GEOMETRY);
    let present = graph.add_node(doc::PRESENT);
    let mut wire = |from: (NodeId, &str), to: (NodeId, &str)| {
        graph.wire(&registry, from, to).expect("gallery wiring");
    };
    wire((scene, "draws"), (pass, "draws"));
    wire((depth, "depth"), (pass, "depth"));
    wire((pass, "color"), (present, "surface"));
    // `wire` borrows the graph, so its last use has to come before the
    // helper's own wiring.
    present_through_tonemap(&mut graph, pass, present);
    graph
}

/// The `tonemap` effect on the end of `graph`'s chain: the display
/// transform every pipeline that presents to a screen needs
/// ([ADR 0039](../../../docs/adr/0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md)).
///
/// `head` is the pass currently writing the frame's target. It is given an
/// HDR colour target to write instead, and the tonemap reads that and
/// presents. Three nodes and a rewire, in a demo, because that is exactly
/// what the stock presets do — a chain is a chain wherever it is built.
pub(crate) fn present_through_tonemap(graph: &mut Graph, head: NodeId, present: NodeId) {
    // The document registry, not the static one: a head whose effect has
    // more than one image — the separable bloom's vertical pass — is a
    // derived row, and its `into` socket is not on the fixed pass.
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let hdr = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("linear")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let tonemap = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("tonemap")
            .with_setting(doc::SETTING_EFFECT, "wxsl.tonemap"),
    );
    graph.disconnect(
        &registry,
        &wxsl::core::graph::SocketRef::new(present, "surface"),
    );
    for (from, to) in [
        ((hdr, "color"), (head, "into")),
        ((hdr, "color"), (tonemap, "image")),
        ((tonemap, "color"), (present, "surface")),
    ] {
        graph.wire(&registry, from, to).expect("gallery wiring");
    }
}

/// The shipped deferred preset with a separable bloom composed onto it.
/// Lighting writes an HDR target; [`separable_bloom`] blurs the bright
/// part along x and then along y and adds it back. The one-pass bloom's
/// taps sit four texels apart, which bands, so these demos do not use it.
pub(crate) fn deferred_bloom_document() -> Graph {
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut graph = wxsl::core::pipeline::document("deferred bloom");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let lights = graph.add_node(doc::SOURCE_LIGHTS);
    let shadows = graph.add_node(doc::PASS_SHADOW);
    let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
    let material = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("deferred material")
            .with_setting(doc::SETTING_STAGE, "gbuffer"),
    );
    // HDR on purpose: bloom thresholds *linear radiance*, and the
    // highlight it is for is brighter than white — an 8-bit intermediate
    // would clamp it to exactly the threshold and the glow would be a
    // rumour. The same honesty the `linear` target before the tonemap has.
    let scene_color = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let lighting =
        graph.add(wxsl::core::graph::Node::new(doc::PASS_SCREEN).with_label("deferred lighting"));
    let present = graph.add_node(doc::PRESENT);
    let mut wire = |from: (NodeId, &str), to: (NodeId, &str)| {
        graph.wire(&registry, from, to).expect("gallery wiring");
    };
    wire((scene, "draws"), (shadows, "draws"));
    wire((lights, "shadows"), (shadows, "into"));
    wire((scene, "draws"), (material, "draws"));
    wire((gbuffer, "gbuffer"), (material, "gbuffer"));
    wire((gbuffer, "gbuffer"), (lighting, "gbuffer"));
    wire((scene_color, "color"), (lighting, "into"));
    let bloom = separable_bloom(&mut graph, scene_color);
    graph
        .wire(&registry, (bloom, "color"), (present, "surface"))
        .expect("gallery wiring");
    // Bloom thresholds *linear* radiance, so it belongs before the display
    // transform — which is the physically correct order, and the one the
    // move in ADR 0039 made expressible.
    present_through_tonemap(&mut graph, bloom, present);
    graph
}

/// Horizontal Gaussian of the bright part of `source`, then the same
/// Gaussian along y, added back onto `source`. Returns the vertical pass,
/// which is the head of the chain.
///
/// `threshold` and `knee` are on the pass labelled `bloom`. `strength` is
/// on the pass labelled `bloom-y`. The glow resource is HDR: the bright
/// signal is linear radiance, and an 8-bit intermediate would clamp it.
pub(crate) fn separable_bloom(graph: &mut Graph, source: NodeId) -> NodeId {
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let glow = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("glow")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let bloom_x = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("bloom")
            .with_setting(doc::SETTING_EFFECT, "wxsl.bloom_x"),
    );
    let bloom_y = graph.add(
        wxsl::core::graph::Node::new(format!("{}wxsl.bloom_y", doc::PASS_SCREEN_PREFIX))
            .with_label("bloom-y"),
    );
    for (from, to) in [
        ((source, "color"), (bloom_x, "image")),
        ((glow, "color"), (bloom_x, "into")),
        ((source, "color"), (bloom_y, "color")),
        ((glow, "color"), (bloom_y, "glow")),
    ] {
        graph.wire(&registry, from, to).expect("gallery wiring");
    }
    bloom_y
}

/// The shipped deferred preset with a bloom pyramid composed onto it —
/// [`doc::PASS_BLOOM`], the whole pyramid in one document node (ADR 0065)
/// where [`deferred_bloom_document`] wires the separable pair by hand.
/// Lighting writes an HDR target; the node's expansion thresholds it into
/// a half-resolution first level, boxes it down three more, folds the
/// gathered energy back up, and the combine adds it onto the image. The
/// generated passes tune like any other: `threshold` on `bloom extract`,
/// `strength` on `bloom combine`.
pub(crate) fn deferred_pyramid_document() -> Graph {
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut graph = wxsl::core::pipeline::document("deferred bloom pyramid");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let lights = graph.add_node(doc::SOURCE_LIGHTS);
    let shadows = graph.add_node(doc::PASS_SHADOW);
    let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
    let material = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("deferred material")
            .with_setting(doc::SETTING_STAGE, "gbuffer"),
    );
    // HDR on purpose, as in [`deferred_bloom_document`]: the pyramid's
    // threshold reads linear radiance, and an 8-bit intermediate would
    // clamp the highlight the glow is for.
    let scene_color = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let lighting =
        graph.add(wxsl::core::graph::Node::new(doc::PASS_SCREEN).with_label("deferred lighting"));
    let pyramid = graph.add(wxsl::core::graph::Node::new(doc::PASS_BLOOM).with_label("bloom"));
    let present = graph.add_node(doc::PRESENT);
    let mut wire = |from: (NodeId, &str), to: (NodeId, &str)| {
        graph.wire(&registry, from, to).expect("gallery wiring");
    };
    wire((scene, "draws"), (shadows, "draws"));
    wire((lights, "shadows"), (shadows, "into"));
    wire((scene, "draws"), (material, "draws"));
    wire((gbuffer, "gbuffer"), (material, "gbuffer"));
    wire((gbuffer, "gbuffer"), (lighting, "gbuffer"));
    wire((scene_color, "color"), (lighting, "into"));
    wire((scene_color, "color"), (pyramid, "image"));
    present_through_tonemap(&mut graph, pyramid, present);
    graph
}

/// The deferred bloom pipeline with FXAA inserted before the separable
/// bloom: deferred lighting writes into an HDR target, FXAA anti-aliases
/// luminance edges in linear radiance, then the horizontal and vertical
/// bloom passes run, and the result is presented through tonemap.
pub(crate) fn bloom_instances_document() -> Graph {
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut graph = wxsl::core::pipeline::document("deferred bloom instances with fxaa");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let lights = graph.add_node(doc::SOURCE_LIGHTS);
    let shadows = graph.add_node(doc::PASS_SHADOW);
    let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
    let material = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("deferred material")
            .with_setting(doc::SETTING_STAGE, "gbuffer"),
    );
    let scene_color = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let lighting =
        graph.add(wxsl::core::graph::Node::new(doc::PASS_SCREEN).with_label("deferred lighting"));
    let antialiased = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("antialiased")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let fxaa = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("fxaa")
            .with_setting(doc::SETTING_EFFECT, "wxsl.fxaa"),
    );
    let present = graph.add_node(doc::PRESENT);
    let mut wire = |from: (NodeId, &str), to: (NodeId, &str)| {
        graph.wire(&registry, from, to).expect("gallery wiring");
    };
    wire((scene, "draws"), (shadows, "draws"));
    wire((lights, "shadows"), (shadows, "into"));
    wire((scene, "draws"), (material, "draws"));
    wire((gbuffer, "gbuffer"), (material, "gbuffer"));
    wire((gbuffer, "gbuffer"), (lighting, "gbuffer"));
    wire((scene_color, "color"), (lighting, "into"));
    wire((scene_color, "color"), (fxaa, "image"));
    wire((antialiased, "color"), (fxaa, "into"));
    let bloom = separable_bloom(&mut graph, antialiased);
    graph
        .wire(&registry, (bloom, "color"), (present, "surface"))
        .expect("gallery wiring");
    present_through_tonemap(&mut graph, bloom, present);
    graph
}
