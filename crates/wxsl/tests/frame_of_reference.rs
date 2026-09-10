//! The two ABI hooks M5 landed while the vertex stage was open, and which
//! nothing yet consumes: where world space is measured from, and which
//! frame's clock a time-driven node reads.
//!
//! Both are macro flags on the hand-written ABI rather than anything a
//! graph can see, and both exist now because retrofitting them once many
//! materials read `world_position` or `time` is a migration, while doing
//! it during the vertex work is an edit to two files.

use std::f32::consts::FRAC_PI_2;

use glam::{Mat4, Vec3};
use wxsl::core::abi;
use wxsl::core::graph::Graph;
use wxsl::core::macros::{MacroSet, MacroValue};
use wxsl::core::node::ValueType;
use wxsl::render::material::Material;
use wxsl::render::{Camera, DrawItem, Environment};

mod probe;
use probe::{close, gpu, no_tonemap, pixel, render_list_in, srgb, Harness, SIZE};

/// `no_tonemap`, plus `flag` turned on.
fn with_flag(flag: &str) -> MacroSet {
    let mut macros = no_tonemap();
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
    let now = Material::from_graph_with_macros(&graph, &harness.registry, &no_tonemap())
        .expect("the graph compiles");
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
        close(this_frame[1], srgb(0.75)),
        "the ordinary graph reads the current clock: {this_frame:?}"
    );
    assert!(
        close(last_frame[1], srgb(0.25)),
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
    // needs model matrices pre-translated on the host in `f64`, and is
    // not this milestone's.
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let mut graph = Graph::new("lit");
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let absolute = Material::from_graph_with_macros(&graph, &harness.registry, &no_tonemap())
        .expect("the graph compiles");
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
