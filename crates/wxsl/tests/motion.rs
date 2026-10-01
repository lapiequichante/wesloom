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
        let scale =
            graph.add(Node::new("math.multiply").with_param("b", Value::F32(SCALE)));
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
        "Draw a velocity buffer as 0.5 + motion * 20 per channel.",
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
    let display = graph.add(
        Node::new(doc::PASS_SCREEN).with_setting(doc::SETTING_EFFECT, "probe.velocity_view"),
    );
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
    let taa = document.add(
        Node::new(format!("{}wxsl.taa", doc::PASS_SCREEN_PREFIX)).with_label("taa"),
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

/// The deferred preset plus *only* the velocity pass: the bisect build.
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
    let present = document
        .nodes()
        .find(|(_, node)| node.def == doc::PRESENT)
        .map(|(id, _)| id)
        .expect("a present");
    let velocity = document.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("velocity")
            .with_setting(doc::SETTING_PRECISION, "pair"),
    );
    let own_depth = document.add_node(doc::RESOURCE_DEPTH);
    let none = document.add(
        Node::new(doc::SOURCE_SCENE).with_setting(doc::SETTING_TAGS, "nothing_at_all"),
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
    match std::env::var("TAA_SPIN").as_deref() {
        // TEMP DEBUG: no motion at all — the resolve must be a no-op.
        Ok("still") => Mat4::IDENTITY,
        // TEMP DEBUG: translate instead of rotate, so the motion field is
        // the one the velocity probe already verified end to end.
        Ok("slide") => Mat4::from_translation(Vec3::new(time * 0.8, 0.0, 0.0)),
        _ => Mat4::from_rotation_y(time * 0.9),
    }
}

fn cube_draws<'a>(mesh: &'a Mesh, material: &'a Material, time: f32) -> wxsl::render::DrawList<'a> {
    wxsl::render::single_draw(
        DrawItem::new(mesh, material)
            .with_transform(spin(time))
            .with_previous(spin(time - STEP)),
    )
}

/// Run `frames` steps of the clock, returning the last two frames it
/// produced — the pair a temporal judgement is made on.
fn settle(
    harness: &Harness,
    renderer: &mut Renderer,
    mesh: &Mesh,
    material: &Material,
    frames: u32,
) -> (Vec<u8>, Vec<u8>) {
    let mut penultimate = Vec::new();
    let mut final_ = Vec::new();
    for frame in 0..frames {
        let time = frame as f32 * STEP;
        let draws = cube_draws(mesh, material, time);
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
    let Some(gpu) = gpu() else { return };
    let harness = probe::Harness::new(gpu);
    let material = plain_material(&harness);
    let mesh = Mesh::cube(&harness.gpu.device, 1.2);

    // The same scene twice: raw, and under the resolve. `settle` renders
    // the same clock for both, so the only difference is what the chain
    // does with time.
    let mut raw = renderer_for(&harness);
    run_document(
        &mut raw,
        &wxsl::render::StockPipeline::Deferred.document(),
    );
    let (raw_then, raw_now) = settle(&harness, &mut raw, &mesh, &material, 32);

    // TEMP DEBUG: what did the ring allocate?
    let ring_schedule = wxsl::render::compile_pipeline(
        &taa_document(),
        &wxsl::render::document_registry(renderer_for(&harness).effects()),
        &EffectRegistry::shipped(),
        &wxsl::render::PipelineConfig::new(TargetConfig::new(SIZE, SIZE, harness.target.format())),
    )
    .expect("compiles")
    .schedule()
    .expect("schedules")
    .slots()
    .iter()
    .map(|slot| slot.label.clone())
    .collect::<Vec<_>>();
    println!("slots: {ring_schedule:?}");

    let mut taa = renderer_for(&harness);
    run_document(&mut taa, &taa_document());
    if std::env::var("TAA_BLEND").is_ok() {
        let blend: f32 = std::env::var("TAA_BLEND").unwrap().parse().unwrap();
        taa.set_pass_param("taa", "blend", wxsl::core::node::Value::F32(blend))
            .expect("the blend knob exists");
    }
    let (taa_then, taa_now) = settle(&harness, &mut taa, &mesh, &material, 32);

    let raw_jitter = mean_difference(&raw_then, &raw_now);
    let taa_jitter = mean_difference(&taa_then, &taa_now);
    println!("frame-to-frame: raw {raw_jitter:.5}, taa {taa_jitter:.5}");
    // TEMP DEBUG: where does the settled resolve differ from raw? One
    // row of pixels, raw then resolved.
    let row = SIZE as usize / 2 * SIZE as usize * 4;
    for x in (0..SIZE as usize).step_by(4) {
        let at = row + x * 4;
        print!(
            "[{:3},{:3},{:3}] ",
            taa_now[at], taa_now[at + 1], taa_now[at + 2]
        );
    }
    println!("<- resolved");
    for x in (0..SIZE as usize).step_by(4) {
        let at = row + x * 4;
        print!(
            "[{:3},{:3},{:3}] ",
            raw_now[at], raw_now[at + 1], raw_now[at + 2]
        );
    }
    println!("<- raw");
    let mut buckets = [0usize; 5];
    let mut worst = (0usize, 0i32);
    for index in 0..raw_now.len() {
        let delta = (i32::from(raw_now[index]) - i32::from(taa_now[index])).abs();
        let bucket = match delta {
            0 => 0,
            1 => 1,
            2..=7 => 2,
            8..=31 => 3,
            _ => 4,
        };
        buckets[bucket] += 1;
        if delta > worst.1 {
            worst = (index, delta);
        }
    }
    println!(
        "delta buckets [0,1,2-7,8-31,32+]: {:?}, worst {} at pixel ({}, {})",
        buckets,
        worst.1,
        (worst.0 / 4) % SIZE as usize,
        (worst.0 / 4) / SIZE as usize
    );
    let (wx, wy) = ((worst.0 / 4) % SIZE as usize, (worst.0 / 4) / SIZE as usize);
    // TEMP DEBUG: what does the velocity buffer say at the worst pixel?
    let mut speedo = renderer_for(&harness);
    speedo.add_effect(velocity_view_effect(&harness.registry));
    run_document(&mut speedo, &velocity_document());
    let still = cube_draws(&mesh, &material, 8.0);
    let motion_image = render_list_in(
        &harness.gpu,
        &mut speedo,
        &harness.target,
        &still,
        &environment(None, 8.0),
    )
    .expect("the velocity frame renders");
    let vat = (wy * SIZE as usize + wx) * 4;
    println!(
        "velocity at ({wx},{wy}): display {:?} -> motion {:?}",
        &motion_image[vat..vat + 3],
        (
            (motion_image[vat] as f32 / 255.0 - 0.5) / 40.0,
            (motion_image[vat + 1] as f32 / 255.0 - 0.5) / 40.0
        )
    );
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            let x = (wx as i32 + dx).clamp(0, SIZE as i32 - 1) as usize;
            let y = (wy as i32 + dy).clamp(0, SIZE as i32 - 1) as usize;
            let at = (y * SIZE as usize + x) * 4;
            print!(
                "[{:3},{:3},{:3}|{:3},{:3},{:3}] ",
                raw_now[at], raw_now[at + 1], raw_now[at + 2],
                taa_now[at], taa_now[at + 1], taa_now[at + 2]
            );
        }
        println!();
    }
    // TEMP DEBUG: lag over time, resolve output vs raw at the same
    // instants. A correct resolve stays at the raw level; a poisoned one
    // grows with the frames it accumulates.
    for frames in [4, 8, 16, 32, 64] {
        let mut taa_n = renderer_for(&harness);
        run_document(&mut taa_n, &taa_document());
        let (_, taa_now_n) = settle(&harness, &mut taa_n, &mesh, &material, frames);
        let mut raw_n = renderer_for(&harness);
        run_document(&mut raw_n, &wxsl::render::StockPipeline::Deferred.document());
        let (_, raw_now_n) = settle(&harness, &mut raw_n, &mesh, &material, frames);
        println!(
            "lag at {frames} frames: {:.5}",
            mean_difference(&taa_now_n, &raw_now_n)
        );
    }
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

#[test]
fn a_moving_cube_blurs() {
    let Some(gpu) = gpu() else { return };
    let harness = probe::Harness::new(gpu);
    let material = plain_material(&harness);
    let mesh = Mesh::cube(&harness.gpu.device, 1.0);

    let mut sharp = renderer_for(&harness);
    run_document(&mut sharp, &blur_document(false));
    let mut blurred = renderer_for(&harness);
    run_document(&mut blurred, &blur_document(true));

    // Mid-sweep, where the screen velocity is largest.
    let time = 0.35;
    let draws = wxsl::render::single_draw(
        DrawItem::new(&mesh, &material)
            .with_transform(sweep(time))
            .with_previous(sweep(time - STEP)),
    );
    let scene = environment(None, time);
    let sharp_image = render_list_in(&harness.gpu, &mut sharp, &harness.target, &draws, &scene)
        .expect("the sharp frame renders");
    let blur_image = render_list_in(&harness.gpu, &mut blurred, &harness.target, &draws, &scene)
        .expect("the blurred frame renders");

    let difference = mean_difference(&sharp_image, &blur_image);
    println!("blur moves the picture by {difference:.5}");
    assert!(
        difference > 2.0,
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

#[test]
fn debug_deferred_plus_velocity_matches_deferred() {
    let Some(gpu) = gpu() else { return };
    let harness = probe::Harness::new(gpu);
    let material = plain_material(&harness);
    let mesh = Mesh::cube(&harness.gpu.device, 1.2);

    let render_at = |renderer: &mut Renderer| {
        let time = 0.4;
        let draws = cube_draws(&mesh, &material, time);
        render_list_in(
            &harness.gpu,
            renderer,
            &harness.target,
            &draws,
            &environment(None, time),
        )
        .expect("the frame renders")
    };
    let mut a = renderer_for(&harness);
    run_document(&mut a, &wxsl::render::StockPipeline::Deferred.document());
    let plain = render_at(&mut a);
    let mut b = renderer_for(&harness);
    run_document(&mut b, &velocity_only_document());
    let with_velocity = render_at(&mut b);
    let difference = mean_difference(&plain, &with_velocity);
    println!("deferred vs deferred+velocity: {difference:.5}");
    assert!(difference < 0.5, "{difference}");
}
