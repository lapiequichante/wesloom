//! Shadows, on real hardware: a caster fills a slice of the shadow map
//! array from its light's point of view, and a receiver reads it back
//! (ADR 0026).
//!
//! The two tests that close M5 are the last two here, and they are the
//! ones that only pass if the *partitioning* of ADR 0025 is right: a
//! shadow pass writes no colour, so a material's alpha test and its vertex
//! displacement reach it only because those are separate subgraphs
//! compiled into a stage that wants no surface at all. Everything before
//! them is the scaffolding those two need in order to mean anything.
//!
//! # Reading a pixel
//!
//! The scene is a big ground plane with a small quad floating over it and
//! one directional light straight overhead, so a caster's shadow lands
//! directly beneath it. A test says which *world* point it wants to look
//! at and [`ground_pixel`] projects it, which is what keeps the assertions
//! about geometry rather than about where the camera happens to be.

use glam::{Mat4, Vec3, Vec4Swizzles};
use wxsl::core::abi;
use wxsl::core::graph::{Graph, Node};
use wxsl::core::node::{NodeRegistry, Value, ValueType};
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::material::{Material, MaterialOptions};
use wxsl::render::pipeline::StockPipeline;
use wxsl::render::{
    Camera, DrawItem, DrawList, Environment, Light, Mesh, RenderRequest, Renderer, TargetConfig,
};

mod probe;
use probe::{gpu, SIZE};

/// How high the caster floats over the ground.
const CASTER_HEIGHT: f32 = 1.0;
/// Edge of the caster quad; its shadow is the same square on the ground.
const CASTER_SIZE: f32 = 1.0;
/// How far the displacing material pushes its vertices along +x.
const DISPLACEMENT: f32 = 2.0;

/// The camera every test in this file looks through.
///
/// Off to one side and above, so that the ground under the caster is
/// visible past it: a camera on the light's own axis would only ever see
/// the caster.
fn camera() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 3.0, 5.0),
        target: Vec3::ZERO,
        aspect: 1.0,
        ..Camera::default()
    }
}

/// One directional light straight overhead, casting.
///
/// Straight overhead so that a caster's shadow is directly beneath it and
/// a test can name the world point it expects to be dark. The ambient is
/// deliberately non-zero: a shadowed pixel should be *darker*, not black,
/// which is also what stops the test passing on a frame that drew nothing.
fn lit(casting: bool) -> Environment {
    let light = Light::directional(Vec3::Y, Vec3::ONE, 3.0);
    Environment {
        camera: camera(),
        lights: vec![if casting {
            light.casting_shadow(4.0)
        } else {
            light
        }],
        ambient_sky: Vec3::splat(0.12),
        ambient_ground: Vec3::splat(0.04),
        exposure: 1.0,
        time: 0.0,
        previous_time: 0.0,
    }
}

/// Where a point on the ground plane lands in the image.
fn ground_pixel(world: Vec3) -> (u32, u32) {
    let clip = camera().view_proj() * world.extend(1.0);
    let ndc = clip.xyz() / clip.w;
    let x = ((ndc.x * 0.5 + 0.5) * SIZE as f32).round();
    let y = ((0.5 - ndc.y * 0.5) * SIZE as f32).round();
    (
        (x.max(0.0) as u32).min(SIZE - 1),
        (y.max(0.0) as u32).min(SIZE - 1),
    )
}

/// Perceived brightness of the ground at `world`, 0..255.
///
/// The green channel: the light and the surface are both neutral, so any
/// channel would do, and one number reads better in an assertion than
/// three.
fn brightness(image: &[u8], world: Vec3) -> u8 {
    let (x, y) = ground_pixel(world);
    let index = ((y * SIZE + x) * 4) as usize;
    image[index + 1]
}

/// A material that does nothing but be a surface.
fn plain(registry: &NodeRegistry, name: &str) -> Graph {
    let mut graph = Graph::new(name);
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let _ = registry;
    graph
}

/// A material that throws away every fragment left of `world_position.x`
/// = 0 — half the caster, so half its shadow.
///
/// Driven by world position rather than by UV so that it says the same
/// thing about the quad whichever way the mesh happens to be wound.
fn perforated(registry: &NodeRegistry) -> Graph {
    let mut graph = Graph::new("perforated");
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let position = graph.add_node("input.world_position");
    let split = graph.add_node("convert.split.vec3f");
    let compare = graph.add_node("compare.less");
    let discard = graph.add_node(abi::DISCARD_OUTPUT_ID);
    graph
        .wire(registry, (position, "out"), (split, "v"))
        .expect("a vec3f splits");
    graph
        .wire(registry, (split, "x"), (compare, "a"))
        .expect("x is a scalar");
    graph.set_param(compare, "b", Value::F32(0.0));
    graph
        .wire(registry, (compare, "out"), (discard, abi::SOCKET_DISCARD))
        .expect("a comparison is a bool");
    graph
}

/// A material that moves its vertices `DISPLACEMENT` along +x in object
/// space — and therefore moves its shadow the same way.
fn displacing(registry: &NodeRegistry) -> Graph {
    let mut graph = Graph::new("displacing");
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let offset = graph.add(Node::new("const.value"));
    graph
        .set_generic(registry, offset, "T", ValueType::Vec3)
        .expect("vec3f is allowed");
    graph.set_param(offset, "value", Value::Vec3([DISPLACEMENT, 0.0, 0.0]));
    let vertex = graph.add_node(abi::VERTEX_OUTPUT_ID);
    graph
        .wire(
            registry,
            (offset, "out"),
            (vertex, abi::SOCKET_POSITION_OFFSET),
        )
        .expect("the offset is a vec3f");
    graph
}

/// Everything one of these tests draws with.
struct Scene {
    gpu: GpuContext,
    target: OffscreenTarget,
    renderer: Renderer,
    registry: NodeRegistry,
    ground: Mesh,
    caster: Mesh,
}

impl Scene {
    fn new(gpu: GpuContext) -> Self {
        let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
        let renderer = Renderer::new(
            &gpu.device,
            wxsl::stdlib_library(),
            TargetConfig::new(SIZE, SIZE, target.format()),
        )
        .expect("the stdlib library satisfies the ABI");
        let ground = Mesh::plane(&gpu.device, 8.0);
        let caster = Mesh::plane(&gpu.device, CASTER_SIZE);
        Scene {
            gpu,
            target,
            renderer,
            registry: wxsl::stdlib::registry(),
            ground,
            caster,
        }
    }

    fn material(&self, graph: &Graph, options: &MaterialOptions) -> Material {
        Material::with_options(graph, &self.registry, options).expect("the graph compiles")
    }

    /// Draw the ground with `ground` and the floating quad with `caster`,
    /// and read the image back.
    fn render(
        &mut self,
        environment: &Environment,
        ground: &Material,
        caster: Option<&Material>,
    ) -> Vec<u8> {
        let mut items = vec![DrawItem::new(&self.ground, ground)];
        if let Some(caster) = caster {
            items.push(
                DrawItem::new(&self.caster, caster)
                    .with_transform(Mat4::from_translation(Vec3::Y * CASTER_HEIGHT)),
            );
        }
        let draws: DrawList<'_> = items.into_iter().collect();
        self.renderer
            .render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view: self.target.view(),
                    environment,
                    draws: &draws,
                },
            )
            .expect("the frame renders");
        self.gpu.wait();
        self.target.read_rgba8(&self.gpu.device, &self.gpu.queue)
    }
}

/// How much darker a pixel has to be than the unshadowed reference before
/// a test will call it shadowed.
///
/// The direct term is three times the ambient, so a shadowed pixel is a
/// long way down; anything close to the reference is the shadow having
/// missed entirely rather than a filtering difference.
const SHADOW_DROP: u8 = 40;

#[test]
fn a_caster_darkens_the_ground_beneath_it_and_nowhere_else() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let plain_graph = plain(&scene.registry, "ground");
    let ground = scene.material(&plain_graph, &MaterialOptions::default());
    let caster = scene.material(&plain_graph, &MaterialOptions::default());

    // The same frame twice, once with the light casting and once not, so
    // the comparison is against this exact scene rather than a constant
    // somebody has to keep up to date.
    let unshadowed = scene.render(&lit(false), &ground, Some(&caster));
    let shadowed = scene.render(&lit(true), &ground, Some(&caster));

    let under = Vec3::ZERO;
    let beside = Vec3::new(0.0, 0.0, 2.0);
    assert!(
        brightness(&unshadowed, under) > SHADOW_DROP,
        "the ground under the caster is lit when nothing casts"
    );
    assert!(
        brightness(&shadowed, under) + SHADOW_DROP < brightness(&unshadowed, under),
        "under the caster: {} should be far below {}",
        brightness(&shadowed, under),
        brightness(&unshadowed, under),
    );
    // And the light has not simply gone out: ground the caster does not
    // cover is as bright as it was.
    assert_eq!(
        brightness(&shadowed, beside),
        brightness(&unshadowed, beside),
        "ground away from the caster is untouched"
    );
}

#[test]
fn a_light_that_casts_nothing_costs_a_clear_and_leaves_the_scene_lit() {
    // The shadow passes are in the pass list whether or not any light is
    // casting — a pass list is built once and scheduled against every
    // frame — so the "no shadows" case has to be a *cleared* slice that
    // reads as fully lit, not a special path.
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let plain_graph = plain(&scene.registry, "ground");
    let ground = scene.material(&plain_graph, &MaterialOptions::default());

    let mut environment = lit(true);
    environment.lights.clear();
    let dark = scene.render(&environment, &ground, None);

    let mut environment = lit(true);
    environment.lights[0].casts_shadow = false;
    let lit_image = scene.render(&environment, &ground, None);

    let point = Vec3::ZERO;
    assert!(
        brightness(&lit_image, point) > brightness(&dark, point) + SHADOW_DROP,
        "an unshadowed light still lights the ground"
    );
}

#[test]
fn a_material_that_casts_no_shadow_is_not_drawn_into_one() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let plain_graph = plain(&scene.registry, "ground");
    let ground = scene.material(&plain_graph, &MaterialOptions::default());
    let casting = scene.material(&plain_graph, &MaterialOptions::default());
    let not_casting = scene.material(
        &plain_graph,
        &MaterialOptions {
            cast_shadow: false,
            ..MaterialOptions::default()
        },
    );
    // The two differ in no generated code at all — this is a selection,
    // not a variant — and the test would pass for the wrong reason if
    // they did.
    assert_eq!(
        casting.wxsl(abi::MaterialStage::SHADOW),
        not_casting.wxsl(abi::MaterialStage::SHADOW)
    );

    let with = scene.render(&lit(true), &ground, Some(&casting));
    let without = scene.render(&lit(true), &ground, Some(&not_casting));

    let under = Vec3::ZERO;
    assert!(
        brightness(&with, under) + SHADOW_DROP < brightness(&without, under),
        "cast_shadow = false: {} should be as bright as unshadowed ground",
        brightness(&without, under),
    );
}

#[test]
fn a_material_that_receives_no_shadow_does_not_compile_the_lookup() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let plain_graph = plain(&scene.registry, "ground");
    let receiving = scene.material(&plain_graph, &MaterialOptions::default());
    let ignoring = scene.material(
        &plain_graph,
        &MaterialOptions {
            receive_shadow: false,
            ..MaterialOptions::default()
        },
    );
    // Unlike `cast_shadow`, this one *is* code: the flag is a macro, so
    // the two are different variants and the cache keeps them apart. The
    // generated module only pins the macro — conditional translation
    // happens when the ABI is linked in, so that is where to look.
    assert_ne!(
        receiving.wxsl(abi::MaterialStage::FORWARD_LIT),
        ignoring.wxsl(abi::MaterialStage::FORWARD_LIT)
    );
    let device = &scene.gpu.device;
    let with_lookup = scene
        .renderer
        .material_wgsl_for(device, &receiving, abi::MaterialStage::FORWARD_LIT)
        .expect("the material compiles");
    let without = scene
        .renderer
        .material_wgsl_for(device, &ignoring, abi::MaterialStage::FORWARD_LIT)
        .expect("the material compiles");
    assert!(with_lookup.contains(abi::SHADOW_FACTOR_FN));
    assert!(
        !without.contains(abi::SHADOW_FACTOR_FN),
        "a material that receives no shadow should not compile the lookup"
    );

    let caster = scene.material(&plain_graph, &MaterialOptions::default());
    let receiving_image = scene.render(&lit(true), &receiving, Some(&caster));
    let ignoring_image = scene.render(&lit(true), &ignoring, Some(&caster));

    let under = Vec3::ZERO;
    assert!(
        brightness(&receiving_image, under) + SHADOW_DROP < brightness(&ignoring_image, under),
        "a ground that ignores shadows stays lit under the caster"
    );
}

#[test]
fn forward_and_deferred_shadow_the_same_ground_the_same_way() {
    // The lookup lives in `shading.wxsl`, which the forward stage calls
    // from its fragment entry and the deferred lighting pass calls after
    // unpacking the G-buffer. If the two ever disagree it is because
    // somebody wrote a second copy.
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let plain_graph = plain(&scene.registry, "ground");
    let ground = scene.material(&plain_graph, &MaterialOptions::default());
    let caster = scene.material(&plain_graph, &MaterialOptions::default());

    let forward = scene.render(&lit(true), &ground, Some(&caster));
    scene.renderer.set_pipeline(StockPipeline::Deferred);
    let deferred = scene.render(&lit(true), &ground, Some(&caster));

    for point in [
        Vec3::ZERO,
        Vec3::new(0.0, 0.0, 2.0),
        Vec3::new(1.5, 0.0, 0.0),
    ] {
        let (forward, deferred) = (brightness(&forward, point), brightness(&deferred, point));
        assert!(
            forward.abs_diff(deferred) <= 4,
            "at {point}: forward {forward} vs deferred {deferred}"
        );
    }
}

#[test]
fn an_alpha_discarding_material_casts_a_perforated_shadow() {
    // One of M5's two acceptance tests. The shadow stage writes no colour
    // and has no surface: the alpha test reaches it only because
    // `output.discard` is a terminal of its own, partitioned into a
    // function the shadow module compiles on its own (ADR 0025).
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let plain_graph = plain(&scene.registry, "ground");
    let ground = scene.material(&plain_graph, &MaterialOptions::default());
    let solid = scene.material(&plain_graph, &MaterialOptions::default());
    let holed_graph = perforated(&scene.registry);
    let holed = scene.material(&holed_graph, &MaterialOptions::default());

    // The half that survives the discard, and the half that does not.
    let kept = Vec3::new(0.3, 0.0, 0.0);
    let thrown = Vec3::new(-0.3, 0.0, 0.0);

    // The same ground with nothing over it, which is what "lit" means
    // here — no constant to keep up to date.
    let bare = scene.render(&lit(true), &ground, None);
    let solid_image = scene.render(&lit(true), &ground, Some(&solid));
    let holed_image = scene.render(&lit(true), &ground, Some(&holed));

    // A solid caster shadows both halves.
    for point in [kept, thrown] {
        assert!(
            brightness(&solid_image, point) + SHADOW_DROP < brightness(&bare, point),
            "a solid caster shadows {point}: {} against {}",
            brightness(&solid_image, point),
            brightness(&bare, point),
        );
    }
    // The perforated one shadows only the half it kept — the hole is a
    // hole, all the way through to the ground.
    assert!(
        brightness(&holed_image, kept) + SHADOW_DROP < brightness(&bare, kept),
        "the surviving half still casts: {} against {}",
        brightness(&holed_image, kept),
        brightness(&bare, kept),
    );
    assert_eq!(
        brightness(&holed_image, thrown),
        brightness(&bare, thrown),
        "the discarded half casts nothing at all"
    );
}

#[test]
fn a_displacing_material_casts_a_displaced_shadow() {
    // M5's other acceptance test, and the vertex half of the same claim:
    // the shadow pass runs `wxsl_vertex`, so the shadow is of the shape
    // the graph made rather than of the shape the mesh came as.
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let plain_graph = plain(&scene.registry, "ground");
    let ground = scene.material(&plain_graph, &MaterialOptions::default());
    let still = scene.material(&plain_graph, &MaterialOptions::default());
    let moving_graph = displacing(&scene.registry);
    let moving = scene.material(&moving_graph, &MaterialOptions::default());

    // Where the mesh is, and where the graph puts it.
    let origin = Vec3::ZERO;
    let displaced = Vec3::new(DISPLACEMENT, 0.0, 0.0);

    let bare = scene.render(&lit(true), &ground, None);
    let still_image = scene.render(&lit(true), &ground, Some(&still));
    let moving_image = scene.render(&lit(true), &ground, Some(&moving));

    assert!(
        brightness(&still_image, origin) + SHADOW_DROP < brightness(&bare, origin),
        "the undisplaced caster shadows the origin: {} against {}",
        brightness(&still_image, origin),
        brightness(&bare, origin),
    );
    assert!(
        brightness(&moving_image, displaced) + SHADOW_DROP < brightness(&bare, displaced),
        "the displaced caster shadows where the graph moved it: {} against {}",
        brightness(&moving_image, displaced),
        brightness(&bare, displaced),
    );
    assert_eq!(
        brightness(&moving_image, origin),
        brightness(&bare, origin),
        "and no longer shadows where the mesh was"
    );
}
