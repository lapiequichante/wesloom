//! Dual depth peeling (ADR 0047).
//!
//! Four transparent quads, overlapping, submitted out of depth order, over
//! an opaque plane. A fifth transparent sits behind the opaque and must not
//! show. The baseline and, when the device can blend `rg32float`, the native
//! path both have to land on the front-to-back composite of those four
//! layers. Skips when no adapter is available.

mod probe;

use std::f32::consts::FRAC_PI_2;

use glam::{Mat4, Vec3};
use probe::{color_at, gap, gpu, render_list_in, unlit, Harness, SIZE};
use wxsl::core::abi::{self, PEEL_LAYERS_MACRO, PEEL_NATIVE_MACRO};
use wxsl::core::graph::{Graph, Node};
use wxsl::core::macros::MacroValue;
use wxsl::core::material::MaterialConfig;
use wxsl::core::node::Value;
use wxsl::core::pipeline as doc;
use wxsl::core::scene::{Tags, TAG_OPAQUE, TAG_TRANSPARENT};
use wxsl::render::{
    compile_pipeline, document_registry, Camera, DrawItem, DrawList, Mesh, PipelineConfig,
};

/// Front-to-back over gray 0.2 of white, blue, green, red, each at alpha 0.5.
///
/// Painter's order from the back: the dual-peel composite has to match it
/// whatever order the draw list used.
const EXPECTED: [f32; 3] = [0.575, 0.325, 0.2];

/// Half-float accumulators, then an 8-bit readback. A few levels, not a
/// different composite.
const TOLERANCE: f32 = 8.0;

fn peel_document() -> Graph {
    let registry = wxsl::core::pipeline::registry();
    let mut graph = wxsl::core::pipeline::document("peel");
    let scene = graph.add(Node::new(doc::SOURCE_SCENE).with_setting(doc::SETTING_TAGS, TAG_OPAQUE));
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let color = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let shade = graph.add(Node::new(doc::PASS_GEOMETRY).with_label("opaque"));
    let peel = graph.add(Node::new(doc::PASS_PEEL).with_label("peel"));
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
    graph
}

/// The tiered document (plan5 D5): the peel, then a sorted-tier pass that
/// composites the `max_layers = 0` transparents over the peel composite,
/// back to front, straight-alpha blended. The sorted pass loads the frame
/// target — clearing it would throw the peel composite away — and its
/// declaration after the peel is what puts it in front.
fn tiered_peel_document() -> Graph {
    let registry = wxsl::core::pipeline::registry();
    let mut graph = wxsl::core::pipeline::document("tiered peel");
    let scene = graph.add(Node::new(doc::SOURCE_SCENE).with_setting(doc::SETTING_TAGS, TAG_OPAQUE));
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let color = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let shade = graph.add(Node::new(doc::PASS_GEOMETRY).with_label("opaque"));
    let peel = graph.add(Node::new(doc::PASS_PEEL).with_label("peel"));
    // A pass draws what its scene source selects: the sorted tier gets
    // its own source, selecting the transparent tag.
    let transparents =
        graph.add(Node::new(doc::SOURCE_SCENE).with_setting(doc::SETTING_TAGS, TAG_TRANSPARENT));
    let sorted = graph.add(
        Node::new(doc::PASS_GEOMETRY)
            .with_label("sorted transparents")
            .with_setting(doc::SETTING_LAYERS, "sorted")
            .with_setting(doc::SETTING_SORT, "back to front")
            .with_setting(doc::SETTING_BLEND, "alpha over"),
    );
    let present = graph.add_node(doc::PRESENT);
    for (from, to) in [
        ((scene, "draws"), (shade, "draws")),
        ((depth, "depth"), (shade, "depth")),
        ((color, "color"), (shade, "into")),
        ((shade, "depth"), (peel, "depth")),
        ((shade, "color"), (peel, "scene")),
        ((shade, "depth"), (sorted, "depth")),
        ((transparents, "draws"), (sorted, "draws")),
        ((sorted, "color"), (present, "surface")),
    ] {
        graph
            .wire(&registry, from, to)
            .expect("the tiered document wires");
    }
    graph
}

fn config(renderer: &wxsl::render::Renderer, native: bool) -> PipelineConfig {
    let mut config = PipelineConfig::new(renderer.target());
    config.macros.set(PEEL_LAYERS_MACRO, MacroValue::Int(4));
    if native {
        config.macros.set(PEEL_NATIVE_MACRO, MacroValue::Flag(true));
    }
    config
}

/// `max_layers` is the peel budget (plan5 D5): the classic four-layer
/// scene asks for the pipeline's whole cap, so every surface is in every
/// iteration and the composite is what ADR 0047 peeled.
fn surface(
    harness: &Harness,
    emissive: [f32; 3],
    alpha: f32,
    tag: &str,
    max_layers: i32,
) -> wxsl::render::Material {
    let mut graph = Graph::new(format!("{tag} {emissive:?}"));
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.set_param(output, "emissive", Value::Vec3(emissive));
    graph.set_param(output, "alpha", Value::F32(alpha));
    graph.set_param(output, "base_color", Value::Vec3([0.0, 0.0, 0.0]));
    let config = MaterialConfig::default()
        .with_tags(Tags::from_iter([tag]))
        .with_shadows(false, false)
        .with_max_layers(max_layers);
    wxsl::render::Material::with_config(&graph, &harness.registry, &config)
        .expect("the surface compiles")
}

/// Stand the plane up on Z and move it. Nearer to the camera at z = 3 is
/// the larger world Z.
fn placed(z: f32) -> Mat4 {
    Mat4::from_translation(Vec3::new(0.0, 0.0, z)) * Mat4::from_rotation_x(FRAC_PI_2)
}

fn render(harness: &mut Harness, native: bool) -> Vec<u8> {
    let document = peel_document();
    let graph = compile_pipeline(
        &document,
        &document_registry(harness.renderer.effects()),
        harness.renderer.effects(),
        &config(&harness.renderer, native),
    )
    .expect("the peel document compiles");
    harness
        .renderer
        .set_graph(graph)
        .expect("the peel pass list runs");

    let mesh = Mesh::plane(&harness.gpu.device, 8.0);
    // Not in depth order. A peel that just blends in submission order
    // cannot hit EXPECTED.
    let surfaces = [
        (
            surface(
                harness,
                [1.0, 0.0, 1.0],
                0.5,
                TAG_TRANSPARENT,
                abi::PEEL_LAYERS_MAX,
            ),
            -1.8,
        ),
        (
            surface(
                harness,
                [0.0, 1.0, 0.0],
                0.5,
                TAG_TRANSPARENT,
                abi::PEEL_LAYERS_MAX,
            ),
            0.8,
        ),
        (surface(harness, [0.2, 0.2, 0.2], 1.0, TAG_OPAQUE, 0), -1.2),
        (
            surface(
                harness,
                [1.0, 1.0, 1.0],
                0.5,
                TAG_TRANSPARENT,
                abi::PEEL_LAYERS_MAX,
            ),
            -0.6,
        ),
        (
            surface(
                harness,
                [1.0, 0.0, 0.0],
                0.5,
                TAG_TRANSPARENT,
                abi::PEEL_LAYERS_MAX,
            ),
            1.5,
        ),
        (
            surface(
                harness,
                [0.0, 0.0, 1.0],
                0.5,
                TAG_TRANSPARENT,
                abi::PEEL_LAYERS_MAX,
            ),
            0.1,
        ),
    ];
    let items: Vec<DrawItem> = surfaces
        .iter()
        .map(|(material, z)| DrawItem::new(&mesh, material).with_transform(placed(*z)))
        .collect();
    let draws: DrawList = items.into_iter().collect();
    let environment = unlit();
    render_list_in(
        &harness.gpu,
        &mut harness.renderer,
        &harness.target,
        &draws,
        &environment,
    )
    .expect("the frame renders")
}

fn assert_composite(image: &[u8], camera: Camera, what: &str) {
    let got = color_at(image, camera, Vec3::ZERO);
    let expected = [
        EXPECTED[0] * 255.0,
        EXPECTED[1] * 255.0,
        EXPECTED[2] * 255.0,
    ];
    let error = gap(got, expected);
    assert!(
        error <= TOLERANCE,
        "{what}: centre {got:?}, analytical {expected:?}, gap {error} (image {SIZE}px)"
    );
    let (x, y) = probe::pixel_of(camera, Vec3::ZERO);
    let pixel = probe::pixel(image, x, y);
    assert_eq!(pixel[3], 255, "{what}: the composite is opaque");
}

#[test]
fn four_transparent_layers_composite_over_the_opaque() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let camera = unlit().camera;

    let baseline = render(&mut harness, false);
    assert_composite(&baseline, camera, "baseline");

    if !harness
        .gpu
        .caps
        .has(wxsl::render::wgpu::Features::FLOAT32_BLENDABLE)
    {
        eprintln!("skipping the native peel: the device has no FLOAT32_BLENDABLE");
        return;
    }
    let native = render(&mut harness, true);
    assert_composite(&native, camera, "native");
    let drift = probe::patch_gap(&baseline, &native, camera, Vec3::ZERO, 2);
    assert!(
        drift <= TOLERANCE,
        "baseline and native disagree by {drift}"
    );
}

/// The tiers compose (plan5 D5): two interpenetrating transparents ask
/// for peeling, and a `max_layers = 0` plate behind them is nobody the
/// peel ever reads — it composites over the peel composite through the
/// sorted-tier pass, in submission order that the sort could reverse.
#[test]
fn a_sorted_transparent_composites_over_the_peeled_pair() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let camera = unlit().camera;

    for native in [false, true] {
        if native
            && !harness
                .gpu
                .caps
                .has(wxsl::render::wgpu::Features::FLOAT32_BLENDABLE)
        {
            eprintln!("skipping the native peel: the device has no FLOAT32_BLENDABLE");
            return;
        }
        let document = tiered_peel_document();
        let graph = compile_pipeline(
            &document,
            &document_registry(harness.renderer.effects()),
            harness.renderer.effects(),
            &config(&harness.renderer, native),
        )
        .unwrap_or_else(|error| panic!("the tiered document does not compile: {error}"));
        harness
            .renderer
            .set_graph(graph)
            .expect("the tiered pass list runs");

        let mesh = Mesh::plane(&harness.gpu.device, 8.0);
        // Submitted plate first: only the sorted tier's own sort could
        // put it behind the peeled pair's composite.
        let surfaces = [
            // The plate: the sorted tier, behind everything.
            (
                surface(&harness, [0.0, 1.0, 0.0], 0.5, TAG_TRANSPARENT, 0),
                -0.6,
            ),
            // White and blue, crossing through each other at the centre.
            (
                surface(
                    &harness,
                    [1.0, 1.0, 1.0],
                    0.5,
                    TAG_TRANSPARENT,
                    abi::PEEL_LAYERS_MAX,
                ),
                0.9,
            ),
            (
                surface(
                    &harness,
                    [0.0, 0.0, 1.0],
                    0.5,
                    TAG_TRANSPARENT,
                    abi::PEEL_LAYERS_MAX,
                ),
                0.4,
            ),
            (surface(&harness, [0.2, 0.2, 0.2], 1.0, TAG_OPAQUE, 0), -1.2),
        ];
        let items: Vec<DrawItem> = surfaces
            .iter()
            .map(|(material, z)| DrawItem::new(&mesh, material).with_transform(placed(*z)))
            .collect();
        let draws: DrawList = items.into_iter().collect();
        let image = render_list_in(
            &harness.gpu,
            &mut harness.renderer,
            &harness.target,
            &draws,
            &unlit(),
        )
        .expect("the frame renders");

        // Gray 0.2, then white over blue (the peel, front to back), then
        // the plate over that (the sorted tier).
        let expected = [
            0.5 * 0.0 + 0.5 * 0.55,
            0.5 * 1.0 + 0.5 * 0.55,
            0.5 * 0.0 + 0.5 * 0.8,
        ];
        let got = color_at(&image, camera, Vec3::ZERO);
        let error = gap(
            got,
            [
                expected[0] * 255.0,
                expected[1] * 255.0,
                expected[2] * 255.0,
            ],
        );
        let what = if native { "native" } else { "baseline" };
        assert!(
            error <= TOLERANCE,
            "{what}: centre {got:?}, analytical {expected:?}, gap {error}"
        );
    }
}

/// The budget is per object, and it is the *draw* count that shows it
/// (plan5 D5's done-when): ten `max_layers = 0` transparents ride the
/// sorted tier and no peel pass reads any of them, a pair asks for two
/// layers, and the peel passes carry the pair for exactly those two
/// iterations and nobody for the rest. The passes still *run* — the
/// budget is a selection, not a skip — so the count, not the run count,
/// is where the cost went.
#[test]
fn the_peel_carries_its_budgets_and_the_sorted_tier_carries_the_rest() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let document = tiered_peel_document();
    let graph = compile_pipeline(
        &document,
        &document_registry(harness.renderer.effects()),
        harness.renderer.effects(),
        &config(&harness.renderer, false),
    )
    .expect("the tiered document compiles");
    harness
        .renderer
        .set_graph(graph)
        .expect("the tiered pass list runs");

    let mesh = Mesh::plane(&harness.gpu.device, 8.0);
    // Materials live longer than the draw list, so they are collected
    // first: ten sorted-tier plates, the two-layer pair, one opaque.
    let plates: Vec<wxsl::render::Material> = (0..10)
        .map(|index| {
            surface(
                &harness,
                [0.05 + index as f32 * 0.01, 0.1, 0.1],
                0.25,
                TAG_TRANSPARENT,
                0,
            )
        })
        .collect();
    let white = surface(&harness, [1.0, 1.0, 1.0], 0.5, TAG_TRANSPARENT, 2);
    let blue = surface(&harness, [0.0, 0.0, 1.0], 0.5, TAG_TRANSPARENT, 2);
    let opaque = surface(&harness, [0.2, 0.2, 0.2], 1.0, TAG_OPAQUE, 0);
    let mut items: Vec<DrawItem> = Vec::new();
    for (index, material) in plates.iter().enumerate() {
        items.push(
            DrawItem::new(&mesh, material).with_transform(placed(-0.5 + index as f32 * 0.09)),
        );
    }
    items.push(DrawItem::new(&mesh, &white).with_transform(placed(0.9)));
    items.push(DrawItem::new(&mesh, &blue).with_transform(placed(0.4)));
    items.push(DrawItem::new(&mesh, &opaque).with_transform(placed(-1.2)));
    let draws: DrawList = items.into_iter().collect();
    render_list_in(
        &harness.gpu,
        &mut harness.renderer,
        &harness.target,
        &draws,
        &unlit(),
    )
    .expect("the frame renders");

    let drawn = |label: &str| {
        harness
            .renderer
            .pass_draw_count(label)
            .unwrap_or_else(|| panic!("no pass labelled `{label}`"))
    };
    // The sorted tier drew all ten plates; the opaque pass, its one.
    assert_eq!(drawn("sorted transparents"), 10, "the sorted tier");
    // The pair's two layers, and nobody else's — the ten plates are
    // in none of them.
    assert_eq!(drawn("peel front depth 0"), 2, "the pair's first layer");
    assert_eq!(drawn("peel front shade 0"), 2, "the pair's first layer");
    assert_eq!(drawn("peel back shade 1"), 2, "the pair's second layer");
    // The budget spent: iterations two and three draw nobody — the
    // per-object half of the clamp, visible instead of paid.
    assert_eq!(drawn("peel front depth 2"), 0, "beyond the pair's budget");
    assert_eq!(drawn("peel back shade 3"), 0, "beyond the pair's budget");
    // And the passes ran anyway: skipping is the policy's decision, not
    // the filter's.
    assert_eq!(
        harness.renderer.pass_run_count("peel front shade 2"),
        Some(1),
        "an emptied peel iteration still runs"
    );
}
