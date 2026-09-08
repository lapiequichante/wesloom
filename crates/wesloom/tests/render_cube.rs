//! Tests that need a real GPU: render the demo cube through both paths and
//! check the pixels.
//!
//! Every test here skips (rather than fails) when no adapter is available, so
//! the suite still passes on a machine or CI runner with no usable GPU. What
//! it would otherwise catch is exactly the class of bug the compile-only
//! tests cannot see: a bind group layout that disagrees with the shader, a
//! G-buffer format that cannot round-trip a normal, a depth reconstruction
//! that is off by a matrix.

use glam::{Mat4, Vec3};
use wesloom::core::abi;
use wesloom::core::graph::Graph;
use wesloom::core::macros::{MacroSet, MacroValue};
use wesloom::render::gpu::{GpuContext, OffscreenTarget};
use wesloom::render::material::Material;
use wesloom::render::{Camera, Light, RenderPath, RenderRequest, Renderer, Scene, TargetConfig};

const SIZE: u32 = 128;

fn demo_graph() -> Graph {
    serde_json::from_str(include_str!("../assets/pbr_cube.wesloom.json")).expect("parses")
}

fn test_scene() -> Scene {
    Scene {
        camera: Camera {
            eye: Vec3::new(2.4, 1.9, 3.2),
            aspect: 1.0,
            ..Camera::default()
        },
        lights: vec![
            Light::point(Vec3::new(2.6, 3.0, 2.2), Vec3::new(1.0, 0.86, 0.72), 42.0),
            Light::directional(Vec3::new(-0.4, 0.7, -1.0), Vec3::new(0.7, 0.75, 0.9), 1.1),
        ],
        time: 1.0,
        ..Scene::default()
    }
}

/// A GPU context, or `None` when this machine has no usable adapter.
fn gpu() -> Option<GpuContext> {
    match pollster::block_on(GpuContext::headless()) {
        Ok(context) => Some(context),
        Err(error) => {
            eprintln!("skipping GPU test: {error}");
            None
        }
    }
}

/// Render the demo cube at `SIZE` square and return the RGBA8 pixels.
fn render(gpu: &GpuContext, macros: &MacroSet, paths: &[RenderPath]) -> (Vec<Vec<u8>>, Renderer) {
    let registry = wesloom::stdlib::registry();
    let material =
        Material::from_graph_with_macros(&demo_graph(), &registry, macros).expect("compiles");
    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = Renderer::new(
        &gpu.device,
        wesloom::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, target.format()),
    )
    .expect("the stdlib library satisfies the ABI");
    let mesh = wesloom::render::Mesh::cube(&gpu.device, 1.6);
    let scene = test_scene();

    let mut images = Vec::new();
    for path in paths {
        renderer.set_path(*path);
        renderer
            .render(
                &gpu.device,
                &gpu.queue,
                &RenderRequest {
                    view: target.view(),
                    scene: &scene,
                    model: Mat4::from_rotation_y(0.6),
                    mesh: &mesh,
                    material: &material,
                },
            )
            .unwrap_or_else(|error| panic!("cannot render on the {path} path: {error}"));
        gpu.wait();
        images.push(target.read_rgba8(&gpu.device, &gpu.queue));
    }
    (images, renderer)
}

fn pixel(image: &[u8], x: u32, y: u32) -> [u8; 4] {
    let index = ((y * SIZE + x) * 4) as usize;
    image[index..index + 4].try_into().expect("in bounds")
}

fn covered(image: &[u8]) -> usize {
    image
        .chunks_exact(4)
        .filter(|texel| texel[0] > 12 || texel[1] > 12 || texel[2] > 12)
        .count()
}

fn mean_difference(a: &[u8], b: &[u8]) -> f32 {
    let sum: f32 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (f32::from(*x) - f32::from(*y)).abs())
        .sum();
    sum / a.len() as f32 / 255.0
}

#[test]
fn the_cube_is_lit_and_the_background_is_not() {
    let Some(gpu) = gpu() else { return };
    let (images, _) = render(&gpu, &MacroSet::new(), &[RenderPath::Forward]);
    let image = &images[0];

    // The cube covers the middle of the frame and nothing covers the corner.
    let center = pixel(image, SIZE / 2, SIZE / 2);
    assert!(
        center[0] > 40,
        "the middle of the frame should be a lit surface, got {center:?}"
    );
    // The albedo is a warm orange, so red must dominate blue.
    assert!(center[0] > center[2], "expected a warm surface: {center:?}");
    let corner = pixel(image, 2, 2);
    assert!(
        corner[0] < 12 && corner[1] < 12,
        "the corner should be background, got {corner:?}"
    );

    let covered = covered(image);
    let total = (SIZE * SIZE) as usize;
    assert!(
        covered > total / 8 && covered < total * 3 / 4,
        "the cube should cover a sensible part of the frame, got {covered}/{total}"
    );
}

#[test]
fn both_paths_produce_the_same_image() {
    let Some(gpu) = gpu() else { return };
    let (images, renderer) = render(&gpu, &MacroSet::new(), RenderPath::ALL);

    // The two paths run the same material graph through the same shading
    // function, so they agree up to the G-buffer's 8-bit base colour and
    // half-float normals. A larger difference means the deferred path's
    // packing, its depth-based world position, or its bindings are wrong.
    let difference = mean_difference(&images[0], &images[1]);
    assert!(
        difference < 0.01,
        "forward and deferred differ by {difference:.4} on average"
    );
    assert!(
        covered(&images[0]).abs_diff(covered(&images[1])) < 32,
        "the two paths cover different areas: {} vs {}",
        covered(&images[0]),
        covered(&images[1])
    );

    // One material variant per path, plus the lighting pass.
    assert_eq!(renderer.variant_count(), 3);
}

#[test]
fn switching_path_reuses_cached_shaders() {
    let Some(gpu) = gpu() else { return };
    let (_, renderer) = render(
        &gpu,
        &MacroSet::new(),
        &[
            RenderPath::Forward,
            RenderPath::Deferred,
            RenderPath::Forward,
            RenderPath::Deferred,
        ],
    );
    let stats = renderer.cache_stats();
    // Four frames over two paths: three compiles (two materials plus the
    // lighting pass), and the rest served from the cache.
    assert_eq!(stats.misses, 3, "{stats:?}");
    assert!(stats.hits >= 3, "{stats:?}");
    assert_eq!(renderer.variant_count(), 3);
}

#[test]
fn the_debug_normal_view_round_trips_through_the_gbuffer() {
    let Some(gpu) = gpu() else { return };
    let mut macros = MacroSet::new();
    macros.set(abi::FEATURE_DEBUG_NORMALS, MacroValue::Flag(true));
    let (images, _) = render(&gpu, &macros, RenderPath::ALL);

    // Encoded normals are the strictest check available on the G-buffer: the
    // deferred path only matches if the normal survived being written to a
    // texture, read back and renormalized.
    let difference = mean_difference(&images[0], &images[1]);
    assert!(
        difference < 0.01,
        "normals differ between paths by {difference:.4}"
    );

    // A normal-coloured cube has faces of flat, saturated colour: with the
    // model rotated about Y only, the top face is +Y, i.e. (0.5, 1, 0.5).
    let top = pixel(&images[0], SIZE / 2, SIZE / 3);
    assert!(
        top[1] > 200 && top[0] > 100 && top[0] < 160,
        "the top face should show a +Y normal, got {top:?}"
    );
}

#[test]
fn a_macro_change_compiles_a_new_variant() {
    let Some(gpu) = gpu() else { return };
    let registry = wesloom::stdlib::registry();
    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = Renderer::new(
        &gpu.device,
        wesloom::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, target.format()),
    )
    .unwrap();
    let mesh = wesloom::render::Mesh::cube(&gpu.device, 1.6);
    let scene = test_scene();
    let graph = demo_graph();

    let mut images = Vec::new();
    for octaves in [1, 6] {
        let mut macros = MacroSet::new();
        macros.set("WESLOOM_FBM_OCTAVES", MacroValue::Int(octaves));
        let material = Material::from_graph_with_macros(&graph, &registry, &macros).unwrap();
        renderer
            .render(
                &gpu.device,
                &gpu.queue,
                &RenderRequest {
                    view: target.view(),
                    scene: &scene,
                    model: Mat4::from_rotation_y(0.6),
                    mesh: &mesh,
                    material: &material,
                },
            )
            .expect("renders");
        gpu.wait();
        images.push(target.read_rgba8(&gpu.device, &gpu.queue));
    }

    // Two octave counts are two shaders, and they must not look the same:
    // more octaves means finer detail in the roughness field.
    assert_eq!(renderer.variant_count(), 2);
    let difference = mean_difference(&images[0], &images[1]);
    assert!(
        difference > 0.0005,
        "changing the octave count changed nothing visible ({difference:.5})"
    );
}
