//! The velocity stage and its first two consumers, on a real device
//! (plan3 N2).
//!
//! Four questions, each with a picture that answers it:
//!
//! * the velocity target holds the screen motion the frame actually saw —
//!   read back through a graph-authored display effect, against the
//!   projection the test computes itself;
//! * the previous-frame rows are *previous*, the camera's half included —
//!   which is `previous_camera`'s whole reason to exist;
//! * a spinning cube settles under TAA: the frame-to-frame difference
//!   drops while the image keeps up with the scene, which is "no edge
//!   shimmer" as a measurement instead of a squint;
//! * and a swept cube smears along its own motion, which is the blur
//!   done by the object and not by a global direction.

use std::f32::consts::FRAC_PI_2;

use glam::{Mat4, Vec2, Vec3, Vec4};
use wxsl::core::abi;
use wxsl::core::graph::{Graph, Node, SocketRef};
use wxsl::core::node::{GraphDomain, Value};
use wxsl::core::pipeline as doc;
use wxsl::render::{
    Camera, DrawItem, Effect, EffectRegistry, Environment, Light, Material, Mesh, PipelineConfig,
    Renderer, TargetConfig,
};

mod probe;
use probe::{gpu, pixel, render_list_in, Harness, SIZE};

/// The frame step every previous-frame transform in these tests answers
/// for — a screenshot's shutter, since a test has no real frame times.
const STEP: f32 = 1.0 / 60.0;

/// The test camera: on the +z axis, looking at the origin, so a plane
/// tipped up to face it covers the middle of the frame and one sample
/// point answers for the surface.
fn camera() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 0.0, 3.0),
        target: Vec3::ZERO,
        ..Camera::default()
    }
}

fn environment(previous: Option<Camera>, time: f32) -> Environment {
    let mut environment = Environment {
        camera: camera(),
        lights: vec![
            Light::directional(Vec3::new(-0.4, 0.7, -1.0), Vec3::ONE, 1.5),
            Light::point(Vec3::new(2.0, 2.0, 2.0), Vec3::ONE, 12.0),
        ],
        ambient_sky: Vec3::new(0.2, 0.2, 0.25),
        ambient_ground: Vec3::new(0.05, 0.04, 0.04),
        exposure: 1.0,
        time,
        previous_time: time,
        previous_camera: previous,
    };
    environment.advance(time + STEP);
    environment
}

/// A material of no interest: plain red. The velocity stage shades
/// nothing, so the more boring the surface the clearer what the test is
/// about.
fn plain_material(harness: &Harness) -> Material {
    let mut graph = Graph::new("plain");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.set_param(output, "emissive", Value::Vec3([0.7, 0.1, 0.1]));
    harness.material(&graph)
}

/// Where a world point lands, in uv with `y` down — the same orientation
/// the velocity buffer is written in, and the reason a reprojected uv is
/// `uv - motion` and nothing more.
fn uv_of(view_proj: Mat4, world: Vec3) -> Vec2 {
    let clip = view_proj * Vec4::new(world.x, world.y, world.z, 1.0);
    let ndc = clip.truncate() / clip.w;
    Vec2::new(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5)
}

/// The display effect a test reads the velocity buffer through: the two
/// motion channels scaled and biased into colour, authored as a screen
/// graph — the domain's whole point, and the reason a test needs no
/// debug-view effect shipped for it.
fn velocity_view_effect(registry: &wxsl::core::node::NodeRegistry) -> Effect {
    // Chosen so one velocity quantum clears the 8-bit step with room to
    // spare: a frame's motion is a few thousandths of a uv, times twenty
    // is a tenth of the range, and an eight-bit step is a 1/255th of it.
    const SCALE: f32 = 40.0;
    let mut graph = Graph::in_domain("velocity view", GraphDomain::Screen);
    let image = graph.add_node(abi::SCREEN_IMAGE_ID);
    let uv = graph.add_node(abi::context_node_id("uv"));
    let load = graph.add_node("sample.load_2d");
    let split = graph.add_node("convert.split.vec4f");
    let color = graph.add_node("convert.combine.vec3f");
    let out = graph.add_node(abi::SCREEN_OUTPUT_ID);
    let wire = |graph: &mut Graph,
                from: (wxsl::core::graph::NodeId, &str),
                to: (wxsl::core::graph::NodeId, &str)| {
        graph
            .wire(registry, from, to)
            .expect("the display graph is wired wrong");
    };
    wire(&mut graph, (image, "out"), (load, "tex"));
    wire(&mut graph, (uv, "out"), (load, "uv"));
    wire(&mut graph, (load, "out"), (split, "v"));
    // One channel at a time — scale, then bias to mid-grey — so the
    // *sign* of the motion is readable and not just its size. The third
    // component stays at the combine node's zero default: the buffer's
    // blue channel is nothing.
    for channel in ["x", "y"] {
        let scale = graph.add(Node::new("math.multiply").with_param("b", Value::F32(SCALE)));
        let bias = graph.add(Node::new("math.add").with_param("b", Value::F32(0.5)));
        wire(&mut graph, (split, channel), (scale, "a"));
        wire(&mut graph, (scale, "out"), (bias, "a"));
        wire(&mut graph, (bias, "out"), (color, channel));
    }
    wire(&mut graph, (color, "out"), (out, abi::SOCKET_SCREEN_COLOR));
    wire(&mut graph, (split, "w"), (out, abi::SOCKET_SCREEN_ALPHA));
    Effect::from_graph(
        "probe.velocity_view",
        "velocity view",
        "Draw a velocity buffer as 0.5 + motion * 40 per channel.",
        graph,
        registry,
    )
    .expect("the display graph generates")
}

/// The motion probe's pipeline: a velocity pass over a depth target of
/// its own, the display effect over it, present. The smallest document
/// that makes the stage observable.
fn velocity_document() -> Graph {
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut graph = wxsl::core::pipeline::document("velocity probe");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let velocity = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("velocity")
            .with_setting(doc::SETTING_PRECISION, "pair"),
    );
    let motion = graph.add(
        Node::new(doc::PASS_GEOMETRY)
            .with_label("velocity")
            .with_setting(doc::SETTING_STAGE, "velocity"),
    );
    let display = graph
        .add(Node::new(doc::PASS_SCREEN).with_setting(doc::SETTING_EFFECT, "probe.velocity_view"));
    let present = graph.add_node(doc::PRESENT);
    for (from, to) in [
        ((scene, "draws"), (motion, "draws")),
        ((depth, "depth"), (motion, "depth")),
        ((velocity, "color"), (motion, "into")),
        ((velocity, "color"), (display, "image")),
        ((display, "color"), (present, "surface")),
    ] {
        graph.wire(&registry, from, to).expect("the probe's wiring");
    }
    graph
}

/// A plane facing the camera at `now`, which was at `before` last frame —
/// the previous transform is the velocity stage's whole input, so the
/// draw states it even for a still.
fn plane<'a>(
    harness: &'a Harness,
    material: &'a Material,
    now: Vec3,
    before: Vec3,
) -> wxsl::render::DrawList<'a> {
    let stand = Mat4::from_rotation_x(FRAC_PI_2);
    let item = DrawItem::new(&harness.mesh, material)
        .with_transform(stand * Mat4::from_translation(now))
        .with_previous(stand * Mat4::from_translation(before));
    wxsl::render::single_draw(item)
}

/// The renderer's twin, for a test that wants the harness's device but a
/// pass list and an effect set of its own.
fn renderer_for(harness: &Harness) -> Renderer {
    Renderer::new(
        &harness.gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, harness.target.format()),
    )
    .expect("the stdlib library satisfies the ABI")
}

/// Compile `document` onto `renderer` and hand the graph back.
fn run_document(renderer: &mut Renderer, document: &Graph) {
    let graph = wxsl::render::compile_pipeline(
        document,
        &wxsl::render::document_registry(renderer.effects()),
        renderer.effects(),
        &PipelineConfig::new(renderer.target()),
    )
    .expect("the document compiles");
    renderer.set_graph(graph).expect("the pass list runs");
}

/// Read the motion vector the display effect left at the projection of
/// `world`, against the expected one.
fn assert_motion(image: &[u8], view_proj: Mat4, world: Vec3, expected: Vec2) {
    let at = uv_of(view_proj, world) * Vec2::new(SIZE as f32, SIZE as f32);
    let seen = pixel(image, at.x as u32, at.y as u32);
    // The display wrote `0.5 + motion * 40`; undo it, quantization and
    // all — one eight-bit step is `40 / 255` of nothing in a test whose
    // signals are a few thousandths.
    let read = Vec2::new(
        (seen[0] as f32 / 255.0 - 0.5) / 40.0,
        (seen[1] as f32 / 255.0 - 0.5) / 40.0,
    );
    let error = (read - expected).abs();
    assert!(
        error.x < 4e-3 && error.y < 4e-3,
        "the buffer says {read:?}, the projection says {expected:?} (pixel {seen:?})"
    );
}

#[test]
fn a_velocity_pass_reports_the_screen_motion_it_sees() {
    let Some(gpu) = gpu() else { return };
    let harness = probe::Harness::new(gpu);
    let material = plain_material(&harness);
    let mut renderer = renderer_for(&harness);
    renderer.add_effect(velocity_view_effect(&harness.registry));
    run_document(&mut renderer, &velocity_document());

    // One unit per second along +x, so one frame's motion is a known
    // world-space step and the projection turns it into a known screen
    // step.
    let now = Vec3::new(0.25, 0.0, 0.0);
    let before = now - Vec3::new(STEP, 0.0, 0.0);
    let view_proj = camera().view_proj();

    // First, the still case: a plane whose previous frame's place *is*
    // this frame's must read exactly no motion.
    let still = plane(&harness, &material, now, now);
    let still_image = render_list_in(
        &harness.gpu,
        &mut renderer,
        &harness.target,
        &still,
        &environment(None, 0.0),
    )
    .expect("the still frame renders");
    assert_motion(&still_image, view_proj, now, Vec2::ZERO);

    let draws = plane(&harness, &material, now, before);
    let image = render_list_in(
        &harness.gpu,
        &mut renderer,
        &harness.target,
        &draws,
        &environment(None, 0.0),
    )
    .expect("the frame renders");

    // The plane's centre, where the interpolated velocity is exactly the
    // vertex value and the projection is the test's own.
    let expected = uv_of(view_proj, now) - uv_of(view_proj, before);
    assert!(expected.x > 0.0, "a rightward motion is positive in uv");
    assert_motion(&image, view_proj, now, expected);
}

#[test]
fn the_previous_camera_is_velocity_too() {
    // A still scene under a camera that moved: the motion vector is the
    // camera's half, which is why `previous_camera` exists — without it
    // every frame would claim the world stood still while the eye moved.
    let Some(gpu) = gpu() else { return };
    let harness = probe::Harness::new(gpu);
    let material = plain_material(&harness);
    let mut renderer = renderer_for(&harness);
    renderer.add_effect(velocity_view_effect(&harness.registry));
    run_document(&mut renderer, &velocity_document());

    let before = camera();
    let mut now = camera();
    now.eye = Vec3::new(0.35, 0.0, 3.0);
    let draws = plane(&harness, &material, Vec3::ZERO, Vec3::ZERO);
    let mut scene = environment(None, 0.0);
    scene.camera = now;
    scene.previous_camera = Some(before);
    let image = render_list_in(&harness.gpu, &mut renderer, &harness.target, &draws, &scene)
        .expect("the frame renders");

    let expected = uv_of(now.view_proj(), Vec3::ZERO) - uv_of(before.view_proj(), Vec3::ZERO);
    assert_motion(&image, now.view_proj(), Vec3::ZERO, expected);
}

/// The TAA chain: the deferred preset with a velocity pass and the
/// resolve composed on — the gallery's `taa` document, as a test.
fn taa_document() -> Graph {
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut document = wxsl::render::StockPipeline::Deferred.document();
    document.set_name("deferred + taa");
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
        Node::new(doc::RESOURCE_COLOR)
            .with_label("velocity")
            .with_setting(doc::SETTING_PRECISION, "pair"),
    );
    let motion = document.add(
        Node::new(doc::PASS_GEOMETRY)
            .with_label("velocity")
            .with_setting(doc::SETTING_STAGE, "velocity"),
    );
    let ring = document.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("taa")
            .with_setting(doc::SETTING_PRECISION, "hdr")
            .with_setting(doc::SETTING_HISTORY, "1"),
    );
    let taa =
        document.add(Node::new(format!("{}wxsl.taa", doc::PASS_SCREEN_PREFIX)).with_label("taa"));
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

/// The deferred preset plus *only* the velocity pass: the bisect build.
/// The deferred preset plus *only* the velocity pass: the bisect build —
/// what a document between the material pass and the lighting one changes
/// about the lit frame, which the test below asserts is nothing.
fn velocity_only_document() -> Graph {
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut document = wxsl::render::StockPipeline::Deferred.document();
    document.set_name("deferred + velocity");
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
        Node::new(doc::RESOURCE_COLOR)
            .with_label("velocity")
            .with_setting(doc::SETTING_PRECISION, "pair"),
    );
    let motion = document.add(
        Node::new(doc::PASS_GEOMETRY)
            .with_label("velocity")
            .with_setting(doc::SETTING_STAGE, "velocity"),
    );
    document.disconnect(&registry, &SocketRef::new(tonemap, "image"));
    for (from, to) in [
        ((scene, "draws"), (motion, "draws")),
        ((material, "depth"), (motion, "depth")),
        ((velocity, "color"), (motion, "into")),
        ((scene_color, "color"), (tonemap, "image")),
    ] {
        document.wire(&registry, from, to).expect("bisect wiring");
    }
    document
}

/// How far two frames sit apart, as the mean absolute difference of their
/// colour channels. The shimmer metric: a cube whose edges crawl makes
/// consecutive frames disagree at every edge, and temporal accumulation
/// is exactly the thing that makes them stop.
fn mean_difference(a: &[u8], b: &[u8]) -> f32 {
    let sum: f32 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs() as f32)
        .sum();
    sum / a.len() as f32
}

/// Where the spinning cube is at `time`.
fn spin(time: f32) -> Mat4 {
    Mat4::from_rotation_y(time * 0.9)
}

/// Where the still cube is, which is where it always was.
fn still(_time: f32) -> Mat4 {
    Mat4::IDENTITY
}

/// Run `frames` steps of the clock, returning the last two frames it
/// produced — the pair a temporal judgement is made on. `motion` says how
/// the cube moves; the previous-frame transform always answers for one
/// `STEP` back.
fn settle(
    harness: &Harness,
    renderer: &mut Renderer,
    mesh: &Mesh,
    material: &Material,
    motion: fn(f32) -> Mat4,
    frames: u32,
) -> (Vec<u8>, Vec<u8>) {
    let mut penultimate = Vec::new();
    let mut final_ = Vec::new();
    for frame in 0..frames {
        let time = frame as f32 * STEP;
        let draws = wxsl::render::single_draw(
            DrawItem::new(mesh, material)
                .with_transform(motion(time))
                .with_previous(motion(time - STEP)),
        );
        final_ = render_list_in(
            &harness.gpu,
            renderer,
            &harness.target,
            &draws,
            &environment(None, time),
        )
        .expect("the frame renders");
        if frame + 2 == frames {
            penultimate = final_.clone();
        }
    }
    (penultimate, final_)
}

#[test]
fn taa_settles_a_spinning_cube() {
    // The plan's bar, as a measurement: the spinning cube under the
    // resolve moves less from frame to frame than the raw chain's — the
    // edges stop crawling — while staying near what the raw chain drew of
    // the same instant, which is "settled", not "frozen".
    let Some(gpu) = gpu() else { return };
    let harness = probe::Harness::new(gpu);
    let material = plain_material(&harness);
    let mesh = Mesh::cube(&harness.gpu.device, 1.2);

    let mut raw = renderer_for(&harness);
    run_document(&mut raw, &wxsl::render::StockPipeline::Deferred.document());
    let (raw_then, raw_now) = settle(&harness, &mut raw, &mesh, &material, spin, 32);

    let mut taa = renderer_for(&harness);
    run_document(&mut taa, &taa_document());
    let (taa_then, taa_now) = settle(&harness, &mut taa, &mesh, &material, spin, 32);

    let raw_jitter = mean_difference(&raw_then, &raw_now);
    let taa_jitter = mean_difference(&taa_then, &taa_now);
    println!("frame-to-frame: raw {raw_jitter:.5}, taa {taa_jitter:.5}");
    assert!(
        raw_jitter > 0.1,
        "the spinning cube should be visibly moving frame to frame: {raw_jitter}"
    );
    assert!(
        taa_jitter < raw_jitter,
        "the resolve should settle what the raw chain crawls: {taa_jitter} vs {raw_jitter}"
    );
    let lag = mean_difference(&taa_now, &raw_now);
    println!("tracked: |taa - raw| = {lag:.5}");
    assert!(
        lag < 0.08,
        "the resolve must track the scene, not accumulate a ghost: {lag}"
    );
}

#[test]
fn taa_accepts_a_still_scene_without_inventing_motion() {
    // The resolve's degenerate case, which it must get exactly right: a
    // scene that does not move has no velocity, so the history and the
    // current frame agree, and the resolve converges to the raw chain —
    // nothing dark seeded in by the unwritten first frame's ring, and no
    // crawl.
    let Some(gpu) = gpu() else { return };
    let harness = probe::Harness::new(gpu);
    let material = plain_material(&harness);
    let mesh = Mesh::cube(&harness.gpu.device, 1.2);

    let mut raw = renderer_for(&harness);
    run_document(&mut raw, &wxsl::render::StockPipeline::Deferred.document());
    let (raw_then, raw_now) = settle(&harness, &mut raw, &mesh, &material, still, 16);

    let mut taa = renderer_for(&harness);
    run_document(&mut taa, &taa_document());
    let (taa_then, taa_now) = settle(&harness, &mut taa, &mesh, &material, still, 16);

    assert_eq!(
        mean_difference(&raw_then, &raw_now),
        0.0,
        "the still scene does not move"
    );
    let jitter = mean_difference(&taa_then, &taa_now);
    assert!(
        jitter < 0.02,
        "a still scene under the resolve stays still: {jitter}"
    );
    let lag = mean_difference(&taa_now, &raw_now);
    assert!(
        lag < 0.05,
        "the resolve converges to the scene it was handed: {lag}"
    );
}

#[test]
fn a_velocity_pass_leaves_the_lit_frame_untouched() {
    // The velocity pass sits *between* the material pass and the lighting
    // one — it loads the same depth, writes its own target — and none of
    // that may show in the lit image. The whole taa chain, with the
    // resolve running beside a presentation that ignores it, against the
    // plain preset.
    let Some(gpu) = gpu() else { return };
    let harness = probe::Harness::new(gpu);
    let material = plain_material(&harness);
    let mesh = Mesh::cube(&harness.gpu.device, 1.2);

    let render_at = |renderer: &mut Renderer, time: f32| {
        let draws = wxsl::render::single_draw(
            DrawItem::new(&mesh, &material)
                .with_transform(spin(time))
                .with_previous(spin(time - STEP)),
        );
        render_list_in(
            &harness.gpu,
            renderer,
            &harness.target,
            &draws,
            &environment(None, time),
        )
        .expect("the frame renders")
    };
    let mut plain = renderer_for(&harness);
    run_document(
        &mut plain,
        &wxsl::render::StockPipeline::Deferred.document(),
    );
    let mut motion = renderer_for(&harness);
    run_document(&mut motion, &velocity_only_document());

    let difference = mean_difference(&render_at(&mut plain, 0.4), &render_at(&mut motion, 0.4));
    println!("deferred vs deferred+velocity: {difference:.5}");
    assert!(difference < 0.5, "{difference}");
}

/// The blur chain: the forward preset with a velocity pass and the blur
/// composed between the shading and the display transform.
fn blur_document(blurred: bool) -> Graph {
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut document = wxsl::render::StockPipeline::Forward.document();
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
    if blurred {
        let velocity = document.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_label("velocity")
                .with_setting(doc::SETTING_PRECISION, "pair"),
        );
        let motion = document.add(
            Node::new(doc::PASS_GEOMETRY)
                .with_label("velocity")
                .with_setting(doc::SETTING_STAGE, "velocity"),
        );
        let target = document.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_label("blurred")
                .with_setting(doc::SETTING_PRECISION, "hdr"),
        );
        let blur = document.add(
            Node::new(format!("{}wxsl.motion_blur", doc::PASS_SCREEN_PREFIX))
                .with_label("motion blur"),
        );
        document.disconnect(&registry, &SocketRef::new(tonemap, "image"));
        for (from, to) in [
            ((scene, "draws"), (motion, "draws")),
            ((prepass, "depth"), (motion, "depth")),
            ((velocity, "color"), (motion, "into")),
            ((scene_color, "color"), (blur, "color")),
            ((velocity, "color"), (blur, "velocity")),
            ((target, "color"), (blur, "into")),
            ((target, "color"), (tonemap, "image")),
        ] {
            document.wire(&registry, from, to).expect("blur wiring");
        }
    }
    document
}

/// Where the swept cube is at `time`: a traverse whose peak screen
/// velocity is several texels a frame at the probe's 64-pixel resolution,
/// which is what makes the smear visible in a readback at all.
fn sweep(time: f32) -> Mat4 {
    Mat4::from_translation(Vec3::new((time * 18.0).sin() * 1.2, 0.0, 0.0))
}

/// A checkered material: the blur test needs structure *on* the cube —
/// smearing a flat colour proves nothing, because the interior averages
/// onto itself and only the silhouette moves.
fn checkered_material(harness: &Harness) -> (Material, wgpu::TextureView, wgpu::Sampler) {
    let mut graph = Graph::new("checkered");
    let uv = graph.add_node(abi::context_node_id("uv"));
    let sampler_node = graph.add(Node::new("texture.sampler").with_setting("name", "linear"));
    let texture_node = graph.add(Node::new("texture.texture_2d").with_setting("name", "albedo"));
    let sample = graph.add_node("sample.texture_2d");
    let split = graph.add_node("convert.split.vec4f");
    let color = graph.add_node("convert.combine.vec3f");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    let wire = |graph: &mut Graph,
                from: (wxsl::core::graph::NodeId, &str),
                to: (wxsl::core::graph::NodeId, &str)| {
        graph
            .wire(&harness.registry, from, to)
            .expect("the checker graph is wired wrong");
    };
    wire(&mut graph, (texture_node, "out"), (sample, "tex"));
    wire(&mut graph, (sampler_node, "out"), (sample, "samp"));
    wire(&mut graph, (uv, "out"), (sample, "uv"));
    wire(&mut graph, (sample, "out"), (split, "v"));
    for channel in ["x", "y", "z"] {
        wire(&mut graph, (split, channel), (color, channel));
    }
    wire(&mut graph, (color, "out"), (output, "emissive"));
    let material = harness.material(&graph);

    // A 32x32 two-texel checker: high-frequency structure in both axes,
    // in linear space, the same supply-what-was-declared move the gallery
    // makes.
    const SIDE: u32 = 32;
    let mut texels = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let bright = (x / 2 + y / 2) % 2 == 0;
            let level = if bright { 230 } else { 25 };
            texels.extend_from_slice(&[level, level, level, 255]);
        }
    }
    let texture = harness.gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("blur checker"),
        size: wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    harness.gpu.queue.write_texture(
        texture.as_image_copy(),
        &texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIDE * 4),
            rows_per_image: Some(SIDE),
        },
        wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = harness.gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (material, view, sampler)
}

#[test]
fn a_moving_cube_blurs() {
    let Some(gpu) = gpu() else { return };
    let harness = probe::Harness::new(gpu);
    let (material, texture, sampler) = checkered_material(&harness);
    let mesh = Mesh::cube(&harness.gpu.device, 1.0);

    let mut sharp = renderer_for(&harness);
    let bindings = {
        let mut bindings = sharp.material_bindings(&harness.gpu.device, &material);
        bindings
            .set_texture("albedo", &texture)
            .expect("the checker texture binds");
        bindings
            .set_sampler("linear", &sampler)
            .expect("the checker sampler binds");
        bindings
            .upload(&harness.gpu.device, &harness.gpu.queue)
            .expect("uploaded");
        bindings
    };
    run_document(&mut sharp, &blur_document(false));
    let mut blurred = renderer_for(&harness);
    {
        let mut blurred_bindings = blurred.material_bindings(&harness.gpu.device, &material);
        blurred_bindings
            .set_texture("albedo", &texture)
            .expect("the checker texture binds");
        blurred_bindings
            .set_sampler("linear", &sampler)
            .expect("the checker sampler binds");
        blurred_bindings
            .upload(&harness.gpu.device, &harness.gpu.queue)
            .expect("uploaded");
    }
    run_document(&mut blurred, &blur_document(true));

    // Mid-sweep, where the screen velocity is largest.
    let time = 0.35;
    let draws = wxsl::render::single_draw(
        DrawItem::new(&mesh, &material)
            .with_transform(sweep(time))
            .with_previous(sweep(time - STEP))
            .with_bindings(&bindings),
    );
    let scene = environment(None, time);
    let sharp_image = render_list_in(&harness.gpu, &mut sharp, &harness.target, &draws, &scene)
        .expect("the sharp frame renders");
    let blur_image = render_list_in(&harness.gpu, &mut blurred, &harness.target, &draws, &scene)
        .expect("the blurred frame renders");

    let difference = mean_difference(&sharp_image, &blur_image);
    println!("blur moves the picture by {difference:.5}");
    // The smear concentrates at the moving silhouette — the interior of
    // a flat-coloured cube smears onto itself — so a whole-frame mean of
    // one-and-a-bit eight-bit steps *is* a strong effect.
    assert!(
        difference > 1.0,
        "a multi-texel sweep under a full-strength blur is not subtle: {difference}"
    );

    // And it moved the picture the way a smear does: sharp local
    // structure along the motion axis averages away, so the blurred
    // frame's horizontal gradient energy is lower than the sharp one's.
    let energy = |image: &[u8]| -> f32 {
        let stride = SIZE as usize * 4;
        let mut sum = 0.0f32;
        for row in 0..SIZE as usize {
            for column in 1..SIZE as usize {
                let at = (row * stride + column * 4) as isize;
                for channel in 0..3 {
                    let here = image[at as usize + channel] as f32;
                    let left = image[(at - 4) as usize + channel] as f32;
                    sum += (here - left).abs();
                }
            }
        }
        sum
    };
    let sharp_energy = energy(&sharp_image);
    let blur_energy = energy(&blur_image);
    println!("horizontal energy: sharp {sharp_energy:.0}, blurred {blur_energy:.0}");
    assert!(
        blur_energy < sharp_energy * 0.85,
        "the smear should average structure along the motion away: \
         {blur_energy} vs {sharp_energy}"
    );
}
