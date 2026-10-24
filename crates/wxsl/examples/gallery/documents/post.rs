//! Post chains: sky, FXAA, TAA, motion blur, depth of field.

use super::super::*;

/// Optical sky preview, replacing the scene image before the display transform.
/// The rendered scene supplies an extent only, not background compositing or IBL.
pub(crate) fn sky_document() -> Graph {
    use wxsl::core::graph::{Node, SocketRef};
    let registry = doc::registry();
    let mut document = StockPipeline::Forward.document();
    document.set_name("single-scattering sky preview");
    let tonemap = document
        .nodes()
        .find(|(_, node)| {
            node.settings
                .get(doc::SETTING_EFFECT)
                .is_some_and(|id| id == "wxsl.tonemap")
        })
        .map(|(id, _)| id)
        .expect("stock tonemap");
    let input = SocketRef::new(tonemap, "image");
    let source = document
        .edge_into(&input)
        .expect("stock HDR image")
        .from
        .clone();
    document.disconnect(&registry, &input);
    let image = document.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("sky radiance")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let sky = document.add(
        Node::new(doc::PASS_SCREEN)
            .with_label("sky")
            .with_setting(doc::SETTING_EFFECT, "wxsl.sky"),
    );
    document
        .wire(
            &registry,
            (source.node, source.socket.as_str()),
            (sky, "image"),
        )
        .expect("extent input");
    document
        .wire(&registry, (image, "color"), (sky, "into"))
        .expect("linear radiance");
    document
        .wire(&registry, (image, "color"), (tonemap, "image"))
        .expect("display transform last");
    document
}

/// The stock forward document with one node appended: a `pass.screen`
/// running `fxaa`, reading what the tonemap wrote
/// ([ADR 0040](../../../docs/adr/0040-screen-domain-graphs-postprocess-is-a-material-over-the-frame.md)).
///
/// After the display transform on purpose: perceived edges are what alias,
/// and by that point in the chain the numbers are perceptual. The two
/// effects this document names are both *graphs* — see where the gallery
/// registers them — so what runs here is two generated modules, composed as
/// a document edit, with nothing in `wxsl-render` touched to allow either.
pub(crate) fn fxaa_document() -> Graph {
    use wxsl::core::graph::SocketRef;

    let registry = wxsl::core::pipeline::registry();
    let mut document = StockPipeline::Forward.document();
    document.set_name("forward + fxaa");
    let tonemap = document
        .nodes()
        .find(|(_, node)| {
            node.settings
                .get(doc::SETTING_EFFECT)
                .is_some_and(|spelled| {
                    wxsl::core::identity::resolve(spelled).as_ref() == "wxsl.tonemap"
                })
        })
        .map(|(id, _)| id)
        .expect("every stock document ends in the tonemap pass");
    let present = document
        .edge_from(&SocketRef::new(tonemap, "color"))
        .expect("the tonemap presents")
        .to
        .node;

    let encoded = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("encoded")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let fxaa = document.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("fxaa")
            .with_setting(doc::SETTING_EFFECT, "wxsl.fxaa"),
    );
    document.disconnect(&registry, &SocketRef::new(present, "surface"));
    for (from, to) in [
        ((encoded, "color"), (tonemap, "into")),
        ((encoded, "color"), (fxaa, "image")),
        ((fxaa, "color"), (present, "surface")),
    ] {
        document.wire(&registry, from, to).expect("gallery wiring");
    }
    document
}

/// The TAA chain as a document: the shipped deferred preset with a
/// velocity pass and a resolve composed onto it — three nodes, one
/// resource, one rewire, in a demo, because that is what a temporal
/// pipeline *is* now ([plan3 N2]).
///
/// The velocity pass loads the depth the G-buffer pass wrote — the same
/// `depth` output every material pass carries — and writes a `pair`
/// precision colour: motion is a difference, signed, and worth half
/// floats. The resolve is a derived `pass.screen.wxsl.taa` row, whose
/// sockets are its declaration's — `color`, `velocity`, and `history`,
/// the last read a frame back through the ring the resolve itself
/// writes, which is the whole reason the pass orders against the scene
/// and never against itself. The tonemap reads the ring.
pub(crate) fn taa_document() -> Graph {
    use wxsl::core::graph::SocketRef;

    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut document = StockPipeline::Deferred.document();
    document.set_name("deferred + taa");

    // The preset's nodes, by what they are.
    let scene = document
        .nodes()
        .find(|(_, node)| node.def == doc::SOURCE_SCENE)
        .map(|(id, _)| id)
        .expect("the preset draws a scene");
    let material = document
        .nodes()
        .find(|(_, node)| {
            node.def == doc::PASS_GEOMETRY
                && node
                    .settings
                    .get(doc::SETTING_STAGE)
                    .is_some_and(|stage| stage.as_str() == "gbuffer")
        })
        .map(|(id, _)| id)
        .expect("the deferred preset shades a G-buffer");
    let scene_color = document
        .nodes()
        .find(|(_, node)| node.def == doc::RESOURCE_COLOR)
        .map(|(id, _)| id)
        .expect("the deferred preset shades into a colour target");
    let tonemap = document
        .nodes()
        .find(|(_, node)| {
            node.settings
                .get(doc::SETTING_EFFECT)
                .is_some_and(|spelled| {
                    wxsl::core::identity::resolve(spelled).as_ref() == "wxsl.tonemap"
                })
        })
        .map(|(id, _)| id)
        .expect("every stock document ends in the tonemap pass");

    let velocity = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("velocity")
            .with_setting(doc::SETTING_PRECISION, "pair"),
    );
    let motion = document.add(
        wxsl::core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("velocity")
            .with_setting(doc::SETTING_STAGE, "velocity"),
    );
    let ring = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("taa")
            .with_setting(doc::SETTING_PRECISION, "hdr")
            .with_setting(doc::SETTING_HISTORY, "1"),
    );
    let taa = document.add(
        wxsl::core::graph::Node::new(format!("{}wxsl.taa", doc::PASS_SCREEN_PREFIX))
            .with_label("taa"),
    );

    document.disconnect(&registry, &SocketRef::new(tonemap, "image"));
    for (from, to) in [
        ((scene, "draws"), (motion, "draws")),
        ((material, "depth"), (motion, "depth")),
        ((velocity, "color"), (motion, "into")),
        ((scene_color, "color"), (taa, "color")),
        ((velocity, "color"), (taa, "velocity")),
        ((ring, "color"), (taa, "history")),
        ((ring, "color"), (taa, "into")),
        ((ring, "color"), (tonemap, "image")),
    ] {
        document.wire(&registry, from, to).expect("taa wiring");
    }
    document
}

/// The motion-blur chain as a document: the shipped forward preset with a
/// velocity pass and the blur composed between the shading and the
/// display transform. The blur reads *linear* radiance — ADR 0039's rule
/// is the reason it sits before the tonemap — and smears each fragment
/// along its own screen motion, which is what makes the smear follow the
/// object rather than a global direction.
pub(crate) fn motion_blur_document() -> Graph {
    use wxsl::core::graph::SocketRef;

    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut document = StockPipeline::Forward.document();
    document.set_name("forward + motion blur");

    let scene = document
        .nodes()
        .find(|(_, node)| node.def == doc::SOURCE_SCENE)
        .map(|(id, _)| id)
        .expect("the preset draws a scene");
    let prepass = document
        .nodes()
        .find(|(_, node)| {
            node.def == doc::PASS_GEOMETRY
                && node
                    .settings
                    .get(doc::SETTING_STAGE)
                    .is_some_and(|stage| stage.as_str() == "depth_only")
        })
        .map(|(id, _)| id)
        .expect("the forward preset starts with a depth prepass");
    let scene_color = document
        .nodes()
        .find(|(_, node)| node.def == doc::RESOURCE_COLOR)
        .map(|(id, _)| id)
        .expect("the forward preset shades into a colour target");
    let tonemap = document
        .nodes()
        .find(|(_, node)| {
            node.settings
                .get(doc::SETTING_EFFECT)
                .is_some_and(|spelled| {
                    wxsl::core::identity::resolve(spelled).as_ref() == "wxsl.tonemap"
                })
        })
        .map(|(id, _)| id)
        .expect("every stock document ends in the tonemap pass");

    let velocity = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("velocity")
            .with_setting(doc::SETTING_PRECISION, "pair"),
    );
    let motion = document.add(
        wxsl::core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("velocity")
            .with_setting(doc::SETTING_STAGE, "velocity"),
    );
    let blurred = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("blurred")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let blur = document.add(
        wxsl::core::graph::Node::new(format!("{}wxsl.motion_blur", doc::PASS_SCREEN_PREFIX))
            .with_label("motion blur"),
    );
    // The reconstruction's tiles: the longest motion per tile, then
    // around it, which the blur follows.
    let tile = |label: &str| {
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label(label)
            .with_setting(doc::SETTING_PRECISION, "pair")
            .with_setting(doc::SETTING_SCALE, wxsl::render::effect::MOTION_TILES_SCALE)
    };
    let tiles = document.add(tile("motion tiles"));
    let neighbors = document.add(tile("motion neighbours"));
    let tiling = document.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_setting(doc::SETTING_EFFECT, "wxsl.motion_tiles")
            .with_label("motion tiles"),
    );
    let spreading = document.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_setting(doc::SETTING_EFFECT, "wxsl.motion_neighbors")
            .with_label("motion neighbours"),
    );

    document.disconnect(&registry, &SocketRef::new(tonemap, "image"));
    for (from, to) in [
        ((scene, "draws"), (motion, "draws")),
        ((prepass, "depth"), (motion, "depth")),
        ((velocity, "color"), (motion, "into")),
        ((velocity, "color"), (tiling, "image")),
        ((tiles, "color"), (tiling, "into")),
        ((tiles, "color"), (spreading, "image")),
        ((neighbors, "color"), (spreading, "into")),
        ((scene_color, "color"), (blur, "color")),
        ((velocity, "color"), (blur, "velocity")),
        ((prepass, "depth"), (blur, "depth")),
        ((neighbors, "color"), (blur, "neighbors")),
        ((blurred, "color"), (blur, "into")),
        ((blurred, "color"), (tonemap, "image")),
    ] {
        document.wire(&registry, from, to).expect("blur wiring");
    }
    document
}

/// The shipped forward preset with [`doc::PASS_DOF`] composed onto it —
/// depth of field, focused on the middle copy of the row (ADR 0065's
/// second expansion). The prepass's depth is what the circles of
/// confusion measure; the chain writes an HDR target the tonemap reads.
/// The lens' numbers ride the node's settings; the expansion's stable
/// labels — `dof circles`, `dof far blur`, `dof near blur` — are what
/// `set_pass_param` would tune through.
pub(crate) fn dof_document() -> Graph {
    use wxsl::core::graph::SocketRef;
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
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
    let dof = graph.add(wxsl::core::graph::Node::new(doc::PASS_DOF).with_label("dof"));
    let post = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
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
    graph
}
