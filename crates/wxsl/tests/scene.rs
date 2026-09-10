//! The scene document, end to end: a `.json` file in, a draw list out.
//!
//! Two halves, and only the second needs a GPU. The document is pure data
//! (`wxsl_core::scene`), so parsing it, validating it and reading its tags
//! are checked with no device at all; resolving it into meshes and compiled
//! materials is `wxsl::scene`'s job and skips when no adapter is available,
//! like `render_cube.rs`.

use wxsl::core::graph::Graph;
use wxsl::core::node::Value;
use wxsl::core::scene::{
    Instance, MaterialEntry, MeshEntry, MeshSource, Scene, SceneError, TagExpr, Tags,
};
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::{Camera, Environment, RenderRequest, Renderer, TargetConfig};

const SIZE: u32 = 96;

fn demo_graph() -> Graph {
    serde_json::from_str(include_str!("../assets/pbr_cube.wxsl.json")).expect("parses")
}

/// A cube and a sphere, one opaque and one also outlined.
fn demo_scene() -> Scene {
    let mut scene = Scene::new("two shapes");
    let cube = scene.add_mesh(MeshEntry::new("cube", MeshSource::Cube { size: 1.2 }));
    let sphere = scene.add_mesh(MeshEntry::new("sphere", MeshSource::Sphere { radius: 0.7 }));
    let paint = scene.add_material(MaterialEntry::new("paint", demo_graph()));

    scene.add_instance(Instance::new(cube, paint).with_name("left").with_transform(
        glam::Mat4::from_translation(glam::Vec3::new(-1.2, 0.0, 0.0)).to_cols_array(),
    ));
    scene.add_instance(
        Instance::new(sphere, paint)
            .with_name("right")
            .with_transform(
                glam::Mat4::from_translation(glam::Vec3::new(1.2, 0.0, 0.0)).to_cols_array(),
            )
            .with_tags(Tags::from_iter(["opaque", "outlined"])),
    );
    scene
}

fn gpu() -> Option<GpuContext> {
    match pollster::block_on(GpuContext::headless()) {
        Ok(context) => Some(context),
        Err(error) => {
            eprintln!("skipping GPU test: {error}");
            None
        }
    }
}

#[test]
fn a_scene_round_trips_through_the_document_format() {
    let scene = demo_scene();
    let text = serde_json::to_string_pretty(&scene).expect("serializes");
    let back: Scene = serde_json::from_str(&text).expect("parses");

    assert_eq!(back.name, "two shapes");
    assert_eq!(back.meshes.len(), 2);
    assert_eq!(back.meshes[1].source, MeshSource::Sphere { radius: 0.7 });
    assert_eq!(back.instances.len(), 2);
    assert_eq!(back.instances[1].name, "right");
    assert_eq!(back.instances[0].transform, scene.instances[0].transform);
    // The material's graph survives too, which is what makes a scene one
    // document rather than a scene plus a pile of loose graphs.
    assert_eq!(
        back.materials[0].graph.node_count(),
        demo_graph().node_count()
    );
    assert!(back.validate().is_empty());
}

#[test]
fn a_pass_draws_the_tag_expression_it_asks_for() {
    // The material says what it is; the pass says what it draws. Neither
    // introspects the other, which is the whole point of the tags.
    let scene = demo_scene();
    let opaque = TagExpr::parse("opaque").expect("parses");
    let outlined = TagExpr::parse("opaque && outlined").expect("parses");

    let matching = |selector: &TagExpr| {
        scene
            .instances
            .iter()
            .filter(|instance| selector.matches(scene.instance_tags(instance)))
            .count()
    };
    assert_eq!(matching(&opaque), 2, "both shapes are opaque");
    assert_eq!(matching(&outlined), 1, "only the sphere is outlined");
    assert_eq!(matching(&TagExpr::Always), 2);
}

#[test]
fn a_dangling_index_is_reported_before_anything_is_uploaded() {
    let mut scene = demo_scene();
    scene.add_instance(Instance::new(9, 0));
    let errors = scene.validate();
    assert_eq!(
        errors,
        vec![SceneError::NoSuchMesh {
            instance: 2,
            mesh: 9
        }]
    );
}

/// A 1x1 white texture and a default sampler: the identity for the demo
/// material, which multiplies its albedo by whatever it samples.
fn white_texture(gpu: &GpuContext) -> (wgpu::TextureView, wgpu::Sampler) {
    let extent = wgpu::Extent3d {
        width: 1,
        height: 1,
        depth_or_array_layers: 1,
    };
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("white"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &[255u8, 255, 255, 255],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        extent,
    );
    (
        texture.create_view(&wgpu::TextureViewDescriptor::default()),
        gpu.device
            .create_sampler(&wgpu::SamplerDescriptor::default()),
    )
}

#[test]
fn a_scene_document_renders() {
    let Some(gpu) = gpu() else { return };
    let registry = wxsl::stdlib::registry();
    let scene = demo_scene();
    let mut resources = wxsl::scene::SceneResources::load(&gpu.device, &scene, &registry, None)
        .expect("the scene loads");
    assert_eq!(resources.meshes().len(), 2);
    assert_eq!(resources.materials().len(), 1);

    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, target.format()),
    )
    .expect("the stdlib library satisfies the ABI");

    // The document names meshes and materials but not images, so the
    // texture the demo material samples is the application's to supply —
    // and until it does, the scene says so by name rather than drawing
    // black (ADR 0023).
    resources.create_bindings(&gpu.device, &mut renderer);
    let unbound = resources
        .upload(&gpu.device, &gpu.queue)
        .expect_err("the demo material declares a texture");
    assert!(unbound.to_string().contains("albedo"), "{unbound}");
    let (view, sampler) = white_texture(&gpu);
    let bindings = resources.bindings_mut(0).expect("created above");
    bindings.set_texture("albedo", &view).expect("declared");
    bindings.set_sampler("linear", &sampler).expect("declared");
    resources
        .upload(&gpu.device, &gpu.queue)
        .expect("everything is bound now");

    // And the same again for what the *geometry* owes: the document
    // names a material that declares a per-instance tint, and a
    // per-instance value is precisely the thing a document with one
    // material and many instances cannot hold (ADR 0024).
    for index in 0..resources.instance_count() {
        resources
            .instance_attributes_mut(index)
            .expect("in range")
            .set("instance_tint", Value::Vec3([1.0, 1.0, 1.0]));
    }

    let draws = resources.draw_list();
    assert_eq!(draws.len(), 2);
    // Both shapes are opaque, and only one carries the extra tag.
    let outlined = TagExpr::parse("outlined").expect("parses");
    assert_eq!(draws.select(&outlined).count(), 1);
    let environment = Environment {
        camera: Camera {
            eye: glam::Vec3::new(0.0, 0.5, 6.0),
            aspect: 1.0,
            ..Camera::default()
        },
        lights: vec![wxsl::render::Light::directional(
            glam::Vec3::new(0.3, 0.8, 1.0),
            glam::Vec3::ONE,
            3.0,
        )],
        ..Environment::default()
    };

    renderer
        .render(
            &gpu.device,
            &gpu.queue,
            &RenderRequest {
                view: target.view(),
                environment: &environment,
                draws: &draws,
            },
        )
        .expect("renders the scene");
    gpu.wait();

    let image = target.read_rgba8(&gpu.device, &gpu.queue);
    let lit = |x: u32| {
        let index = ((SIZE / 2 * SIZE + x) * 4) as usize;
        image[index] > 12 || image[index + 1] > 12 || image[index + 2] > 12
    };
    // One shape each side of the middle, from two instances of two
    // different meshes sharing one material.
    assert!(lit(SIZE / 4), "the cube is missing");
    assert!(lit(SIZE * 3 / 4), "the sphere is missing");
}
