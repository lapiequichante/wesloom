//! Lighting models, on real hardware: three objects shaded by three
//! different models through one deferred lighting pass, the dispatch id
//! riding a G-buffer channel that costs nothing when the set does not
//! dispatch (ADR 0028).
//!
//! The strongest assertion here is the equivalence one: with mixed models
//! in the frame, forward and deferred still produce the same image — the
//! forward path calls each material's model directly, the deferred pass
//! switches over the id its G-buffer channel carries, and the two only
//! agree if both halves name the same models.

use glam::{Mat4, Vec3, Vec4Swizzles};
use wxsl::core::abi;
use wxsl::core::graph::{Graph, Node};
use wxsl::core::lighting::{LightingSet, DEFAULT_MODELS, DEFAULT_MODEL_ID};
use wxsl::core::node::{NodeRegistry, Value};
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::material::{Material, MaterialOptions};
use wxsl::render::pipeline::StockPipeline;
use wxsl::render::{
    Camera, DrawItem, DrawList, Environment, Light, Mesh, RenderRequest, Renderer, TargetConfig,
};

mod probe;
use probe::{gpu, SIZE};

/// Edge of each test quad.
const QUAD: f32 = 1.5;
/// Where the three quads sit, so one camera sees all of them.
const SLOTS: [f32; 3] = [-2.0, 0.0, 2.0];

fn camera() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 3.0, 5.0),
        target: Vec3::ZERO,
        aspect: 1.0,
        ..Camera::default()
    }
}

/// One directional light overhead plus, for each quad, a point light
/// placed along the *mirror* of the view direction about the ground
/// normal — the one direction whose specular highlight peaks exactly at
/// the quad centre the tests sample, which is where the models differ
/// most.
fn lit() -> Environment {
    let eye = camera().eye;
    let mut lights = vec![Light::directional(Vec3::Y, Vec3::splat(0.25), 1.0)];
    for x in SLOTS {
        let point = Vec3::new(x, 0.0, 0.0);
        let view = (eye - point).normalize();
        let mirror = 2.0 * view.dot(Vec3::Y) * Vec3::Y - view;
        lights.push(Light::point(point + mirror * 8.0, Vec3::ONE, 30.0));
    }
    Environment {
        camera: camera(),
        lights,
        ambient_sky: Vec3::splat(0.05),
        ambient_ground: Vec3::splat(0.02),
        exposure: 1.0,
        time: 0.0,
        previous_time: 0.0,
    }
}

/// Where a world point lands in the image.
fn pixel_of(world: Vec3) -> (u32, u32) {
    let clip = camera().view_proj() * world.extend(1.0);
    let ndc = clip.xyz() / clip.w;
    let x = ((ndc.x * 0.5 + 0.5) * SIZE as f32).round();
    let y = ((0.5 - ndc.y * 0.5) * SIZE as f32).round();
    ((x as u32).min(SIZE - 1), (y as u32).min(SIZE - 1))
}

/// The image's RGB at a world point, as `f32` thirds so an assertion reads
/// as a colour difference rather than a channel coincidence.
fn color_at(image: &[u8], world: Vec3) -> [f32; 3] {
    let (x, y) = pixel_of(world);
    let index = ((y * SIZE + x) * 4) as usize;
    [
        image[index] as f32,
        image[index + 1] as f32,
        image[index + 2] as f32,
    ]
}

fn gap(a: [f32; 3], b: [f32; 3]) -> f32 {
    let mut worst = 0.0f32;
    for channel in 0..3 {
        worst = worst.max((a[channel] - b[channel]).abs());
    }
    worst
}

/// The worst channel difference anywhere in a small patch around a world
/// point.
///
/// The models are compared on their specular *lobes*, and a lobe is a
/// curve, not a number: two lobes that agree at the highlight's peak
/// disagree on its shoulders, so the honest sample is a patch.
fn patch_gap(a: &[u8], b: &[u8], world: Vec3) -> f32 {
    let (cx, cy) = pixel_of(world);
    let mut worst = 0.0f32;
    for dy in -6i32..=6 {
        for dx in -6i32..=6 {
            let (x, y) = (
                (cx as i32 + dx).clamp(0, SIZE as i32 - 1) as u32,
                (cy as i32 + dy).clamp(0, SIZE as i32 - 1) as u32,
            );
            let index = ((y * SIZE + x) * 4) as usize;
            for channel in 0..3 {
                let difference = (a[index + channel] as f32 - b[index + channel] as f32).abs();
                worst = worst.max(difference);
            }
        }
    }
    worst
}

/// A surface that differs from the default only in roughness, so the
/// specular lobes the models disagree about are the difference that shows.
fn glossy(_registry: &NodeRegistry, name: &str, roughness: f32) -> Graph {
    let mut graph = Graph::new(name);
    let output = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
    graph.set_param(output, "roughness", Value::F32(roughness));
    graph
}

/// Everything the tests draw with.
struct Scene {
    gpu: GpuContext,
    target: OffscreenTarget,
    renderer: Renderer,
    registry: NodeRegistry,
    quad: Mesh,
}

impl Scene {
    fn new(gpu: GpuContext) -> Self {
        Self::with_lighting(
            gpu,
            wxsl::core::lighting::default_set().expect("the shipped models"),
        )
    }

    /// A scene running the deferred pipeline under `set`, which every
    /// material in it must have been compiled against.
    fn with_lighting(gpu: GpuContext, set: LightingSet) -> Self {
        let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
        let mut renderer = Renderer::new(
            &gpu.device,
            wxsl::stdlib_library(),
            TargetConfig::new(SIZE, SIZE, target.format()),
        )
        .expect("the stdlib library satisfies the ABI");
        renderer
            .set_lighting(set)
            .expect("the shipped models are in the library");
        // The tests read the deferred pipeline: the id channel, the
        // dispatch, the extra targets.
        renderer.set_pipeline(StockPipeline::Deferred);
        let quad = Mesh::plane(&gpu.device, QUAD);
        Scene {
            gpu,
            target,
            renderer,
            registry: wxsl::stdlib::registry(),
            quad,
        }
    }

    fn material(&self, graph: &Graph, options: &MaterialOptions) -> Material {
        Material::with_lighting(graph, &self.registry, options, self.renderer.lighting())
            .expect("the graph compiles")
    }

    /// Draw one quad per slot, each with its own material, and read the
    /// image back.
    fn render(&mut self, materials: &[&Material]) -> Vec<u8> {
        let items: Vec<DrawItem> = SLOTS
            .iter()
            .zip(materials)
            .map(|(x, material)| {
                DrawItem::new(&self.quad, material)
                    .with_transform(Mat4::from_translation(Vec3::new(*x, 0.0, 0.0)))
            })
            .collect();
        let draws: DrawList<'_> = items.into_iter().collect();
        self.renderer
            .render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view: self.target.view(),
                    environment: &lit(),
                    draws: &draws,
                },
            )
            .expect("the frame renders");
        self.gpu.wait();
        self.target.read_rgba8(&self.gpu.device, &self.gpu.queue)
    }
}

fn named(name: &str) -> MaterialOptions {
    MaterialOptions {
        lighting: Some(name.to_string()),
        ..MaterialOptions::default()
    }
}

/// The acceptance test: three objects shaded by three different models
/// through one deferred lighting pass — and the same three through the
/// forward path, agreeing.
///
/// Each model also gets a frame of its own (all three slots the same
/// material) so that a pixel difference between two frames is purely the
/// model's answer, with no per-slot lighting differences to subtract.
#[test]
fn three_models_shade_three_objects_through_one_deferred_pass() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let graph = glossy(&scene.registry, "shaded", 0.4);
    let models: Vec<Material> = ["lambert", "phong", "pbr"]
        .iter()
        .map(|name| scene.material(&graph, &named(name)))
        .collect();

    // Mixed in one frame — the literal acceptance picture — through both
    // paths.
    let mixed = scene.render(&[&models[0], &models[1], &models[2]]);
    scene.renderer.set_pipeline(StockPipeline::Forward);
    let mixed_forward = scene.render(&[&models[0], &models[1], &models[2]]);
    // One model per frame, for the model-vs-model comparisons.
    let deferred: Vec<Vec<u8>> = models.iter().map(|m| scene.render(&[m, m, m])).collect();

    let centers: Vec<Vec3> = SLOTS.iter().map(|x| Vec3::new(*x, 0.0, 0.0)).collect();
    for (index, name) in ["lambert", "phong", "pbr"].into_iter().enumerate() {
        let d = color_at(&mixed, centers[index]);
        let f = color_at(&mixed_forward, centers[index]);
        println!("{name}: deferred {d:?} forward {f:?}");
        assert!(
            gap(d, f) <= 4.0,
            "{name}: forward and deferred disagree at its slot: {f:?} vs {d:?}"
        );
    }
    // The three models answer differently for the same surface under the
    // same lights: Lambertian only, diffuse plus a Phong lobe, and
    // Cook-Torrance. Around the highlight their lobes are not the same
    // curve, which is where the comparison looks — the same pixel of two
    // one-model frames, worst channel.
    for (a, b) in [(0, 1), (1, 2), (0, 2)] {
        let difference = patch_gap(&deferred[a], &deferred[b], centers[a]);
        println!("gap {a}-{b}: {difference}");
        assert!(
            difference > 8.0,
            "models {a} and {b} shade the same surface to the same colour, which \
             would mean the dispatch drew one of them twice"
        );
    }
}

/// The id channel exists only when the set dispatches — and costs no
/// pixel: the same PBR scene under a one-model pipeline and under the full
/// set renders the same, because the only thing the extra channel carries
/// is who to ask.
#[test]
fn the_dispatch_channel_changes_no_pbr_pixel() {
    let Some(gpu) = gpu() else { return };
    let single_gpu = pollster::block_on(GpuContext::headless()).expect("second context");
    let mut single = Scene::with_lighting(single_gpu, {
        LightingSet::single(DEFAULT_MODELS[DEFAULT_MODEL_ID as usize])
    });
    let mut full = Scene::new(gpu);

    // The layout is where the difference lives: base targets, versus base
    // plus the id channel plus the clearcoat model's request.
    assert_eq!(
        full.renderer
            .render_graph()
            .passes()
            .iter()
            .find(|pass| matches!(
                pass.kind,
                wxsl::render::pass::PassKind::Geometry {
                    stage: abi::MaterialStage::GBUFFER,
                    ..
                }
            ))
            .expect("deferred material pass")
            .color
            .len(),
        full.renderer.lighting().gbuffer_layout().len(),
    );
    assert!(full.renderer.lighting().dispatches());

    let graph = glossy(&single.registry, "plain", 0.5);
    let material = single.material(&graph, &MaterialOptions::default());
    let full_material = full.material(&graph, &MaterialOptions::default());

    let one_model = single.render(&[&material, &material, &material]);
    let every_model = full.render(&[&full_material, &full_material, &full_material]);
    for x in SLOTS {
        let a = color_at(&one_model, Vec3::new(x, 0.0, 0.0));
        let b = color_at(&every_model, Vec3::new(x, 0.0, 0.0));
        assert!(
            gap(a, b) <= 2.0,
            "the id channel changed the pixel at x = {x}: {a:?} vs {b:?}"
        );
    }
}

/// Enabling the clearcoat model adds its target, and its second lobe
/// changes what a smooth surface shades to.
#[test]
fn the_clearcoat_model_uses_the_extra_target_it_asks_for() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let coat = scene
        .renderer
        .lighting()
        .by_name("clearcoat")
        .expect("the full set enables it");
    assert!(
        coat.extra.is_some(),
        "the clearcoat model is the one that proves the composition"
    );
    assert!(scene
        .renderer
        .lighting()
        .gbuffer_layout()
        .iter()
        .any(|target| target.field == "clearcoat"));

    let graph = glossy(&scene.registry, "lacquered", 0.3);
    let pbr = scene.material(&graph, &named("pbr"));
    let clearcoat = scene.material(&graph, &named("clearcoat"));

    // One model per frame: any pixel difference between the two is the
    // coat's second lobe, fed by the target the model asked for.
    let without = scene.render(&[&pbr, &pbr, &pbr]);
    let with = scene.render(&[&clearcoat, &clearcoat, &clearcoat]);
    let difference = patch_gap(&without, &with, Vec3::new(0.0, 0.0, 0.0));
    println!("clearcoat gap: {difference}");
    assert!(
        difference > 8.0,
        "a surface shaded with and without its coat differs nowhere: \
         the extra target's data is not reaching the model"
    );
}

/// A material naming a model the set does not enable is reported at
/// compile time, naming the model.
#[test]
fn a_model_outside_the_set_is_reported_at_compile_time() {
    let Some(gpu) = gpu() else { return };
    let gpu_context = gpu;
    let target = OffscreenTarget::new(&gpu_context.device, SIZE, SIZE);
    let renderer = Renderer::new(
        &gpu_context.device,
        wxsl::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, target.format()),
    )
    .expect("the stdlib library satisfies the ABI");
    // The default renderer state: one model, the default.
    let set = renderer.lighting().clone();
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("misfiled");
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let error = Material::with_lighting(&graph, &registry, &named("lambert"), &set)
        .expect_err("the single-model set has no lambert");
    let message = error.to_string();
    assert!(message.contains("lambert"), "{message}");
}

/// A frame whose materials were compiled against a different set than the
/// renderer runs is a named error naming both, not a `wgpu` complaint
/// about a fragment target count.
#[test]
fn a_frame_mismatched_with_the_renderers_set_is_reported_by_name() {
    let Some(gpu) = gpu() else { return };
    let single_gpu = pollster::block_on(GpuContext::headless()).expect("second context");
    let single_set = LightingSet::single(DEFAULT_MODELS[DEFAULT_MODEL_ID as usize]);
    let mut scene = Scene::new(gpu);
    let single = Scene::with_lighting(single_gpu, single_set);

    let graph = glossy(&scene.registry, "from elsewhere", 0.5);
    let stranger = single.material(&graph, &MaterialOptions::default());

    let items = vec![DrawItem::new(&scene.quad, &stranger)];
    let draws: DrawList<'_> = items.into_iter().collect();
    let error = scene
        .renderer
        .render(
            &scene.gpu.device,
            &scene.gpu.queue,
            &RenderRequest {
                view: scene.target.view(),
                environment: &lit(),
                draws: &draws,
            },
        )
        .expect_err("the sets disagree");
    let message = error.to_string();
    assert!(message.contains("models="), "{message}");
    assert!(message.contains(&stranger.name), "{message}");
}
