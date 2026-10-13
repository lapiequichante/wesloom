//! Shadows, on real hardware: a caster fills its light's atlas tile and
//! a receiver reads it back (ADRs 0026, 0066).
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
use wxsl::render::material::{Material, MaterialConfig};
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
        previous_camera: None,
        origin: Default::default(),
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

    fn material(&self, graph: &Graph, options: &MaterialConfig) -> Material {
        Material::with_config(graph, &self.registry, options).expect("the graph compiles")
    }

    /// Draw the ground with `ground` and the floating quad with `caster`,
    /// and read the image back.
    fn render(
        &mut self,
        environment: &Environment,
        ground: &Material,
        caster: Option<&Material>,
    ) -> Vec<u8> {
        self.render_lowered(
            environment,
            ground,
            caster,
            Mat4::from_translation(Vec3::ZERO),
        )
    }

    /// The same, with the ground plane moved by `floor` — the gallery's
    /// floor hangs below the origin, and the difference has to be
    /// reachable from the tests that pin it.
    fn render_lowered(
        &mut self,
        environment: &Environment,
        ground: &Material,
        caster: Option<&Material>,
        floor: Mat4,
    ) -> Vec<u8> {
        let mut items = vec![DrawItem::new(&self.ground, ground).with_transform(floor)];
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
fn every_atlas_slot_shadows_and_clears_after_its_light_stops_casting() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let graph = plain(&scene.registry, "atlas receiver");
    let ground = scene.material(&graph, &MaterialConfig::default());
    let caster = scene.material(&graph, &MaterialConfig::default());
    for pipeline in [StockPipeline::Forward, StockPipeline::Deferred] {
        scene.renderer.set_pipeline(pipeline);
        for slot in 0..abi::MAX_LIGHTS {
            let mut environment = lit(true);
            let light = environment.lights.remove(0);
            environment.lights = vec![Light::directional(Vec3::Y, Vec3::ONE, 0.0); abi::MAX_LIGHTS];
            environment.lights[slot] = light;
            let shadow = scene.render(&environment, &ground, Some(&caster));
            environment.lights[slot].casts_shadow = false;
            let clear = scene.render(&environment, &ground, Some(&caster));
            assert!(
                brightness(&shadow, Vec3::ZERO) + SHADOW_DROP < brightness(&clear, Vec3::ZERO),
                "{} slot {slot} must shadow",
                pipeline.name()
            );
            assert_eq!(
                brightness(&shadow, Vec3::new(0.0, 0.0, 2.0)),
                brightness(&clear, Vec3::new(0.0, 0.0, 2.0))
            );
        }
    }
}

#[test]
fn a_caster_darkens_the_ground_beneath_it_and_nowhere_else() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let plain_graph = plain(&scene.registry, "ground");
    let ground = scene.material(&plain_graph, &MaterialConfig::default());
    let caster = scene.material(&plain_graph, &MaterialConfig::default());

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

/// The cascaded twin of the test above: the same sun, the same caster, the
/// shadow split over four camera-fitted cascades instead of one box.
///
/// The second half moves the camera: a cascade is fitted to the view, so
/// the shadow has to still be under the caster afterwards — a fixed box
/// would pass the first half and smear away on the second.
#[test]
fn four_cascades_shadow_the_ground_and_follow_the_camera() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let graph = plain(&scene.registry, "cascade receiver");
    let ground = scene.material(&graph, &MaterialConfig::default());
    let caster = scene.material(&graph, &MaterialConfig::default());

    let sun = |casting: bool| {
        let mut light = Light::directional(Vec3::Y, Vec3::ONE, 3.0)
            .casting_shadow(4.0)
            .with_cascades(4);
        light.casts_shadow = casting;
        let mut environment = lit(false);
        environment.lights = vec![light];
        environment
    };
    let unshadowed = scene.render(&sun(false), &ground, Some(&caster));
    let shadowed = scene.render(&sun(true), &ground, Some(&caster));

    let under = Vec3::ZERO;
    let beside = Vec3::new(0.0, 0.0, 2.0);
    assert!(
        brightness(&shadowed, under) + SHADOW_DROP < brightness(&unshadowed, under),
        "the nearest cascade must shadow under the caster: {} vs {}",
        brightness(&shadowed, under),
        brightness(&unshadowed, under),
    );
    assert_eq!(
        brightness(&shadowed, beside),
        brightness(&unshadowed, beside),
        "ground away from the caster is untouched"
    );

    // The camera walks away and looks back: every cascade is refitted, and
    // the caster's shadow comes with it.
    let mut moved = sun(true);
    moved.camera.eye = Vec3::new(4.0, 2.5, 6.0);
    moved.camera.target = Vec3::ZERO;
    let followed = scene.render(&moved, &ground, Some(&caster));
    let mut moved_uncast = moved;
    moved_uncast.lights[0].casts_shadow = false;
    let unshadowed_moved = scene.render(&moved_uncast, &ground, Some(&caster));
    assert!(
        brightness(&followed, under) + SHADOW_DROP < brightness(&unshadowed_moved, under),
        "the shadow follows the refitted cascades: {} vs {}",
        brightness(&followed, under),
        brightness(&unshadowed_moved, under),
    );
}

/// A slanted sun must not darken an unobstructed floor as the camera's
/// depth crosses the split and its blend band, even on low-resolution tiles.
#[test]
fn cascaded_floor_has_no_acne_across_splits() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    scene.ground = Mesh::plane(&scene.gpu.device, 80.0);
    let ground = scene.material(
        &plain(&scene.registry, "cascade floor"),
        &MaterialConfig::default(),
    );
    let mut environment = lit(false);
    environment.camera = Camera {
        eye: Vec3::new(1.0, 6.0, 10.5),
        target: Vec3::new(0.0, -0.8, 0.0),
        ..Camera::default()
    };
    // Six preceding tiles force the cascades onto the smaller atlas slots.
    environment.lights = vec![
        Light::point(Vec3::Y, Vec3::ONE, 0.0).casting_shadow(4.0),
        Light::directional(Vec3::new(0.7, 0.75, 0.45), Vec3::ONE, 3.0)
            .casting_shadow(24.0)
            .with_cascades(4),
    ];
    for pipeline in [StockPipeline::Forward, StockPipeline::Deferred] {
        scene.renderer.set_pipeline(pipeline);
        let shadowed = scene.render(&environment, &ground, None);
        let mut uncast = environment.clone();
        uncast.lights[1].casts_shadow = false;
        let reference = scene.render(&uncast, &ground, None);
        let mut checked = 0;
        for (actual, expected) in shadowed.chunks_exact(4).zip(reference.chunks_exact(4)) {
            if expected[1] > 80 {
                assert!(
                    actual[1].abs_diff(expected[1]) <= 1,
                    "unobstructed floor darkened: {} vs {}",
                    actual[1],
                    expected[1]
                );
                checked += 1;
            }
        }
        assert!(checked > 500, "must check visible floor pixels");
    }
}

/// The end of a grazing sun's shadow must stay in place when its atlas
/// texels get coarser; increasing the normal offset would move it inwards.
#[test]
fn coarser_cascades_preserve_the_shadow_tip() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    scene.caster = Mesh::plane(&scene.gpu.device, 4.0);
    scene.ground = Mesh::plane(&scene.gpu.device, 40.0);
    let graph = plain(&scene.registry, "shadow length");
    let ground = scene.material(&graph, &MaterialConfig::default());
    let caster = scene.material(&graph, &MaterialConfig::default());
    let mut environment = lit(false);
    environment.camera.eye.x = -2.0;
    environment.camera.target.x = -2.0;
    let mut sun = Light::directional(Vec3::new(1.0, 0.4, 0.0), Vec3::ONE, 5.0).casting_shadow(4.0);
    sun.shadow_distance = 12.0;
    sun.shadow_normal_bias = 0.005;
    environment.lights = vec![
        Light::point(Vec3::Y, Vec3::ONE, 0.0).casting_shadow(4.0),
        sun,
    ];
    // Quad edge x=-2 at height 1 casts its tip at x=-4.5.
    let clip = environment.camera.view_proj() * Vec3::new(-4.2, 0.0, 0.0).extend(1.0);
    let ndc = clip.xyz() / clip.w;
    let x = ((ndc.x * 0.5 + 0.5) * SIZE as f32) as u32;
    let y = ((0.5 - ndc.y * 0.5) * SIZE as f32) as u32;
    assert!(x < SIZE && y < SIZE);
    let pixel = ((y * SIZE + x) * 4 + 1) as usize;
    for count in [2, 3, 4] {
        environment.lights[1].shadow_cascades = count;
        let shadowed = scene.render(&environment, &ground, Some(&caster));
        let mut uncast = environment.clone();
        uncast.lights[1].casts_shadow = false;
        let reference = scene.render(&uncast, &ground, Some(&caster));
        assert!(
            shadowed[pixel] + SHADOW_DROP < reference[pixel],
            "shadow tip lost with {count} cascades: {} vs {}",
            shadowed[pixel],
            reference[pixel]
        );
    }
}

/// A point light now casts through all six of its faces: a floating plate
/// under the light darkens the ground beneath it, on every pipeline.
///
/// The light sits straight above, so the ground under it samples the +y
/// face and the other five contribute nothing — one tile of the six does
/// all the work, which is also the assertion that the face picked by
/// dominant axis is the face the pass drew.
#[test]
fn a_point_light_casts_through_its_faces() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let graph = plain(&scene.registry, "point receiver");
    let ground = scene.material(&graph, &MaterialConfig::default());
    let caster = scene.material(&graph, &MaterialConfig::default());

    let environment = |casting: bool| {
        let mut light = Light::point(Vec3::new(0.0, 2.5, 0.0), Vec3::ONE, 12.0);
        light.shadow_distance = 8.0;
        light.casts_shadow = casting;
        let mut environment = lit(false);
        environment.lights = vec![light];
        environment
    };
    for pipeline in [StockPipeline::Forward, StockPipeline::Deferred] {
        scene.renderer.set_pipeline(pipeline);
        let unshadowed = scene.render(&environment(false), &ground, Some(&caster));
        let shadowed = scene.render(&environment(true), &ground, Some(&caster));
        assert!(
            brightness(&shadowed, Vec3::ZERO) + SHADOW_DROP < brightness(&unshadowed, Vec3::ZERO),
            "{}: the plate must shadow the ground beneath the light",
            pipeline.name()
        );
        // Off the plate's projection (its shadow's half-edge is
        // 0.5 * 2.5 / 1.5 ≈ 0.83), the ground is untouched.
        assert_eq!(
            brightness(&shadowed, Vec3::new(0.0, 0.0, 2.0)),
            brightness(&unshadowed, Vec3::new(0.0, 0.0, 2.0)),
            "{}: ground away from the plate is untouched",
            pipeline.name()
        );
    }
}

/// The normal offset can move a receiver across a cube-face boundary.
/// A fully shadowed strip straddling that boundary must stay dark.
#[test]
fn point_shadow_has_no_lit_gap_at_a_cube_face_edge() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let size = 256;
    scene.target = OffscreenTarget::new(&scene.gpu.device, size, size);
    scene.renderer = Renderer::new(
        &scene.gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(size, size, scene.target.format()),
    )
    .unwrap();
    scene.caster = Mesh::plane(&scene.gpu.device, 2.0);
    let graph = plain(&scene.registry, "cube edge receiver");
    let ground = scene.material(&graph, &MaterialConfig::default());
    let caster = scene.material(&graph, &MaterialConfig::default());
    let mut environment = lit(false);
    environment.camera.eye = Vec3::new(2.0, 3.0, 5.0);
    environment.camera.target = Vec3::new(2.0, 0.0, 0.0);
    environment.lights =
        vec![Light::point(Vec3::new(0.0, 2.0, 0.0), Vec3::ONE, 30.0).casting_shadow(4.0)];
    let render = |scene: &mut Scene, environment: &Environment| {
        let draws: DrawList<'_> = vec![
            DrawItem::new(&scene.ground, &ground),
            DrawItem::new(&scene.caster, &caster)
                .with_transform(Mat4::from_translation(Vec3::new(1.0, 1.0, 0.0))),
        ]
        .into_iter()
        .collect();
        scene
            .renderer
            .render(
                &scene.gpu.device,
                &scene.gpu.queue,
                &RenderRequest {
                    view: scene.target.view(),
                    environment,
                    draws: &draws,
                },
            )
            .unwrap();
        scene.gpu.wait();
        scene.target.read_rgba8(&scene.gpu.device, &scene.gpu.queue)
    };
    for pipeline in [StockPipeline::Forward, StockPipeline::Deferred] {
        scene.renderer.set_pipeline(pipeline);
        let shadowed = render(&mut scene, &environment);
        let mut uncast = environment.clone();
        uncast.lights[0].casts_shadow = false;
        let reference = render(&mut scene, &uncast);
        // The plate projects to x=[0,4], z=[-2,2]. The -y/+x cube
        // boundary crosses its interior at x=2, far from the shadow edge.
        for x in [1.92, 1.95, 1.98, 2.0, 2.02, 2.05, 2.08] {
            for z in [-0.3, 0.0, 0.3] {
                let clip = environment.camera.view_proj() * Vec3::new(x, 0.0, z).extend(1.0);
                let ndc = clip.xyz() / clip.w;
                let px = ((ndc.x * 0.5 + 0.5) * size as f32) as u32;
                let py = ((0.5 - ndc.y * 0.5) * size as f32) as u32;
                assert!(px < size && py < size);
                let pixel = ((py * size + px) * 4 + 1) as usize;
                assert!(
                    shadowed[pixel] + SHADOW_DROP < reference[pixel],
                    "{}: cube edge ({x}, {z}) is lit: {} vs {}",
                    pipeline.name(),
                    shadowed[pixel],
                    reference[pixel]
                );
            }
        }
    }
}

/// Finite emission has a fully occluded centre, partially visible fringe,
/// and no emission from its back. Both pipelines integrate the same samples.
#[test]
fn rectangular_area_light_casts_a_penumbra_and_emits_on_one_side() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let graph = plain(&scene.registry, "area receiver");
    let ground = scene.material(&graph, &MaterialConfig::default());
    let caster = scene.material(&graph, &MaterialConfig::default());
    let area = Light::rectangle(
        Vec3::new(0.0, 2.5, 0.0),
        Vec3::X * 1.5,
        Vec3::Z * 0.5,
        Vec3::ONE,
        12.0,
    )
    .casting_shadow(4.0);
    let mut env = lit(false);
    env.lights = vec![area];
    for pipeline in [StockPipeline::Forward, StockPipeline::Deferred] {
        scene.renderer.set_pipeline(pipeline);
        let empty_shadow = scene.render(&env, &ground, None);
        let mut uncast_empty = env.clone();
        uncast_empty.lights[0].casts_shadow = false;
        let empty_reference = scene.render(&uncast_empty, &ground, None);
        for (actual, expected) in empty_shadow
            .chunks_exact(4)
            .zip(empty_reference.chunks_exact(4))
        {
            assert!(
                actual[1].abs_diff(expected[1]) <= 1,
                "unobstructed area-lit floor darkened: {} vs {}",
                actual[1],
                expected[1]
            );
        }
        let shadowed = scene.render(&env, &ground, Some(&caster));
        let mut uncast = env.clone();
        uncast.lights[0].casts_shadow = false;
        let lit = scene.render(&uncast, &ground, Some(&caster));
        let centre_drop =
            i16::from(brightness(&lit, Vec3::ZERO)) - i16::from(brightness(&shadowed, Vec3::ZERO));
        let fringe = Vec3::new(1.0, 0.0, 0.0);
        let fringe_drop =
            i16::from(brightness(&lit, fringe)) - i16::from(brightness(&shadowed, fringe));
        assert!(
            centre_drop > i16::from(SHADOW_DROP),
            "area must cast an umbra: {centre_drop}"
        );
        assert!(
            fringe_drop > 5 && fringe_drop < centre_drop - 10,
            "area must cast a partially occluded fringe: {fringe_drop} vs {centre_drop}"
        );
        let mut back = env.clone();
        back.lights[0].area_v = -back.lights[0].area_v;
        let backside = scene.render(&back, &ground, None);
        back.lights.clear();
        let ambient = scene.render(&back, &ground, None);
        assert_eq!(backside, ambient, "back of the rectangle emits nothing");
    }
}

#[test]
fn spotlight_cone_cookie_and_shadow_agree_on_both_paths() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let graph = plain(&scene.registry, "spot receiver");
    let ground = scene.material(&graph, &MaterialConfig::default());
    let caster = scene.material(&graph, &MaterialConfig::default());
    let mut env = lit(false);
    env.lights = vec![Light::spot(
        Vec3::new(0.0, 2.5, 0.0),
        Vec3::NEG_Y,
        20.0_f32.to_radians(),
        40.0_f32.to_radians(),
        Vec3::ONE,
        20.0,
    )
    .casting_shadow(4.0)];
    let cookie = scene
        .gpu
        .device
        .create_texture(&wxsl::render::wgpu::TextureDescriptor {
            label: Some("half masked spot cookie"),
            size: wxsl::render::wgpu::Extent3d {
                width: 2,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wxsl::render::wgpu::TextureDimension::D2,
            format: wxsl::render::wgpu::TextureFormat::Rgba8Unorm,
            usage: wxsl::render::wgpu::TextureUsages::TEXTURE_BINDING
                | wxsl::render::wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
    let view = cookie.create_view(&wxsl::render::wgpu::TextureViewDescriptor {
        dimension: Some(wxsl::render::wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    scene.gpu.queue.write_texture(
        cookie.as_image_copy(),
        &[255, 255, 255, 255, 0, 0, 0, 255],
        wxsl::render::wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(1),
        },
        cookie.size(),
    );
    scene
        .renderer
        .set_light_cookies(&scene.gpu.device, Some(&view));
    for pipeline in [StockPipeline::Forward, StockPipeline::Deferred] {
        scene.renderer.set_pipeline(pipeline);
        let open = scene.render(&env, &ground, None);
        let shadow = scene.render(&env, &ground, Some(&caster));
        assert!(brightness(&shadow, Vec3::ZERO) + SHADOW_DROP < brightness(&open, Vec3::ZERO));
        let mut masked = env.clone();
        masked.lights[0].cookie_layer = Some(0);
        let image = scene.render(&masked, &ground, None);
        let white = Vec3::new(1.1, 0.0, 0.0);
        let black = Vec3::new(-1.1, 0.0, 0.0);
        assert!(
            brightness(&image, black) + 25 < brightness(&open, black),
            "cookie masks its black half"
        );
        assert!(
            brightness(&image, white).abs_diff(brightness(&open, white)) <= 2,
            "cookie preserves its white half"
        );
        let mut dark = env.clone();
        dark.lights.clear();
        let ambient = scene.render(&dark, &ground, None);
        let outside = Vec3::new(0.0, 0.0, 3.0);
        assert_eq!(
            brightness(&open, outside),
            brightness(&ambient, outside),
            "outside cone is unlit"
        );
        let misses = scene.renderer.cache_stats().misses;
        let pipelines = scene.renderer.pipeline_count();
        scene.gpu.queue.write_texture(
            cookie.as_image_copy(),
            &[0, 0, 0, 255, 0, 0, 0, 255],
            wxsl::render::wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8),
                rows_per_image: Some(1),
            },
            cookie.size(),
        );
        let black_image = scene.render(&masked, &ground, None);
        assert_eq!(
            black_image, ambient,
            "all-black cookie masks the whole spot"
        );
        assert_eq!(scene.renderer.cache_stats().misses, misses);
        assert_eq!(scene.renderer.pipeline_count(), pipelines);
        // Restore the mask for the next pipeline.
        scene.gpu.queue.write_texture(
            cookie.as_image_copy(),
            &[255, 255, 255, 255, 0, 0, 0, 255],
            wxsl::render::wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8),
                rows_per_image: Some(1),
            },
            cookie.size(),
        );
    }
}

/// The light budget is sixteen: fifteen dim lamps in a ring light the
/// ground measurably more than one of them alone, which is only true if
/// every row of the light array reaches the shader.
#[test]
fn sixteen_lights_all_reach_the_shader() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let graph = plain(&scene.registry, "ring receiver");
    let ground = scene.material(&graph, &MaterialConfig::default());

    let ring = |count: usize| {
        let mut environment = lit(false);
        environment.lights = (0..count)
            .map(|index| {
                let angle = index as f32 / 16.0 * core::f32::consts::TAU;
                Light::point(
                    Vec3::new(angle.cos() * 2.0, 1.5, angle.sin() * 2.0),
                    Vec3::ONE,
                    3.0,
                )
            })
            .collect();
        environment
    };
    let one = scene.render(&ring(1), &ground, None);
    let all = scene.render(&ring(abi::MAX_LIGHTS), &ground, None);
    let centre = Vec3::ZERO;
    assert!(
        brightness(&all, centre) > brightness(&one, centre),
        "sixteen lamps outshine one: {} vs {}",
        brightness(&all, centre),
        brightness(&one, centre)
    );
}

/// The gallery's `point-shadow` rig, exactly: a big cube hanging below a
/// lamp, a second cube beside it, a cool fill that casts nothing, and the
/// ground well below both. The shadows must ring the casters' footprints
/// on the ground, not hide beneath them.
#[test]
fn the_gallery_point_rig_shadows_outward() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let graph = plain(&scene.registry, "gallery point");
    let ground = scene.material(&graph, &MaterialConfig::default());
    let caster = scene.material(&graph, &MaterialConfig::default());

    // The gallery's `point-shadow` lights, exactly — a casting lamp high
    // overhead and a fill that casts nothing — over the floor the gallery
    // hangs at -1.6. The camera is the harness's, which is what
    // `brightness` projects with, so the samples land where the frame
    // rendered them.
    let environment = |casting: bool| {
        let mut lamp = Light::point(Vec3::new(0.0, 4.2, 0.0), Vec3::new(1.0, 0.9, 0.75), 40.0)
            .casting_shadow(8.0);
        lamp.casts_shadow = casting;
        let mut environment = lit(false);
        environment.lights = vec![
            lamp,
            Light::point(Vec3::new(-4.5, 1.4, -3.5), Vec3::new(0.3, 0.4, 0.65), 7.0),
        ];
        environment
    };
    let floor_drop = Mat4::from_translation(Vec3::new(0.0, -1.6, 0.0));
    let unshadowed = scene.render_lowered(&environment(false), &ground, Some(&caster), floor_drop);
    let shadowed = scene.render_lowered(&environment(true), &ground, Some(&caster), floor_drop);
    // The plate at height 1 casts a square of half-edge ~0.9 on the lowered
    // floor: sample inside it and outside it.
    let ring = Vec3::new(0.0, -1.6, 0.6);
    let beyond = Vec3::new(0.0, -1.6, 2.0);
    assert!(
        brightness(&shadowed, ring) + 20 < brightness(&unshadowed, ring),
        "the ring around the footprint must darken: {} vs {}",
        brightness(&shadowed, ring),
        brightness(&unshadowed, ring)
    );
    assert_eq!(
        brightness(&shadowed, beyond),
        brightness(&unshadowed, beyond),
        "floor beyond the shadow square is untouched"
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
    let ground = scene.material(&plain_graph, &MaterialConfig::default());

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
    let ground = scene.material(&plain_graph, &MaterialConfig::default());
    let casting = scene.material(&plain_graph, &MaterialConfig::default());
    let not_casting = scene.material(
        &plain_graph,
        &MaterialConfig {
            cast_shadow: false,
            ..MaterialConfig::default()
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
    let receiving = scene.material(&plain_graph, &MaterialConfig::default());
    let ignoring = scene.material(
        &plain_graph,
        &MaterialConfig {
            receive_shadow: false,
            ..MaterialConfig::default()
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

    let caster = scene.material(&plain_graph, &MaterialConfig::default());
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
    // The lookup lives in the generated shading function, which the
    // forward stage calls from its fragment entry and the deferred
    // lighting pass calls after unpacking the G-buffer. If the two ever
    // disagree it is because somebody wrote a second copy.
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let plain_graph = plain(&scene.registry, "ground");
    let ground = scene.material(&plain_graph, &MaterialConfig::default());
    let caster = scene.material(&plain_graph, &MaterialConfig::default());

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
    let ground = scene.material(&plain_graph, &MaterialConfig::default());
    let solid = scene.material(&plain_graph, &MaterialConfig::default());
    let holed_graph = perforated(&scene.registry);
    let holed = scene.material(&holed_graph, &MaterialConfig::default());

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
    let ground = scene.material(&plain_graph, &MaterialConfig::default());
    let still = scene.material(&plain_graph, &MaterialConfig::default());
    let moving_graph = displacing(&scene.registry);
    let moving = scene.material(&moving_graph, &MaterialConfig::default());

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
