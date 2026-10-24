//! The ambient term reads the baked environment-BRDF table (ADR 0039).
//!
//! The device-free halves — that the bake's shader declares what its
//! descriptor promises, that the table's bindings are in the frame group —
//! are checked in `wxsl-render` and by the corpus gate. What needs a
//! device is the loop closing: the `brdf_lut` effect runs once into the
//! frame group, `ambient_environment` samples it through
//! `environment_brdf`, and a surface lit by nothing but an environment
//! comes out looking like that environment.
//!
//! The tests isolate ambient light or check its sum with direct lights.
//! They present through `probe::linear_document`, without a display transform.

use glam::{Mat4, Vec3};
use wxsl::core::abi;
use wxsl::core::graph::Graph;
use wxsl::core::node::Value;
use wxsl::render::{Camera, DrawItem, Environment};

mod probe;

use probe::{gpu, pixel, render_list_in, Harness, SIZE};

#[path = "probe/ibl.rs"]
#[allow(dead_code)] // Shared fixture also supports the full IBL and refraction tests.
mod fixture;

/// A rough receiver must obey linear addition even when the light loops
/// diverge at disc ranges. Evaluating IBL after those loops produced
/// bright speckles on Metal (the drive's asphalt at night).
#[test]
fn ibl_and_varying_direct_lights_add_without_bright_speckles() {
    use wxsl::render::Light;
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    for effect in fixture::effects() {
        harness.renderer.add_effect(effect);
    }
    let mut frame = harness.renderer.render_graph().clone();
    fixture::append_lighting_bake(&mut frame, false);
    harness.renderer.set_graph(frame).unwrap();
    let mut graph = Graph::new("rough receiver");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.set_param(output, "base_color", Value::Vec3([0.02, 0.01, 0.01]));
    graph.set_param(output, "roughness", Value::F32(0.93));
    let material = harness.material(&graph);
    let draws = wxsl::render::single_draw(DrawItem::new(&harness.mesh, &material));
    let mut env = probe::unlit();
    env.camera.eye = Vec3::new(0.0, 0.8, 1.5);
    let lights = vec![
        Light::spot(
            Vec3::new(-0.4, 1.0, 0.0),
            Vec3::NEG_Y,
            0.8,
            1.2,
            Vec3::ONE,
            1.0,
        )
        .with_range(2.0),
        Light::spot(
            Vec3::new(0.4, 1.0, 0.0),
            Vec3::NEG_Y,
            0.8,
            1.2,
            Vec3::ONE,
            1.0,
        )
        .with_range(2.0),
        Light::directional(Vec3::new(0.6, 0.4, 0.6), Vec3::new(0.05, 0.065, 0.1), 1.0),
        Light::disc(
            Vec3::new(-0.2, 0.15, 0.0),
            Vec3::NEG_Y,
            0.08,
            Vec3::ONE,
            0.2,
        )
        .with_range(0.25),
        Light::disc(Vec3::new(0.2, 0.15, 0.0), Vec3::NEG_Y, 0.08, Vec3::ONE, 0.2).with_range(0.25),
    ];
    let ambient = render_list_in(
        &harness.gpu,
        &mut harness.renderer,
        &harness.target,
        &draws,
        &env,
    )
    .unwrap();
    assert!(ambient.chunks_exact(4).any(|pixel| pixel[..3] != [0; 3]));
    let mut expected: Vec<i32> = ambient.iter().map(|&value| i32::from(value)).collect();
    for light in &lights {
        env.lights = vec![light.clone()];
        let image = render_list_in(
            &harness.gpu,
            &mut harness.renderer,
            &harness.target,
            &draws,
            &env,
        )
        .unwrap();
        for ((sum, actual), base) in expected.iter_mut().zip(image).zip(&ambient) {
            *sum += i32::from(actual) - i32::from(*base);
        }
    }
    env.lights = lights;
    let combined = render_list_in(
        &harness.gpu,
        &mut harness.renderer,
        &harness.target,
        &draws,
        &env,
    )
    .unwrap();
    assert!(combined
        .iter()
        .zip(&ambient)
        .enumerate()
        .any(|(index, (&actual, &base))| index % 4 != 3 && actual > base.saturating_add(1)));
    for (index, (&actual, expected)) in combined.iter().zip(expected).enumerate() {
        if index % 4 != 3 {
            assert!(
                (i32::from(actual) - expected).abs() <= 7,
                "pixel {}, channel {}: combined {actual}, sum {expected}",
                index / 4,
                index % 4
            );
        }
    }
}

/// A surface with no light of its own: a mirror-ish metal, or a rough one.
///
/// Metallic, because a metal's ambient has no diffuse term at all — so
/// what reaches the image is exactly the specular half, which is the half
/// the table is in.
fn metal(roughness: f32) -> Graph {
    let mut graph = Graph::new(format!("metal {roughness}"));
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.set_param(output, "base_color", Value::Vec3([1.0, 1.0, 1.0]));
    graph.set_param(output, "metallic", Value::F32(1.0));
    graph.set_param(output, "roughness", Value::F32(roughness));
    graph
}

/// No lamps, a strongly blue sky and a black ground: an environment that
/// cannot be confused with the surface's own white.
fn sky_only() -> Environment {
    Environment {
        camera: Camera {
            eye: Vec3::new(0.0, 0.0, 3.0),
            aspect: 1.0,
            ..Camera::default()
        },
        lights: Vec::new(),
        ambient_sky: Vec3::new(0.15, 0.35, 0.9),
        ambient_ground: Vec3::ZERO,
        exposure: 1.0,
        time: 0.0,
        previous_time: 0.0,
        previous_camera: None,
        origin: Default::default(),
        cluster_lights: true,
        probe: None,
        shadow_atlas_size: wxsl::core::abi::SHADOW_ATLAS_SIZE,
    }
}

/// Draw one plane of `graph`'s material under `sky_only` and read the
/// middle pixel.
fn shade(harness: &mut Harness, graph: &Graph) -> [u8; 4] {
    let material = harness.material(graph);
    let mut bindings = harness.bindings(&material);
    bindings
        .upload(&harness.gpu.device, &harness.gpu.queue)
        .expect("the material declares nothing to bind");
    let item = DrawItem::new(&harness.mesh, &material)
        // Lying flat by default, so stand it up to face the camera.
        .with_transform(Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2))
        .with_bindings(&bindings);
    let draws = wxsl::render::single_draw(item);
    let image = render_list_in(
        &harness.gpu,
        &mut harness.renderer,
        &harness.target,
        &draws,
        &sky_only(),
    )
    .expect("the frame renders");
    pixel(&image, SIZE / 2, SIZE / 2)
}

#[test]
fn a_metal_lit_only_by_an_environment_shades_as_that_environment() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let shaded = shade(&mut harness, &metal(0.05));

    // A metal's ambient is `irradiance * (F0 * scale + bias)`, and `scale`
    // and `bias` come from the table. An unbaked table is all zeroes, so
    // this assertion is also the one that says the bake ran: without it
    // the surface would be black, whatever the sky.
    assert!(
        shaded[2] > 20,
        "a metal under a blue sky reflects it: {shaded:?}"
    );
    // And it reflects the *sky's* colour rather than its own white.
    assert!(
        shaded[2] > shaded[0] * 2,
        "the reflection is the sky's blue, not the surface's white: {shaded:?}"
    );
}

#[test]
fn roughness_changes_what_the_ambient_reflects() {
    let Some(gpu) = gpu() else { return };
    let mut harness = Harness::new(gpu);
    let smooth = shade(&mut harness, &metal(0.05));
    let rough = shade(&mut harness, &metal(0.95));

    // The table's second axis *is* roughness, and the mirror lookup blurs
    // towards the diffuse irradiance as it rises — so two metals differing
    // only in roughness cannot come out the same colour. If they do, the
    // lookup is reading one row of the table, or none of it.
    let gap = (0..3)
        .map(|channel| smooth[channel].abs_diff(rough[channel]))
        .max()
        .expect("three channels");
    assert!(
        gap > 4,
        "roughness changed nothing: smooth {smooth:?}, rough {rough:?}"
    );
}
