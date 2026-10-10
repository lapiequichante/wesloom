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

use glam::{Mat4, Vec3};
use wxsl::core::abi;
use wxsl::core::graph::{Graph, Node};
use wxsl::core::lighting::{LightingSet, DEFAULT_MODELS, DEFAULT_MODEL_ID};
use wxsl::core::node::{NodeRegistry, Value};
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::material::{Material, MaterialConfig};
use wxsl::render::pipeline::StockPipeline;
use wxsl::render::{Camera, DrawItem, DrawList, Environment, Light, Mesh, Renderer, TargetConfig};

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
        previous_camera: None,
    }
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

    fn material(&self, graph: &Graph, options: &MaterialConfig) -> Material {
        Material::with_lighting(graph, &self.registry, options, self.renderer.lighting())
            .expect("the graph compiles")
    }

    /// Draw one quad per slot, each with its own material, and read the
    /// image back.
    fn render(&mut self, materials: &[&Material]) -> Vec<u8> {
        self.render_in(materials, &lit())
    }

    fn render_in(&mut self, materials: &[&Material], environment: &Environment) -> Vec<u8> {
        let items: Vec<DrawItem> = SLOTS
            .iter()
            .zip(materials)
            .map(|(x, material)| {
                DrawItem::new(&self.quad, material)
                    .with_transform(Mat4::from_translation(Vec3::new(*x, 0.0, 0.0)))
            })
            .collect();
        let draws: DrawList<'_> = items.into_iter().collect();
        probe::render_list_in(
            &self.gpu,
            &mut self.renderer,
            &self.target,
            &draws,
            environment,
        )
        .expect("the frame renders")
    }
}

fn named(name: &str) -> MaterialConfig {
    MaterialConfig {
        model: Some(name.to_string()),
        ..MaterialConfig::default()
    }
}

#[test]
fn iridescence_transports_fragment_thickness_and_has_an_exact_zero_film_fallback() {
    let Some(gpu) = gpu() else { return };
    let film_model = *DEFAULT_MODELS
        .iter()
        .find(|m| m.name == "wxsl.iridescent")
        .unwrap();
    let mut scene = Scene::with_lighting(gpu, LightingSet::single(film_model));
    let mut graph = glossy(&scene.registry, "film", 0.4);
    let output = graph
        .nodes()
        .find(|(_, n)| n.def == abi::SURFACE_OUTPUT_ID)
        .map(|(id, _)| id)
        .unwrap();
    graph.set_param(output, "metallic", Value::F32(1.0));
    graph.set_param(output, "base_color", Value::Vec3([0.5, 0.5, 0.5]));
    let zero = scene.material(&graph, &named("iridescent"));
    let without = scene.render(&[&zero, &zero, &zero]);
    graph.set_param(output, "iridescence_strength", Value::F32(1.0));
    graph.set_param(output, "iridescence_thickness", Value::F32(0.0));
    let zero_thickness = scene.material(&graph, &named("iridescent"));
    assert_eq!(
        without,
        scene.render(&[&zero_thickness, &zero_thickness, &zero_thickness])
    );

    let uv = graph.add_node("input.uv");
    let split = graph.add_node("convert.split.vec2f");
    let thickness = graph.add(
        Node::new("math.remap")
            .with_param("out_min", Value::F32(240.0))
            .with_param("out_max", Value::F32(880.0)),
    );
    graph
        .set_generic(
            &scene.registry,
            thickness,
            "T",
            wxsl::core::node::ValueType::F32,
        )
        .unwrap();
    graph
        .wire(&scene.registry, (uv, "out"), (split, "v"))
        .unwrap();
    graph
        .wire(&scene.registry, (split, "x"), (thickness, "value"))
        .unwrap();
    graph
        .wire(
            &scene.registry,
            (thickness, "out"),
            (output, "iridescence_thickness"),
        )
        .unwrap();
    let film = scene.material(&graph, &named("iridescent"));
    let deferred = scene.render(&[&film, &film, &film]);
    scene.renderer.set_pipeline(StockPipeline::Forward);
    let forward = scene.render(&[&film, &film, &film]);
    for x in SLOTS {
        let point = Vec3::new(x, 0.0, 0.0);
        assert!(
            probe::patch_gap(&deferred, &forward, camera(), point, 6) <= 4.0,
            "film channel disagrees across paths at {x}"
        );
    }
    assert!(
        probe::patch_gap(&deferred, &without, camera(), Vec3::ZERO, 6) > 8.0,
        "fragment-authored film did not change the specular response"
    );
    let mut constant_graph = graph.clone();
    constant_graph.disconnect(
        &scene.registry,
        &wxsl::core::graph::SocketRef::new(output, "iridescence_thickness"),
    );
    constant_graph.set_param(output, "iridescence_thickness", Value::F32(300.0));
    let constant = scene.material(&constant_graph, &named("iridescent"));
    let constant_image = scene.render(&[&constant, &constant, &constant]);
    assert!(
        probe::patch_gap(&constant_image, &forward, camera(), Vec3::ZERO, 6) > 4.0,
        "the fragment thickness gradient was replaced by a default thickness"
    );
    graph.set_param(output, "iridescence_ior", Value::F32(2.0));
    let other_ior = scene.material(&graph, &named("iridescent"));
    let changed = scene.render(&[&other_ior, &other_ior, &other_ior]);
    assert!(
        probe::patch_gap(&changed, &forward, camera(), Vec3::ZERO, 6) > 4.0,
        "film IOR did not reach the model"
    );
    scene.renderer.set_pipeline(StockPipeline::Deferred);
    let changed_deferred = scene.render(&[&other_ior, &other_ior, &other_ior]);
    let ior_gap = probe::patch_gap(&changed, &changed_deferred, camera(), Vec3::ZERO, 6);
    println!("changed IOR forward/deferred gap: {ior_gap}");
    // The HDR channel rounds thickness to float16; the spectral response
    // at IOR 2 amplifies that sub-nanometre error near a highlight.
    assert!(
        ior_gap <= 6.0,
        "the changed IOR was lost in the deferred channel"
    );

    let oversized =
        LightingSet::new([DEFAULT_MODELS[DEFAULT_MODEL_ID as usize], film_model]).unwrap();
    let error = scene
        .renderer
        .set_lighting(oversized)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("iridescent") && error.contains("bytes per sample"),
        "{error}"
    );
    scene
        .renderer
        .set_lighting(wxsl::core::lighting::default_single_set())
        .unwrap();
    let pbr = scene.material(&glossy_film_base(&scene.registry), &named("pbr"));
    scene.renderer.set_pipeline(StockPipeline::Deferred);
    assert_eq!(without, scene.render(&[&pbr, &pbr, &pbr]));
}

fn glossy_film_base(registry: &NodeRegistry) -> Graph {
    let mut graph = glossy(registry, "base without film", 0.4);
    let output = graph
        .nodes()
        .find(|(_, n)| n.def == abi::SURFACE_OUTPUT_ID)
        .map(|(id, _)| id)
        .unwrap();
    graph.set_param(output, "metallic", Value::F32(1.0));
    graph.set_param(output, "base_color", Value::Vec3([0.5, 0.5, 0.5]));
    graph
}

#[test]
fn sheen_transports_tint_and_roughness_in_direct_and_ambient_light() {
    let Some(gpu) = gpu() else { return };
    let model = *DEFAULT_MODELS
        .iter()
        .find(|m| m.name == "wxsl.sheen")
        .unwrap();
    let mut scene = Scene::with_lighting(gpu, LightingSet::single(model));
    // The mixed layout exceeds the portable render budget, but its shader
    // still needs device validation: exercise the ambient switch in naga.
    let switched = wxsl::core::lighting::lighting_pass_source(
        &LightingSet::new([DEFAULT_MODELS[DEFAULT_MODEL_ID as usize], model]).unwrap(),
        &[],
    );
    let wgsl = wxsl::render::variants::compile(
        &wxsl::stdlib_library(),
        &[(abi::LIGHTING_PASS_MODULE, std::borrow::Cow::Owned(switched))],
        abi::LIGHTING_PASS_MODULE,
        &wxsl::core::macros::MacroSet::new(),
    )
    .unwrap();
    let scope = scene
        .gpu
        .device
        .push_error_scope(wxsl::render::wgpu::ErrorFilter::Validation);
    let _module =
        scene
            .gpu
            .device
            .create_shader_module(wxsl::render::wgpu::ShaderModuleDescriptor {
                label: Some("sheen ambient switch"),
                source: wxsl::render::wgpu::ShaderSource::Wgsl(wgsl.into()),
            });
    let validation = pollster::block_on(scope.pop());
    assert!(
        validation.is_none(),
        "invalid ambient switch: {validation:?}"
    );
    let mut ambient = lit();
    ambient.lights.clear();
    ambient.ambient_sky = Vec3::splat(0.7);
    ambient.ambient_ground = Vec3::splat(0.7);
    let mut graph = glossy(&scene.registry, "sheen", 0.4);
    let output = graph
        .nodes()
        .find(|(_, node)| node.def == abi::SURFACE_OUTPUT_ID)
        .map(|(id, _)| id)
        .unwrap();
    graph.set_param(output, "base_color", Value::Vec3([0.12; 3]));
    let no_sheen = scene.material(&graph, &named("sheen"));
    let base_ambient = scene.render_in(&[&no_sheen; 3], &ambient);
    let base_direct = scene.render(&[&no_sheen; 3]);

    let uv = graph.add_node("input.uv");
    let split = graph.add_node("convert.split.vec2f");
    let tint = graph.add_node("convert.combine.vec3f");
    let roughness = graph.add(
        Node::new("math.remap")
            .with_param("out_min", Value::F32(0.2))
            .with_param("out_max", Value::F32(0.85)),
    );
    graph
        .set_generic(
            &scene.registry,
            roughness,
            "T",
            wxsl::core::node::ValueType::F32,
        )
        .unwrap();
    for (from, to) in [
        ((uv, "out"), (split, "v")),
        ((split, "x"), (tint, "x")),
        ((split, "y"), (tint, "z")),
        ((tint, "out"), (output, "sheen_color")),
        ((split, "x"), (roughness, "value")),
        ((roughness, "out"), (output, "sheen_roughness")),
    ] {
        graph.wire(&scene.registry, from, to).unwrap();
    }
    let sheen = scene.material(&graph, &named("sheen"));
    let ambient_deferred = scene.render_in(&[&sheen; 3], &ambient);
    let direct_deferred = scene.render(&[&sheen; 3]);
    scene.renderer.set_pipeline(StockPipeline::Forward);
    let ambient_forward = scene.render_in(&[&sheen; 3], &ambient);
    let direct_forward = scene.render(&[&sheen; 3]);
    for x in SLOTS {
        let point = Vec3::new(x, 0.0, 0.0);
        assert!(
            probe::patch_gap(&ambient_forward, &ambient_deferred, camera(), point, 6) <= 4.0,
            "sheen ambient lost its stored inputs at {x}"
        );
        assert!(
            probe::patch_gap(&direct_forward, &direct_deferred, camera(), point, 6) <= 4.0,
            "sheen direct lighting lost its stored inputs at {x}"
        );
    }
    assert!(
        probe::patch_gap(&ambient_deferred, &base_ambient, camera(), Vec3::ZERO, 6) > 8.0,
        "the ambient function did not consume the sheen channel"
    );
    assert!(
        probe::patch_gap(&direct_deferred, &base_direct, camera(), Vec3::ZERO, 6) > 4.0,
        "sheen did not change the light response"
    );

    let mut flat = graph.clone();
    for field in ["sheen_color", "sheen_roughness"] {
        flat.disconnect(
            &scene.registry,
            &wxsl::core::graph::SocketRef::new(output, field),
        );
    }
    flat.set_param(output, "sheen_color", Value::Vec3([0.9, 0.1, 0.03]));
    flat.set_param(output, "sheen_roughness", Value::F32(0.2));
    let red = scene.material(&flat, &named("sheen"));
    let red_image = scene.render_in(&[&red; 3], &ambient);
    flat.set_param(output, "sheen_color", Value::Vec3([0.03, 0.1, 0.9]));
    let blue = scene.material(&flat, &named("sheen"));
    let blue_image = scene.render_in(&[&blue; 3], &ambient);
    assert!(
        probe::patch_gap(&red_image, &blue_image, camera(), Vec3::ZERO, 6) > 8.0,
        "sheen tint never reached ambient"
    );
    flat.set_param(output, "sheen_roughness", Value::F32(0.85));
    let rough = scene.material(&flat, &named("sheen"));
    let rough_image = scene.render_in(&[&rough; 3], &ambient);
    assert!(
        probe::patch_gap(&blue_image, &rough_image, camera(), Vec3::ZERO, 6) > 4.0,
        "sheen roughness never reached ambient"
    );

    // A white layer under a uniform white hemisphere takes energy from
    // the white base instead of adding its full response on top.
    flat.set_param(output, "base_color", Value::Vec3([1.0; 3]));
    flat.set_param(output, "sheen_color", Value::Vec3([1.0; 3]));
    let white = scene.material(&flat, &named("sheen"));
    let white_image = scene.render_in(&[&white; 3], &ambient);
    flat.set_param(output, "sheen_color", Value::Vec3([0.0; 3]));
    let white_base = scene.material(&flat, &named("sheen"));
    let white_base_image = scene.render_in(&[&white_base; 3], &ambient);
    for x in SLOTS {
        let point = Vec3::new(x, 0.0, 0.0);
        let layer = probe::color_at(&white_image, camera(), point);
        let base = probe::color_at(&white_base_image, camera(), point);
        assert!(
            layer.iter().zip(base).all(|(l, b)| *l <= b + 1.0),
            "white sheen created ambient energy: {layer:?} vs {base:?}"
        );
    }

    scene
        .renderer
        .set_lighting(wxsl::core::lighting::default_single_set())
        .unwrap();
    let mut base_graph = glossy(&scene.registry, "PBR base", 0.4);
    let output = base_graph
        .nodes()
        .find(|(_, node)| node.def == abi::SURFACE_OUTPUT_ID)
        .map(|(id, _)| id)
        .unwrap();
    base_graph.set_param(output, "base_color", Value::Vec3([0.12; 3]));
    let pbr = scene.material(&base_graph, &named("pbr"));
    scene.renderer.set_pipeline(StockPipeline::Deferred);
    assert_eq!(base_ambient, scene.render_in(&[&pbr; 3], &ambient));
    assert_eq!(base_direct, scene.render(&[&pbr; 3]));
}

/// A roughness-AA'd surface (plan5 D1): a hard checker pattern folded
/// into a normal, whose screen-space variance the `roughness_aa` node
/// absorbs into the roughness. The derivatives are the measurement, so
/// the node is fragment-only — which is exactly what makes this material
/// the forward-shading test case (plan5 D2): under deferred it shades in
/// the G-buffer pass, derivatives and all.
fn aa_shaded(registry: &NodeRegistry, name: &str) -> Graph {
    let mut graph = Graph::new(name);
    let uv = graph.add_node("input.uv");
    let checker =
        graph.add(Node::new("generative.checker").with_param("cells", Value::Vec2([4.0, 4.0])));
    let combine = graph.add_node("convert.combine.vec3f");
    let normalize = graph.add_node("math.safe_normalize");
    let aa = graph.add(Node::new("lighting.roughness_aa").with_param("roughness", Value::F32(0.2)));
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .set_generic(registry, normalize, "T", wxsl::core::node::ValueType::Vec3)
        .expect("vec3f is an allowed type");
    graph
        .wire(registry, (uv, "out"), (checker, "uv"))
        .expect("a vec2f");
    graph
        .wire(registry, (checker, "out"), (combine, "x"))
        .expect("a scalar");
    graph
        .wire(registry, (combine, "out"), (normalize, "v"))
        .expect("a vec3f");
    graph
        .wire(registry, (normalize, "out"), (aa, "normal"))
        .expect("a vec3f");
    graph
        .wire(registry, (aa, "out"), (output, "roughness"))
        .expect("a scalar");
    graph
}

/// The acceptance test of the escape hatch (plan5 D2): a forward-shaded
/// material under the *deferred* preset shades in the G-buffer pass — the
/// roughness-AA'd graph above, derivatives and all — and the lighting pass
/// returns the radiance it stored, so the two pipelines agree almost
/// exactly: the same fragment output travels both paths, with one float16
/// round trip between them, where the ordinary models' agreement is a
/// tolerance over two different shading sites.
#[test]
fn a_forward_shaded_material_shades_identically_under_both_pipelines() {
    let Some(gpu) = gpu() else { return };
    let mut scene = Scene::new(gpu);
    let graph = aa_shaded(&scene.registry, "forward shaded");
    let config = MaterialConfig {
        model: Some("pbr".to_string()),
        receive_shadow: false,
        forward_shaded: true,
        ..MaterialConfig::default()
    };
    let material = scene.material(&graph, &config);

    // The mechanism is visible before anything renders: the gbuffer
    // module shades and then packs, and the forward module never packs.
    let gbuffer = material.shader(abi::MaterialStage::GBUFFER).source.clone();
    assert!(
        gbuffer.contains("pack_gbuffer(surface, shade_surface(surface, ctx)"),
        "the gbuffer fragment does not shade: {gbuffer}"
    );
    assert!(!material
        .shader(abi::MaterialStage::FORWARD_LIT)
        .source
        .contains("pack_gbuffer"));

    let deferred = scene.render(&[&material, &material, &material]);
    scene.renderer.set_pipeline(StockPipeline::Forward);
    let forward = scene.render(&[&material, &material, &material]);

    for x in SLOTS {
        let d = probe::color_at(&deferred, camera(), Vec3::new(x, 0.0, 0.0));
        let f = probe::color_at(&forward, camera(), Vec3::new(x, 0.0, 0.0));
        println!("x = {x}: deferred {d:?} forward {f:?}");
        assert!(
            probe::gap(d, f) <= 4.0,
            "the forward-shaded material disagrees across the pipelines at x = {x}: \
             {f:?} vs {d:?}"
        );
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
        let d = probe::color_at(&mixed, camera(), centers[index]);
        let f = probe::color_at(&mixed_forward, camera(), centers[index]);
        println!("{name}: deferred {d:?} forward {f:?}");
        assert!(
            probe::gap(d, f) <= 4.0,
            "{name}: forward and deferred disagree at its slot: {f:?} vs {d:?}"
        );
    }
    // The three models answer differently for the same surface under the
    // same lights: Lambertian only, diffuse plus a Phong lobe, and
    // Cook-Torrance. Around the highlight their lobes are not the same
    // curve, which is where the comparison looks — the same pixel of two
    // one-model frames, worst channel.
    for (a, b) in [(0, 1), (1, 2), (0, 2)] {
        let difference = probe::patch_gap(&deferred[a], &deferred[b], camera(), centers[a], 6);
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
    let material = single.material(&graph, &MaterialConfig::default());
    let full_material = full.material(&graph, &MaterialConfig::default());

    let one_model = single.render(&[&material, &material, &material]);
    let every_model = full.render(&[&full_material, &full_material, &full_material]);
    for x in SLOTS {
        let a = probe::color_at(&one_model, camera(), Vec3::new(x, 0.0, 0.0));
        let b = probe::color_at(&every_model, camera(), Vec3::new(x, 0.0, 0.0));
        assert!(
            probe::gap(a, b) <= 2.0,
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
    let difference = probe::patch_gap(&without, &with, camera(), Vec3::new(0.0, 0.0, 0.0), 6);
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
    let stranger = single.material(&graph, &MaterialConfig::default());

    let items = vec![DrawItem::new(&scene.quad, &stranger)];
    let draws: DrawList<'_> = items.into_iter().collect();
    let error = probe::render_list_in(
        &scene.gpu,
        &mut scene.renderer,
        &scene.target,
        &draws,
        &lit(),
    )
    .expect_err("the sets disagree");
    let message = error.to_string();
    assert!(message.contains("models="), "{message}");
    assert!(message.contains(&stranger.name), "{message}");
}
