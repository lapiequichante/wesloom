//! An effect authored as a graph, on the GPU (ADR 0040).
//!
//! The device-free halves — that a screen graph generates, that its module
//! imports the library rather than copying it, that a screen node in a
//! material is refused by name — are in `wxsl`'s own unit tests. What needs
//! a device is the claim the whole item rests on: a pipeline naming
//! `tonemap` does not care whether the effect behind that id is a file or a
//! graph, and the image does not either.
//!
//! Both tests render the same emissive plane through the same chain — a
//! `resource.color` the forward pass writes into, then one screen effect
//! into the frame's target — and differ only in which effect that is.

use glam::Mat4;
use wxsl::core::abi;
use wxsl::core::graph::{Graph, Node};
use wxsl::core::node::Value;
use wxsl::core::pipeline as doc;
use wxsl::render::{DrawItem, PipelineConfig, Renderer};

mod probe;

use probe::{gpu, pixel, render_list, Harness, SIZE};

/// A chain of screen effects over a forward pass: shade into an HDR image,
/// run each effect in turn over what the last one wrote, present the end.
///
/// The shape every stock pipeline has had since ADR 0039, minus the parts a
/// probe does not need — so what differs between two runs of it really is
/// only the effects named here.
fn chain_document(effects: &[&str]) -> Graph {
    let registry = doc::registry();
    let mut graph = doc::document(effects.join(" then "));
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let shade = graph.add(Node::new(doc::PASS_GEOMETRY).with_label("forward"));
    let present = graph.add_node(doc::PRESENT);
    graph
        .wire(&registry, (scene, "draws"), (shade, "draws"))
        .expect("draws");
    graph
        .wire(&registry, (depth, "depth"), (shade, "depth"))
        .expect("depth");

    // Each effect reads the image the pass before it wrote. The last one
    // leaves its own `into` unwired, so it writes the frame's target —
    // which is what makes the chain end rather than go on.
    let mut writer = (shade, "color");
    for (index, effect) in effects.iter().enumerate() {
        let image = graph.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_setting(doc::SETTING_PRECISION, "hdr")
                .with_label(format!("image {index}")),
        );
        let pass = graph.add(
            Node::new(doc::PASS_SCREEN)
                .with_setting(doc::SETTING_EFFECT, *effect)
                .with_label(*effect),
        );
        graph
            .wire(&registry, (image, "color"), (writer.0, "into"))
            .expect("the writer writes into the image");
        graph
            .wire(&registry, (image, "color"), (pass, "image"))
            .expect("and the effect reads it");
        writer = (pass, "color");
    }
    graph
        .wire(&registry, writer, (present, "surface"))
        .expect("the end of the chain presents");
    graph
}

/// Put `renderer` on [`chain_document`] for `effects`.
fn present_through(renderer: &mut Renderer, effects: &[&str]) {
    let document = chain_document(effects);
    let graph = wxsl::render::compile_pipeline(
        &document,
        &doc::registry(),
        renderer.effects(),
        &PipelineConfig::new(renderer.target()),
    )
    .expect("the chain compiles");
    renderer.set_graph(graph).expect("the chain runs");
}

/// A plane emitting `color`, which with no lights is the whole image.
fn emissive(color: [f32; 3]) -> Graph {
    let mut graph = Graph::new("emissive");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.set_param(output, "emissive", Value::Vec3(color));
    graph
}

/// Draw one plane of `graph`'s material and read the whole image back.
fn render(harness: &mut Harness, graph: &Graph) -> Vec<u8> {
    let material = harness.material(graph);
    let item = DrawItem::new(&harness.mesh, &material)
        // Lying flat by default, so stand it up to face the camera.
        .with_transform(Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2));
    let draws = wxsl::render::single_draw(item);
    render_list(&harness.gpu, &mut harness.renderer, &harness.target, &draws)
        .expect("the frame renders")
}

#[test]
fn a_graph_authored_tonemap_is_the_shipped_one() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    // Bright enough that the curve is doing real work: at 2.4 linear the
    // filmic shoulder is most of the difference between this and a clamp,
    // so two implementations that disagree cannot agree here by accident.
    let plane = emissive([2.4, 0.9, 0.35]);

    present_through(&mut harness.renderer, &["tonemap"]);
    let from_file = render(&mut harness, &plane);

    // The same id, now answered by a graph — which is the whole claim: the
    // document is untouched, the pipeline is recompiled from the same
    // text, and what changed is only where the effect's WXSL came from.
    let effect = wxsl::effects::tonemap(&harness.registry).expect("the graph generates");
    harness.renderer.add_effect(effect);
    present_through(&mut harness.renderer, &["tonemap"]);
    let from_graph = render(&mut harness, &plane);

    let worst = from_file
        .chunks_exact(4)
        .zip(from_graph.chunks_exact(4))
        .flat_map(|(a, b)| (0..3).map(move |c| a[c].abs_diff(b[c])))
        .max()
        .expect("a non-empty image");
    assert!(
        worst <= 1,
        "the graph and the file are the same display transform; worst channel gap {worst}"
    );
    // And it is a tonemapped image rather than two identical black frames:
    // 2.4 linear comes back well short of saturated, which is what a curve
    // does and a clamp does not.
    let centre = pixel(&from_graph, SIZE / 2, SIZE / 2);
    assert!(
        (200..255).contains(&centre[0]) && centre[1] < centre[0],
        "the curve ran: {centre:?}"
    );
}

#[test]
fn fxaa_softens_an_edge_and_leaves_a_flat_region_alone() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    // A plane covering the middle of the frame: its silhouette against the
    // cleared background is one hard, aliased edge, and everything inside
    // it is flat. FXAA goes *after* the display transform — perceived
    // edges are what alias — so the baseline is the same chain without the
    // last link.
    let plane = emissive([0.9, 0.9, 0.9]);

    present_through(&mut harness.renderer, &["tonemap"]);
    let plain = render(&mut harness, &plane);

    let effect = wxsl::effects::fxaa(&harness.registry).expect("the graph generates");
    harness.renderer.add_effect(effect);
    present_through(&mut harness.renderer, &["tonemap", "fxaa"]);
    let filtered = render(&mut harness, &plane);

    // Somewhere along the silhouette, a pixel moved.
    let moved = plain
        .chunks_exact(4)
        .zip(filtered.chunks_exact(4))
        .filter(|(a, b)| a[0].abs_diff(b[0]) > 2)
        .count();
    assert!(moved > 0, "FXAA found no edge to soften");

    // And the middle of the plane — flat, well inside the silhouette — did
    // not: an anti-aliaser that blurs everything is a blur.
    let inside_before = pixel(&plain, SIZE / 2, SIZE / 2);
    let inside_after = pixel(&filtered, SIZE / 2, SIZE / 2);
    assert_eq!(
        inside_before, inside_after,
        "a flat region is left exactly as it was"
    );
}
