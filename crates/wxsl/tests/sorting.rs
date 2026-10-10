//! Draw order as data (plan5 D3/D4, ADR 0055).
//!
//! A hologram forced in front of a transparent sheet by `render_order`,
//! whichever order the draw list submitted them in — and, with the pass's
//! sort off, the submission order back in charge. All three surfaces ride
//! one blended pass: the backdrop is the order `-1` group, an underlay.
//! Skips when no adapter is available.

mod probe;

use std::f32::consts::FRAC_PI_2;

use glam::{Mat4, Vec3};
use probe::{color_at, gap, gpu, render_list_in, unlit, Harness};
use wxsl::core::graph::{Graph, Node};
use wxsl::core::material::MaterialConfig;
use wxsl::core::node::Value;
use wxsl::core::pipeline as doc;
use wxsl::core::scene::{Tags, TAG_TRANSPARENT};
use wxsl::render::{compile_pipeline, document_registry, DrawItem, DrawList, Mesh, PipelineConfig};

/// Half-float accumulators, then an 8-bit readback.
const TOLERANCE: f32 = 8.0;

/// The order `-1` backdrop card, the background every expectation
/// composites over.
const BACKDROP: [f32; 3] = [0.2, 0.2, 0.2];

/// One blended pass drawing the `transparent` tag: straight-alpha over,
/// and — with `sorted` — the pass's own back-to-front sort, the order
/// `render_order` rides. Without the sort the pass draws in submission
/// order, unchanged from the day before sorting existed. The backdrop is
/// the order `-1` group, so the expectations below composite three
/// surfaces over `BACKDROP`.
fn order_document(sorted: bool) -> Graph {
    let registry = wxsl::core::pipeline::registry();
    let mut graph = wxsl::core::pipeline::document("render order");
    let scene =
        graph.add(Node::new(doc::SOURCE_SCENE).with_setting(doc::SETTING_TAGS, TAG_TRANSPARENT));
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let mut ordered = Node::new(doc::PASS_GEOMETRY)
        .with_label("ordered")
        .with_setting(doc::SETTING_BLEND, "alpha over");
    if sorted {
        ordered = ordered.with_setting(doc::SETTING_SORT, "back to front");
    }
    let pass = graph.add(ordered);
    let present = graph.add_node(doc::PRESENT);
    for (from, to) in [
        ((scene, "draws"), (pass, "draws")),
        ((depth, "depth"), (pass, "depth")),
        ((pass, "color"), (present, "surface")),
    ] {
        graph
            .wire(&registry, from, to)
            .expect("the order document wires");
    }
    graph
}

/// A flat card of one colour, tagged and ordered: `order` rides the
/// material configuration (plan5 D4). Negative orders are legal — the
/// backdrop is one.
fn card(harness: &Harness, emissive: [f32; 3], alpha: f32, order: i32) -> wxsl::render::Material {
    let mut graph = Graph::new(format!("card {emissive:?} order {order}"));
    let output = graph.add_node(wxsl::core::abi::SURFACE_OUTPUT_ID);
    graph.set_param(output, "emissive", Value::Vec3(emissive));
    graph.set_param(output, "alpha", Value::F32(alpha));
    graph.set_param(output, "base_color", Value::Vec3([0.0, 0.0, 0.0]));
    let config = MaterialConfig::default()
        .with_tags(Tags::from_iter([TAG_TRANSPARENT]))
        .with_shadows(false, false)
        .with_render_order(order);
    wxsl::render::Material::with_config(&graph, &harness.registry, &config)
        .expect("the card compiles")
}

/// Stand the plane up on Z, camera at z = 3; the larger world Z is the
/// nearer surface.
fn placed(z: f32) -> Mat4 {
    Mat4::from_translation(Vec3::new(0.0, 0.0, z)) * Mat4::from_rotation_x(FRAC_PI_2)
}

/// Render the scene: the sheet at z = 0.5, the hologram behind it at
/// z = -0.5, the order `-1` backdrop furthest — `sheet_first` flips the
/// submission order, the thing only the sort is allowed to override.
fn render(harness: &mut Harness, sorted: bool, sheet_first: bool) -> Vec<u8> {
    let document = order_document(sorted);
    let config = PipelineConfig::new(harness.renderer.target());
    let graph = compile_pipeline(
        &document,
        &document_registry(harness.renderer.effects()),
        harness.renderer.effects(),
        &config,
    )
    .expect("the order document compiles");
    harness
        .renderer
        .set_graph(graph)
        .expect("the order pass list runs");

    let mesh = Mesh::plane(&harness.gpu.device, 8.0);
    let sheet = card(harness, [0.0, 1.0, 0.0], 0.5, 0);
    let hologram = card(harness, [1.0, 0.0, 0.0], 0.5, 1);
    let backdrop = card(harness, [0.2, 0.2, 0.2], 1.0, -1);
    let sheet_draw = DrawItem::new(&mesh, &sheet).with_transform(placed(0.5));
    let hologram_draw = DrawItem::new(&mesh, &hologram).with_transform(placed(-0.5));
    let backdrop_draw = DrawItem::new(&mesh, &backdrop).with_transform(placed(-1.8));
    let draws: DrawList = if sheet_first {
        vec![backdrop_draw, sheet_draw, hologram_draw]
    } else {
        vec![backdrop_draw, hologram_draw, sheet_draw]
    }
    .into_iter()
    .collect();
    render_list_in(
        &harness.gpu,
        &mut harness.renderer,
        &harness.target,
        &draws,
        &unlit(),
    )
    .expect("the frame renders")
}

fn assert_colour(image: &[u8], camera: wxsl::render::Camera, expected: [f32; 3], what: &str) {
    let got = color_at(image, camera, Vec3::ZERO);
    let error = gap(
        got,
        [
            expected[0] * 255.0,
            expected[1] * 255.0,
            expected[2] * 255.0,
        ],
    );
    assert!(
        error <= TOLERANCE,
        "{what}: centre {got:?}, expected {expected:?}, gap {error}"
    );
}

/// The hologram's group draws after the sheet's whatever the submission
/// order was (plan5 D4's done-when): order 1 is in front of order 0,
/// alpha-over, and the hologram sits *behind* the sheet in depth — which
/// is why the pass must not write depth while it composites.
#[test]
fn render_order_forces_its_group_in_front_of_the_submission_order() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let camera = unlit().camera;
    // Backdrop 1.0, green 0.5 over it, the hologram's red 0.5 over that
    // — the same picture both ways, because the group order is not the
    // submission's to change.
    let over = |top: [f32; 3], a: f32, under: [f32; 3]| {
        [
            a * top[0] + (1.0 - a) * under[0],
            a * top[1] + (1.0 - a) * under[1],
            a * top[2] + (1.0 - a) * under[2],
        ]
    };
    let sheet = over([0.0, 1.0, 0.0], 0.5, BACKDROP);
    let expected = over([1.0, 0.0, 0.0], 0.5, sheet);
    for sheet_first in [true, false] {
        let image = render(&mut harness, true, sheet_first);
        assert_colour(
            &image,
            camera,
            expected,
            if sheet_first {
                "sorted, sheet submitted first"
            } else {
                "sorted, hologram submitted first"
            },
        );
    }
}

/// With the pass's sort off, the submission order is the composite: two
/// alpha-0.5 draws in each order land on different pictures, and depth —
/// which the pass reads, never writes — rejects nothing.
#[test]
fn without_a_sort_the_submission_order_composites() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let camera = unlit().camera;
    let over = |top: [f32; 3], a: f32, under: [f32; 3]| {
        [
            a * top[0] + (1.0 - a) * under[0],
            a * top[1] + (1.0 - a) * under[1],
            a * top[2] + (1.0 - a) * under[2],
        ]
    };
    // Sheet first: green 0.5 over the backdrop, red 0.5 over that — the
    // sorted pass's own answer, submission order coinciding with it.
    let sheet = over([0.0, 1.0, 0.0], 0.5, BACKDROP);
    let sheet_first = over([1.0, 0.0, 0.0], 0.5, sheet);
    // Hologram first: red 0.5 over the backdrop, green 0.5 over that —
    // a different picture, which is the point.
    let hologram = over([1.0, 0.0, 0.0], 0.5, BACKDROP);
    let hologram_first = over([0.0, 1.0, 0.0], 0.5, hologram);
    let image = render(&mut harness, false, true);
    assert_colour(&image, camera, sheet_first, "unsorted, sheet first");
    let image = render(&mut harness, false, false);
    assert_colour(&image, camera, hologram_first, "unsorted, hologram first");
}
