//! Peeled and sorted transparency documents.

use super::super::*;

/// Opaque colour and depth, then dual depth peeling of the peeled tier,
/// then the sorted tier over the composite, then the same tonemap every
/// other demo presents through.
///
/// The layer count is the default of `wxsl_peel_layers`, four: a ray
/// through the torus and the sphere meets four surfaces, and four is also
/// what the baseline path can peel inside the eight-pass budget. The
/// plate behind the pair asks for no layers at all (plan5 D5), so no peel
/// pass reads it: a sorted-tier pass composites it over the peel
/// composite after them, alpha-over, and the tonemap reads what it left.
pub(crate) fn peel_document() -> Graph {
    let registry = wxsl::core::pipeline::registry();
    let mut graph = wxsl::core::pipeline::document("dual depth peel");
    // The opaque pass draws this source. The peel ignores it and draws
    // `transparent` itself, so the two passes never draw the same instance.
    let scene = graph.add(Node::new(doc::SOURCE_SCENE).with_setting(doc::SETTING_TAGS, TAG_OPAQUE));
    let transparents =
        graph.add(Node::new(doc::SOURCE_SCENE).with_setting(doc::SETTING_TAGS, TAG_TRANSPARENT));
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let color = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let shade = graph.add(Node::new(doc::PASS_GEOMETRY).with_label("opaque"));
    let peel = graph.add(Node::new(doc::PASS_PEEL).with_label("peel"));
    let peeled = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("peeled")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    // The sorted tier: the plate, and only the plate (`layers = sorted`
    // is the tier no peel reads). It loads the peel composite rather than
    // clearing it, and its declaration after the peel is what puts it in
    // front of what the pair left.
    let sorted = graph.add(
        Node::new(doc::PASS_GEOMETRY)
            .with_label("sorted transparents")
            .with_setting(doc::SETTING_LAYERS, "sorted")
            .with_setting(doc::SETTING_SORT, "back to front")
            .with_setting(doc::SETTING_BLEND, "alpha over"),
    );
    let tonemap = graph.add(
        Node::new(doc::PASS_SCREEN)
            .with_label("tonemap")
            .with_setting(doc::SETTING_EFFECT, "wxsl.tonemap"),
    );
    let present = graph.add_node(doc::PRESENT);
    for (from, to) in [
        ((scene, "draws"), (shade, "draws")),
        ((depth, "depth"), (shade, "depth")),
        ((color, "color"), (shade, "into")),
        ((shade, "depth"), (peel, "depth")),
        ((shade, "color"), (peel, "scene")),
        ((peeled, "color"), (peel, "into")),
        ((shade, "depth"), (sorted, "depth")),
        ((transparents, "draws"), (sorted, "draws")),
        ((peeled, "color"), (sorted, "into")),
        ((peeled, "color"), (tonemap, "image")),
        ((tonemap, "color"), (present, "surface")),
    ] {
        graph
            .wire(&registry, from, to)
            .expect("the peel document wires");
    }
    graph
}

/// One blended pass over the frame (plan5 D3/D4): everything tagged
/// `transparent`, straight-alpha over, and — with `sorted` — the pass's
/// own back-to-front sort, the order the draws' `render_order` groups
/// ride. The pass writes the frame target as its first writer, so the
/// clear colour is the background the composite sits on.
pub(crate) fn blended_document(name: &str, sorted: bool) -> Graph {
    let registry = wxsl::core::pipeline::registry();
    let mut graph = wxsl::core::pipeline::document(name);
    let scene =
        graph.add(Node::new(doc::SOURCE_SCENE).with_setting(doc::SETTING_TAGS, TAG_TRANSPARENT));
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let mut pass = Node::new(doc::PASS_GEOMETRY)
        .with_label("blended")
        .with_setting(doc::SETTING_BLEND, "alpha over");
    if sorted {
        pass = pass.with_setting(doc::SETTING_SORT, "back to front");
    }
    let blended = graph.add(pass);
    let present = graph.add_node(doc::PRESENT);
    for (from, to) in [
        ((scene, "draws"), (blended, "draws")),
        ((depth, "depth"), (blended, "depth")),
        ((blended, "color"), (present, "surface")),
    ] {
        graph
            .wire(&registry, from, to)
            .expect("the blended document wires");
    }
    graph
}

pub(crate) fn order_document() -> Graph {
    blended_document("render order", true)
}

pub(crate) fn alpha_over_document() -> Graph {
    blended_document("alpha over", false)
}
