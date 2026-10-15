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
    ColorSpace, Instance, MaterialEntry, MeshEntry, MeshSource, Scene, SceneCamera,
    SceneEnvironment, SceneError, SceneLight, SceneLightKind, SceneNode, TagExpr, Tags,
    TextureEntry, TextureSource,
};
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::{Camera, Environment, RenderRequest, Renderer, TargetConfig};
use wxsl::scene::SceneImage;

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

/// The consolidation of ADR 0038 is a Rust-side one: the material's knobs
/// became one `MaterialConfig`, and the document they are written in did
/// *not* move. A document written before it still loads, which is the guard
/// rail "new document fields default to today's behaviour" taken seriously
/// for a field that was renamed rather than added.
#[test]
fn the_material_configs_fields_stay_where_the_document_had_them() {
    let scene = demo_scene();
    let text = serde_json::to_string(&scene).expect("serializes");
    let value: serde_json::Value = serde_json::from_str(&text).expect("parses as json");
    let material = &value["materials"][0];
    for field in ["macros", "tags", "cast_shadow", "receive_shadow"] {
        assert!(
            material.get(field).is_some(),
            "`{field}` should still sit on the material, not inside a nested config: {material}"
        );
    }
    assert!(
        material.get("config").is_none(),
        "the config is flattened, so nothing nests it: {material}"
    );

    // And the one field that *was* renamed — `lighting` to `model` — still
    // reads under its old name.
    let old = serde_json::json!({
        "name": "legacy",
        "materials": [{
            "name": "paint",
            "graph": serde_json::to_value(demo_graph()).expect("serializes"),
            "lighting": "lambert",
            "cast_shadow": false,
        }],
    });
    let back: Scene = serde_json::from_value(old).expect("a pre-0038 document still parses");
    assert_eq!(back.materials[0].config.model.as_deref(), Some("lambert"));
    assert!(!back.materials[0].config.cast_shadow);
    // The fields it left out still default to what they always did.
    assert!(back.materials[0].config.receive_shadow);
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

    // This document binds no image, so the texture the demo material
    // samples is the application's to supply — and until it does, the
    // scene says so by name rather than drawing black (ADR 0023).
    resources
        .create_bindings(&gpu.device, &gpu.queue, &mut renderer)
        .expect("nothing to bind");
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

fn translation(x: f32, y: f32, z: f32) -> [f32; 16] {
    glam::Mat4::from_translation(glam::Vec3::new(x, y, z)).to_cols_array()
}

#[test]
fn an_environment_block_places_its_camera_and_lights_by_node() {
    let mut scene = Scene::new("lit");
    let rig = scene.add_node(SceneNode::new("rig").with_transform(translation(0.0, 1.0, 0.0)));
    let eye = scene.add_node(
        SceneNode::new("eye")
            .with_parent(rig)
            .with_transform(translation(0.0, 0.0, 5.0)),
    );
    // Turned to face down: its −Z is world −Y.
    let down = scene.add_node(SceneNode::new("down").with_transform(
        glam::Mat4::from_rotation_x(-core::f32::consts::FRAC_PI_2).to_cols_array(),
    ));
    scene.environment = Some(SceneEnvironment {
        cameras: vec![SceneCamera::new("main").with_node(eye)],
        lights: vec![
            SceneLight::new("sun", SceneLightKind::Directional).with_node(down),
            SceneLight::new("bulb", SceneLightKind::Point).with_node(eye),
        ],
    });

    let environment = wxsl::scene::environment(&scene, 2.0).expect("the block is there");
    assert_eq!(environment.camera.eye, glam::Vec3::new(0.0, 1.0, 5.0));
    assert!((environment.camera.target - glam::Vec3::new(0.0, 1.0, 4.0)).length() < 1e-5);
    assert_eq!(environment.camera.aspect, 2.0);
    // A directional light stores the direction *towards* it: up.
    assert!((environment.lights[0].position_or_direction - glam::Vec3::Y).length() < 1e-5);
    assert_eq!(
        environment.lights[1].position_or_direction,
        glam::Vec3::new(0.0, 1.0, 5.0)
    );

    assert!(wxsl::scene::environment(&Scene::new("bare"), 1.0).is_none());
}

/// The demo material's textures, bound by the document itself: `albedo`
/// from a PNG file next to the scene, the `linear` sampler from an
/// embedded image. The cube sits under a node, not at its own transform.
fn textured_scene() -> Scene {
    let mut scene = Scene::new("textured");
    let cube = scene.add_mesh(MeshEntry::new("cube", MeshSource::Cube { size: 1.6 }));
    let red = scene.add_texture(TextureEntry::new(
        "red",
        TextureSource::File {
            path: "red.png".to_string(),
        },
    ));
    let sampler = scene.add_texture(
        TextureEntry::new("sampler", TextureSource::Embedded { index: 0 })
            .with_color_space(ColorSpace::Linear),
    );
    let paint = scene.add_material(
        MaterialEntry::new("paint", demo_graph())
            .with_texture("albedo", red)
            .with_texture("linear", sampler),
    );
    let left = scene.add_node(SceneNode::new("left").with_transform(translation(-1.0, 0.0, 0.0)));
    scene.add_instance(
        Instance::new(cube, paint)
            .with_node(left)
            .with_transform(translation(1.0, 0.0, 0.0)),
    );
    scene
}

#[test]
fn a_v2_document_binds_its_own_textures() {
    let Some(gpu) = gpu() else { return };
    let directory = std::env::temp_dir().join(format!("wxsl-scene-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("temp dir");
    image::save_buffer(
        directory.join("red.png"),
        &[255, 0, 0, 255].repeat(16),
        4,
        4,
        image::ExtendedColorType::Rgba8,
    )
    .expect("writes the png");

    let registry = wxsl::stdlib::registry();
    let scene = textured_scene();
    let mut resources =
        wxsl::scene::SceneResources::load(&gpu.device, &scene, &registry, Some(&directory))
            .expect("the scene loads");
    resources.set_images(vec![SceneImage::Rgba8 {
        width: 1,
        height: 1,
        pixels: vec![0, 0, 0, 255],
    }]);
    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, target.format()),
    )
    .expect("the stdlib library satisfies the ABI");
    resources
        .create_bindings(&gpu.device, &gpu.queue, &mut renderer)
        .expect("the document's bindings are declared");
    resources
        .upload(&gpu.device, &gpu.queue)
        .expect("the document bound everything");
    resources
        .instance_attributes_mut(0)
        .expect("one instance")
        .set("instance_tint", Value::Vec3([1.0, 1.0, 1.0]));

    let draws = resources.draw_list();
    let environment = Environment {
        camera: Camera {
            eye: glam::Vec3::new(0.0, 0.5, 4.0),
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
    let _ = std::fs::remove_dir_all(&directory);

    // The node and the instance's own offset cancel: the cube is centred,
    // and red because the file says so.
    let image = target.read_rgba8(&gpu.device, &gpu.queue);
    let centre = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
    let [r, g, b] = [image[centre], image[centre + 1], image[centre + 2]];
    assert!(r > 24 && r > g * 2 && r > b * 2, "centre is {r},{g},{b}");
}

#[test]
fn a_binding_the_material_does_not_declare_is_refused_by_name() {
    let Some(gpu) = gpu() else { return };
    let registry = wxsl::stdlib::registry();
    let mut scene = textured_scene();
    scene.textures[0].source = TextureSource::Embedded { index: 0 };
    let paint = &mut scene.materials[0];
    paint.bindings.textures.insert("albedoo".to_string(), 0);

    let mut resources = wxsl::scene::SceneResources::load(&gpu.device, &scene, &registry, None)
        .expect("the scene loads");
    resources.set_images(vec![SceneImage::Rgba8 {
        width: 1,
        height: 1,
        pixels: vec![255; 4],
    }]);
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, wgpu::TextureFormat::Rgba8UnormSrgb),
    )
    .expect("the stdlib library satisfies the ABI");
    let error = resources
        .create_bindings(&gpu.device, &gpu.queue, &mut renderer)
        .expect_err("`albedoo` is not declared");
    let message = error.to_string();
    assert!(
        message.contains("pbr cube") && message.contains("albedoo"),
        "{message}"
    );
}

/// A grid of cubes and spheres, interleaved in document order, with one
/// cube behind the camera (ADR 0086).
fn grid_scene() -> Scene {
    let mut scene = Scene::new("grid");
    let cube = scene.add_mesh(MeshEntry::new("cube", MeshSource::Cube { size: 0.5 }));
    let sphere = scene.add_mesh(MeshEntry::new("sphere", MeshSource::Sphere { radius: 0.3 }));
    let mut graph = Graph::new("plain");
    graph.add_node(wxsl::core::abi::SURFACE_OUTPUT_ID);
    let plain = scene.add_material(MaterialEntry::new("plain", graph));
    for index in 0..24 {
        let (x, y) = ((index % 6) as f32 - 2.5, (index / 6) as f32 - 1.5);
        let mesh = if index % 2 == 0 { cube } else { sphere };
        scene.add_instance(Instance::new(mesh, plain).with_transform(translation(x, y, 0.0)));
    }
    scene.add_instance(Instance::new(cube, plain).with_transform(translation(0.0, 0.0, 50.0)));
    scene
}

#[test]
fn a_scene_draws_identical_pairs_as_one_call_and_culls_what_no_view_sees() {
    let Some(gpu) = gpu() else { return };
    let registry = wxsl::stdlib::registry();
    let scene = grid_scene();
    let mut resources = wxsl::scene::SceneResources::load(&gpu.device, &scene, &registry, None)
        .expect("the scene loads");
    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, target.format()),
    )
    .expect("the stdlib library satisfies the ABI");
    resources
        .create_bindings(&gpu.device, &gpu.queue, &mut renderer)
        .expect("nothing to bind");
    resources
        .upload(&gpu.device, &gpu.queue)
        .expect("nothing declared");
    let environment = Environment {
        camera: Camera {
            eye: glam::Vec3::new(0.0, 0.0, 8.0),
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
    let render = |renderer: &mut Renderer, draws: &wxsl::render::DrawList<'_>| {
        renderer
            .render(
                &gpu.device,
                &gpu.queue,
                &RenderRequest {
                    view: target.view(),
                    environment: &environment,
                    draws,
                },
            )
            .expect("renders");
        gpu.wait();
        let passes: Vec<String> = renderer
            .render_graph()
            .passes()
            .iter()
            .map(|pass| pass.label.clone())
            .collect();
        let counts: Vec<(u32, u32)> = passes
            .iter()
            .map(|label| {
                (
                    renderer.pass_draw_count(label).unwrap_or(0),
                    renderer.pass_call_count(label).unwrap_or(0),
                )
            })
            .filter(|(draws, _)| *draws > 0)
            .collect();
        (counts, target.read_rgba8(&gpu.device, &gpu.queue))
    };

    // The scene's own list: by mesh within the one material, each box
    // tested against the camera. The cube behind it is not drawn.
    let (counts, batched) = render(&mut renderer, &resources.draw_list());
    assert!(!counts.is_empty(), "nothing was drawn");
    for (draws, calls) in &counts {
        assert_eq!((*draws, *calls), (24, 2), "{counts:?}");
    }
    assert_eq!(resources.draw_instance(0), Some(0));
    assert_eq!(resources.draw_instance(13), Some(1), "spheres after cubes");

    // The same draws by hand, in document order and without boxes: one
    // call each, the hidden cube too, and the same picture.
    let draws: wxsl::render::DrawList<'_> = scene
        .instances
        .iter()
        .map(|instance| {
            wxsl::render::DrawItem::new(
                &resources.meshes()[instance.mesh],
                &resources.materials()[instance.material],
            )
            .with_transform(glam::Mat4::from_cols_array(&instance.transform))
        })
        .collect();
    let (counts, unbatched) = render(&mut renderer, &draws);
    for (draws, calls) in &counts {
        assert_eq!((*draws, *calls), (25, 25), "{counts:?}");
    }
    let differs = batched
        .iter()
        .zip(&unbatched)
        .filter(|(a, b)| a.abs_diff(**b) > 1)
        .count();
    assert_eq!(differs, 0, "batching changed the picture");
    assert!(
        batched.chunks(4).any(|px| px[0] > 12),
        "the grid is missing"
    );
}
