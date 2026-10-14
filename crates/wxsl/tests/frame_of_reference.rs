//! The two ABI hooks M5 landed while the vertex stage was open: where
//! world space is measured from (with its host half, the floating origin
//! of ADR 0074), and which frame's clock a time-driven node reads.
//!
//! Both are macro flags on the hand-written ABI rather than anything a
//! graph can see, and both exist now because retrofitting them once many
//! materials read `world_position` or `time` is a migration, while doing
//! it during the vertex work is an edit to two files.

use std::f32::consts::FRAC_PI_2;

use glam::{DMat4, DVec3, Mat4, Vec3};
use wxsl::core::abi;
use wxsl::core::graph::Graph;
use wxsl::core::macros::{MacroSet, MacroValue};
use wxsl::core::node::ValueType;
use wxsl::render::material::Material;
use wxsl::render::{Camera, DrawItem, Environment, StockPipeline};

mod probe;
use probe::{close, gpu, pixel, quantized, render_list_in, Harness, SIZE};

/// An otherwise empty macro set with `flag` turned on.
fn with_flag(flag: &str) -> MacroSet {
    let mut macros = MacroSet::new();
    macros.set(flag, MacroValue::Flag(true));
    macros
}

/// A material whose emissive is the frame clock, splatted to a colour.
///
/// The shortest possible time-driven graph: whatever `input.time`
/// answers is what lands in the framebuffer, so a test can read the clock
/// off a pixel.
fn clock_graph(harness: &Harness) -> Graph {
    let mut graph = Graph::new("clock");
    let time = graph.add_node("input.time");
    let splat = graph.add_node("convert.splat");
    graph
        .set_generic(&harness.registry, splat, "T", ValueType::Vec3)
        .expect("vec3f is allowed");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .wire(&harness.registry, (time, "out"), (splat, "value"))
        .expect("time is a scalar");
    graph
        .wire(&harness.registry, (splat, "out"), (output, "emissive"))
        .expect("a vec3f is an emissive");
    graph
}

/// Draw the harness's plane, facing the camera, and read the centre pixel.
fn shade_in(harness: &mut Harness, material: &Material, environment: &Environment) -> [u8; 4] {
    let item =
        DrawItem::new(&harness.mesh, material).with_transform(Mat4::from_rotation_x(FRAC_PI_2));
    let draws = wxsl::render::single_draw(item);
    let image = render_list_in(
        &harness.gpu,
        &mut harness.renderer,
        &harness.target,
        &draws,
        environment,
    )
    .expect("the frame renders");
    pixel(&image, SIZE / 2, SIZE / 2)
}

/// The unlit environment, with a clock that has ticked from `previous` to
/// `now`.
fn at(previous: f32, now: f32) -> Environment {
    let mut environment = Environment {
        camera: Camera {
            eye: Vec3::new(0.0, 0.0, 3.0),
            aspect: 1.0,
            ..Camera::default()
        },
        lights: Vec::new(),
        ambient_sky: Vec3::ZERO,
        ambient_ground: Vec3::ZERO,
        exposure: 1.0,
        time: previous,
        previous_time: previous,
        previous_camera: None,
        origin: Default::default(),
        cluster_lights: true,
    };
    environment.advance(now);
    environment
}

#[test]
fn advancing_the_clock_remembers_what_it_was() {
    let mut environment = Environment::default();
    environment.advance(0.25);
    environment.advance(0.75);
    assert_eq!(environment.previous_time, 0.25);
    assert_eq!(environment.time, 0.75);
}

#[test]
fn a_graph_compiled_for_the_previous_frame_reads_the_previous_clock() {
    // The shape a velocity stage needs. A motion vector for an object the
    // *graph* moves is wrong unless the whole graph — not just the model
    // matrix — is re-evaluated for the previous frame, and this is the
    // switch that does it in one place.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let graph = clock_graph(&harness);
    let now = Material::from_graph(&graph, &harness.registry).expect("the graph compiles");
    let before = Material::from_graph_with_macros(
        &graph,
        &harness.registry,
        &with_flag(abi::FEATURE_PREVIOUS_FRAME),
    )
    .expect("the graph compiles");

    let environment = at(0.25, 0.75);
    let this_frame = shade_in(&mut harness, &now, &environment);
    let last_frame = shade_in(&mut harness, &before, &environment);

    assert!(
        close(this_frame[1], quantized(0.75)),
        "the ordinary graph reads the current clock: {this_frame:?}"
    );
    assert!(
        close(last_frame[1], quantized(0.25)),
        "the same graph, one flag on, reads the previous one: {last_frame:?}"
    );
}

#[test]
fn measuring_world_space_from_the_eye_changes_no_pixel_near_the_origin() {
    // M10's acceptance criterion, half of it: whatever else
    // relative-to-eye buys, it must not change the image. Every place the
    // ABI needs absolute world space — a point light's falloff, a shadow
    // lookup — puts the origin back through `world_origin()`, so the
    // algebra cancels exactly and this is an equality rather than a
    // tolerance.
    //
    // The other half — a scene at 10^7 units rendering without jitter —
    // is `a_scene_far_from_the_origin_renders_as_it_does_near_it`.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let mut graph = Graph::new("lit");
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let absolute = Material::from_graph(&graph, &harness.registry).expect("the graph compiles");
    let relative = Material::from_graph_with_macros(
        &graph,
        &harness.registry,
        &with_flag(abi::FEATURE_RELATIVE_TO_EYE),
    )
    .expect("the graph compiles");
    // The flag is a macro, so the two really are different variants
    // rather than the same module drawn twice.
    assert_ne!(absolute.macros().signature(), relative.macros().signature());

    // A point light, because its falloff is the one lighting term that
    // reads an absolute position rather than a direction.
    let mut environment = at(0.0, 0.0);
    environment.lights = vec![wxsl::render::Light::point(
        Vec3::new(1.5, 1.5, 2.0),
        Vec3::ONE,
        6.0,
    )];
    environment.ambient_sky = Vec3::splat(0.1);

    let plain = shade_in(&mut harness, &absolute, &environment);
    let moved = shade_in(&mut harness, &relative, &environment);
    assert_eq!(plain, moved, "relative-to-eye changed the image");
}

/// The lit plane of the test above, placed at `offset` in the absolute
/// world, drawn either rebased onto the eye in `f64` or naively, as
/// absolute `f32`s.
fn far_frame(harness: &mut Harness, material: &Material, offset: DVec3, rebased: bool) -> Vec<u8> {
    let eye = offset + DVec3::new(0.0, 0.0, 3.0);
    let light = offset + DVec3::new(0.6, 0.4, 0.8);
    let model = DMat4::from_translation(offset) * DMat4::from_rotation_x(FRAC_PI_2.into());
    let mut environment = at(0.0, 0.0);
    environment.ambient_sky = Vec3::splat(0.1);
    let transform = if rebased {
        environment.look_from(eye, offset);
        environment.local_transform(model)
    } else {
        environment.camera.eye = eye.as_vec3();
        environment.camera.target = offset.as_vec3();
        model.as_mat4()
    };
    let light = if rebased {
        environment.local_point(light)
    } else {
        light.as_vec3()
    };
    environment.lights = vec![wxsl::render::Light::point(light, Vec3::ONE, 3.0)];
    let item = DrawItem::new(&harness.mesh, material).with_transform(transform);
    let draws = wxsl::render::single_draw(item);
    render_list_in(
        &harness.gpu,
        &mut harness.renderer,
        &harness.target,
        &draws,
        &environment,
    )
    .expect("the frame renders")
}

/// The largest channel difference between two frames.
fn widest(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0)
}

#[test]
fn a_scene_far_from_the_origin_renders_as_it_does_near_it() {
    // M10's other half. At 10^7 an f32 has a whole unit between
    // neighbours, so the naive frame — camera, model and light as absolute
    // f32s — snaps a two-unit plane onto that grid and is wrecked. Rebased
    // onto the eye in f64 (`Environment::look_from`), the same scene is
    // the near-origin frame again, under both stock pipelines: the
    // deferred one reconstructs positions from depth in buffer space and
    // has to measure them from `world_origin()` like everything else.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let mut graph = Graph::new("lit");
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let relative = Material::from_graph_with_macros(
        &graph,
        &harness.registry,
        &with_flag(abi::FEATURE_RELATIVE_TO_EYE),
    )
    .expect("the graph compiles");
    let far = DVec3::new(1.0e7, 0.0, -1.0e7);
    // Not a round number, so nothing about the far frame is exact by luck.
    let nudged = far + DVec3::new(0.37, 0.0, 0.0213);

    for pipeline in [
        None,
        Some(StockPipeline::Forward),
        Some(StockPipeline::Deferred),
    ] {
        if let Some(pipeline) = pipeline {
            harness.renderer.set_pipeline(pipeline);
        }
        let near = far_frame(&mut harness, &relative, DVec3::ZERO, true);
        let unmoved = far_frame(&mut harness, &relative, DVec3::ZERO, false);
        // Not an equality: the view matrix is built from another eye, so
        // it rounds differently, and the deferred pass reconstructs
        // positions through its inverse.
        assert!(
            widest(&near, &unmoved) <= 3,
            "{pipeline:?}: rebasing at the origin changed the image by {}",
            widest(&near, &unmoved)
        );
        assert!(
            near.chunks(4).any(|texel| texel[..3] != near[..3]),
            "{pipeline:?}: the near frame should show the plane against the clear"
        );
        for offset in [far, nudged] {
            let rebased = far_frame(&mut harness, &relative, offset, true);
            assert!(
                widest(&near, &rebased) <= 3,
                "{pipeline:?}: at {offset} the rebased frame drifted by {}",
                widest(&near, &rebased)
            );
        }
        let naive = far_frame(&mut harness, &relative, far, false);
        assert!(
            widest(&near, &naive) > 32,
            "{pipeline:?}: the naive f32 frame should have been wrecked, or this test proves nothing"
        );
    }
}
