//! Bake passes, on the GPU (plan3 N5, ADR 0045).
//!
//! The device-free halves — the declaration's checks, the sample-and-skip
//! emission, the generated dispatch, the imported target in the document
//! vocabulary — are tested in `wxsl-core` and `wxsl-render`. What needs a
//! device is the loop the ADR exists for:
//!
//! * the done-when: an expensive noise-driven PBR term bakes to a texture,
//!   and toggling the bake **changes cost but not image** — the two arms
//!   of the toggle, rendered and compared;
//! * invalidation is the Policy machinery: a static bake runs `once` and
//!   stays run, an `on demand` one sleeps until `mark_pass` and stops
//!   after;
//! * and the cross-link nothing else exercises: a material sampling a
//!   table the *pass list* writes — the texture is the material's, the
//!   write rides it as an imported resource.
//!
//! The document places the bake pass before the material pass on purpose:
//! the material's sample is a dependency the scheduler cannot see, and
//! declaration order is its tie-break.

use glam::{Mat4, Vec3};
use wxsl::core::graph::Graph;
use wxsl::render::effect::Effect;
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::material::MaterialConfig;
use wxsl::render::wgpu;
use wxsl::render::{
    Camera, DrawItem, EffectRegistry, Environment, Light, Material, MaterialBindings, Mesh,
    PipelineConfig, Renderer, TargetConfig,
};

mod probe;

use probe::{gpu, render_list_in, SIZE};

/// The texture name the demo material's declaration goes by — the same
/// string in the material's `bakes` declaration, the pipeline's
/// `resource.color` label, and the renderer's import.
const TABLE: &str = "roughness_bake";

/// The bake material, parsed from its document — one authoring, and the
/// toggle is a field on the configuration, not an edit here.
fn bake_material() -> Graph {
    serde_json::from_str(include_str!("../assets/bake_term.wxsl.json"))
        .expect("the bake material document parses")
}

/// The bake effect, generated from the material's subgraph — the same call
/// the gallery demo and an application make.
fn bake_effect() -> Effect {
    Effect::from_bake(
        "demo.bake_roughness",
        "roughness bake",
        "Evaluate the material's baked term over its table.",
        &bake_material(),
        TABLE,
        &wxsl::core::macros::MacroSet::new(),
        &wxsl::stdlib::registry(),
    )
    .expect("the baked cone generates")
}

/// A minimal forward document with the bake in it — the probe's linear
/// chain (no display transform between the shader and the pixel), plus the
/// imported table and the bake pass, the bake *before* the material pass
/// in declaration order.
fn bake_document(policy: &str) -> Graph {
    use wxsl::core::pipeline as doc;
    let effects = EffectRegistry::default().with(bake_effect());
    let registry = wxsl::render::document_registry(&effects);
    let mut graph = wxsl_core::pipeline::document("bake probe");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let table = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label(TABLE)
            .with_setting(doc::SETTING_PRECISION, "hdr")
            .with_setting(doc::SETTING_IMPORTED, "true"),
    );
    let bake = graph.add(
        wxsl::core::graph::Node::new(format!("{}demo.bake_roughness", doc::PASS_COMPUTE_PREFIX))
            .with_label("roughness bake")
            .with_setting(doc::SETTING_POLICY, policy),
    );
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let shade = graph.add(wxsl::core::graph::Node::new(doc::PASS_GEOMETRY).with_label("forward"));
    let present = graph.add_node(doc::PRESENT);
    for (from, to) in [
        ((scene, "draws"), (shade, "draws")),
        ((depth, "depth"), (shade, "depth")),
        ((shade, "color"), (present, "surface")),
        ((table, "color"), (bake, "bake")),
    ] {
        graph.wire(&registry, from, to).expect("probe wiring");
    }
    graph
}

/// A lit metal plane: ambient specular, which is exactly the term the
/// roughness noise bends, and two lamps for the highlight.
fn lit() -> Environment {
    Environment {
        camera: Camera {
            eye: Vec3::new(0.0, 0.0, 3.0),
            aspect: 1.0,
            ..Camera::default()
        },
        lights: vec![
            Light::point(Vec3::new(1.4, 1.6, 1.2), Vec3::new(1.0, 0.9, 0.8), 9.0),
            Light::point(Vec3::new(-1.6, 0.4, 1.4), Vec3::new(0.45, 0.6, 1.0), 6.0),
        ],
        ambient_sky: Vec3::new(0.15, 0.35, 0.9),
        ambient_ground: Vec3::new(0.05, 0.04, 0.035),
        exposure: 1.0,
        time: 0.0,
        previous_time: 0.0,
    previous_camera: None,
    }
}

/// The bake material, compiled with `bakes` set, plus the table its
/// declaration names — created by the *host* (this test, like any scene
/// would), bound into the material, and handed to the renderer as the
/// imported resource the pass list writes through.
fn material_and_table(
    gpu: &GpuContext,
    renderer: &mut Renderer,
    bakes: bool,
) -> (Material, MaterialBindings) {
    let registry = wxsl::stdlib::registry();
    let graph = bake_material();
    let material = Material::with_lighting(
        &graph,
        &registry,
        &MaterialConfig {
            bakes,
            ..MaterialConfig::default()
        },
        renderer.lighting(),
    )
    .expect("the bake material compiles under both arms of the toggle");
    let decl = graph.bake(TABLE).expect("the declaration is there");
    let format = wxsl::render::gbuffer_format(decl.precision);
    let [width, height] = decl.size;
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("roughness bake table"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let mut bindings = renderer.material_bindings(&gpu.device, &material);
    for resource in &material.interface().resources {
        let name = resource.name.as_str();
        if resource.bake.is_some() && resource.ty == wxsl::core::node::ValueType::Sampler {
            bindings
                .set_sampler(name, &sampler)
                .expect("bind the table's sampler");
        } else if resource.bake.is_some() {
            bindings.set_texture(name, &view).expect("bind the table");
        }
    }
    bindings.upload(&gpu.device, &gpu.queue).expect("upload");
    renderer.import_resource(TABLE, view);
    (material, bindings)
}

/// A renderer on the bake document, and the target to render into.
fn setup(gpu: &GpuContext, policy: &str) -> (OffscreenTarget, Renderer) {
    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, target.format()),
    )
    .expect("renderer");
    renderer.add_effect(bake_effect());
    let document = bake_document(policy);
    let graph = wxsl::render::compile_pipeline(
        &document,
        &wxsl::render::document_registry(renderer.effects()),
        renderer.effects(),
        &PipelineConfig::new(renderer.target()),
    )
    .expect("the bake document compiles");
    renderer.set_graph(graph).expect("the bake pass list runs");
    (target, renderer)
}

/// The frame: one plane standing up to face the camera, shaded by the bake
/// material. The mesh outlives the draw list it feeds, so the caller holds
/// it.
fn frame<'a>(
    mesh: &'a Mesh,
    material: &'a Material,
    bindings: &'a MaterialBindings,
) -> wxsl::render::DrawList<'a> {
    let item = DrawItem::new(mesh, material)
        .with_transform(Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2))
        .with_bindings(bindings);
    wxsl::render::single_draw(item)
}

/// Mean absolute difference of two RGBA8 images, in 0..1 — how far the two
/// arms of the toggle are allowed to drift, which is the sampling of a
/// continuous function at texel centres versus at the fragment.
fn mean_difference(a: &[u8], b: &[u8]) -> f32 {
    let total: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (i32::from(*x) - i32::from(*y)).unsigned_abs() as u64)
        .sum();
    total as f32 / (a.len().max(1) as f32 * 255.0)
}

#[test]
fn toggling_the_bake_changes_cost_but_not_image() {
    let Some(gpu) = gpu() else { return };

    // The same scene twice: baked, then inline. The baked arm's material
    // module is *smaller by the whole cone* — the cost half of the
    // done-when, owned by the device-free tests — and here the image half:
    // the two pictures agree to within what sampling a continuous function
    // at texel centres can cost.
    let mut baked_frame = None;
    let mut inline_frame = None;
    let mesh = Mesh::plane(&gpu.device, 2.0);
    for (bakes, slot) in [(true, &mut baked_frame), (false, &mut inline_frame)] {
        let (target, mut renderer) = setup(&gpu, "once");
        let (material, bindings) = material_and_table(&gpu, &mut renderer, bakes);
        let draws = frame(&mesh, &material, &bindings);
        // Frame one bakes and shades; frame two shades what is certainly
        // the table's final content.
        for _ in 0..2 {
            let image = render_list_in(&gpu, &mut renderer, &target, &draws, &lit())
                .expect("the bake frame renders");
            *slot = Some(image);
        }
    }
    let baked = baked_frame.expect("the baked arm rendered");
    let inline = inline_frame.expect("the inline arm rendered");
    let difference = mean_difference(&baked, &inline);
    assert!(
        difference < 0.01,
        "the two arms of the toggle disagree by {difference} on average — \
         a bake must be its subgraph's own value, to within sampling"
    );
    // And the toggle was not free of *effect*: a twelve-octave noise term
    // is visibly in the image, so this comparison tested something.
    let varies = baked
        .chunks_exact(4)
        .filter(|p| p[0] != p[1] || p[1] != p[2])
        .count();
    assert!(
        varies > (SIZE * SIZE) as usize / 10,
        "the scene is not one flat colour: {varies} varied pixels"
    );
}

#[test]
fn a_once_bake_runs_once_and_stays_run() {
    let Some(gpu) = gpu() else { return };
    let (target, mut renderer) = setup(&gpu, "once");
    let (material, bindings) = material_and_table(&gpu, &mut renderer, true);
    let mesh = Mesh::plane(&gpu.device, 2.0);
    let draws = frame(&mesh, &material, &bindings);
    for frame_number in 1..=3 {
        render_list_in(&gpu, &mut renderer, &target, &draws, &lit())
            .unwrap_or_else(|e| panic!("frame {frame_number} renders: {e}"));
        assert_eq!(
            renderer.pass_run_count("roughness bake"),
            Some(1),
            "frame {frame_number}: a `once` bake bakes once"
        );
    }
}

#[test]
fn an_on_demand_bake_sleeps_until_marked_and_stops_after() {
    let Some(gpu) = gpu() else { return };
    let (target, mut renderer) = setup(&gpu, "on demand");
    let (material, bindings) = material_and_table(&gpu, &mut renderer, true);
    let mesh = Mesh::plane(&gpu.device, 2.0);
    let draws = frame(&mesh, &material, &bindings);
    let render = |renderer: &mut Renderer| {
        render_list_in(&gpu, renderer, &target, &draws, &lit()).expect("frame renders")
    };

    // Asleep: frames pass, the bake does not.
    render(&mut renderer);
    render(&mut renderer);
    assert_eq!(renderer.pass_run_count("roughness bake"), Some(0));
    // Marked: exactly one run.
    renderer.mark_pass("roughness bake");
    render(&mut renderer);
    assert_eq!(renderer.pass_run_count("roughness bake"), Some(1));
    // And the re-bake is the same pure function, so the picture does not
    // move — which is the point of marking being the *only* trigger. (A
    // bake whose content *changes* on re-bake waits for the frame group in
    // compute — the ADR 0035 note this item does not spend.)
    let before = render(&mut renderer);
    assert_eq!(renderer.pass_run_count("roughness bake"), Some(1));
    let after = render(&mut renderer);
    let difference = mean_difference(&before, &after);
    assert!(
        difference < 0.0005,
        "an unmarked bake must leave its table alone: {difference}"
    );
}
