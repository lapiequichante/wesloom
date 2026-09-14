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
//! Every test here draws one plane facing the camera with no lights at
//! all, so the image is the ambient term and nothing else, and presents
//! through `probe::linear_document` — no display transform between the
//! shader's number and the pixel.

use glam::{Mat4, Vec3};
use wxsl::core::abi;
use wxsl::core::graph::Graph;
use wxsl::core::node::Value;
use wxsl::render::{Camera, DrawItem, Environment};

mod probe;

use probe::{gpu, pixel, render_list_in, Harness, SIZE};

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
