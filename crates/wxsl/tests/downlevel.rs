//! The fallback profile on a real device (ADR 0075).
//!
//! WebGL2 and GL are a fallback, not the target: compute and storage stay
//! in the shipped pipelines, and a device without them gets instance rows
//! from data textures and compute effects through their fragment arms. A
//! native adapter asked for `Limits::downlevel_webgl2_defaults()` refuses
//! every storage buffer and every compute pipeline exactly as WebGL2 would,
//! so a violation is a validation error here the day it is written, not a
//! console error in somebody's browser.
//!
//! The fallback is a second implementation, so it owes the msdf rule's
//! test: each picture here is rendered twice, once per profile, and the two
//! must agree. They do to the bit — the rows are the same bytes, the LUT the
//! same integral at the same texel centres.
//!
//! Skips with no adapter, like every GPU test.

use std::sync::{Arc, Mutex};

use glam::{Mat4, Vec3};
use wxsl::core::graph::{Graph, Node};
use wxsl::core::node::Value;
use wxsl::core::pipeline as doc;
use wxsl::render::effect::{BRDF_LUT, LUT_VIEW};
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::material::Material;
use wxsl::render::wgpu;
use wxsl::render::{
    Camera, DrawItem, DrawList, Environment, InstanceAttributes, Light, MaterialBindings, Mesh,
    Profile, RenderError, RenderRequest, Renderer, StockPipeline, TargetConfig,
};

const SIZE: u32 = 96;

/// A device that can do exactly what WebGL2 can, or `None`.
fn webgl2() -> Option<GpuContext> {
    pollster::block_on(GpuContext::headless_webgl2()).ok()
}

/// The full-profile device and the WebGL2-equivalent one, or `None` (with
/// a note) when there is no adapter.
fn devices() -> Option<(GpuContext, GpuContext)> {
    match (pollster::block_on(GpuContext::headless()), webgl2()) {
        (Ok(full), Some(webgl2)) => Some((full, webgl2)),
        _ => {
            eprintln!("skipping GPU test: no adapter");
            None
        }
    }
}

/// Collect every validation error the device raises, so a test can say
/// the frame was clean rather than only that it produced pixels.
fn watch(gpu: &GpuContext) -> Arc<Mutex<Vec<String>>> {
    let errors = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&errors);
    gpu.device.on_uncaptured_error(Arc::new(move |error| {
        sink.lock().expect("error sink").push(error.to_string());
    }));
    errors
}

fn renderer(gpu: &GpuContext, target: &OffscreenTarget) -> Renderer {
    Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, target.format()),
    )
    .expect("the stdlib library satisfies the ABI")
}

fn render(
    gpu: &GpuContext,
    renderer: &mut Renderer,
    target: &OffscreenTarget,
    draws: &DrawList<'_>,
    environment: &Environment,
) -> Vec<u8> {
    renderer
        .render(
            &gpu.device,
            &gpu.queue,
            &RenderRequest {
                view: target.view(),
                environment,
                draws,
            },
        )
        .expect("the frame renders");
    gpu.wait();
    target.read_rgba8(&gpu.device, &gpu.queue)
}

fn widest(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0)
}

/// The demo material's bind group, every texture a white texel — the
/// identity for what the graph multiplies it by.
fn white_bindings(
    gpu: &GpuContext,
    renderer: &mut Renderer,
    material: &Material,
) -> MaterialBindings {
    let mut bindings = renderer.material_bindings(&gpu.device, material);
    let extent = wgpu::Extent3d {
        width: 1,
        height: 1,
        depth_or_array_layers: 1,
    };
    let white = gpu.device.create_texture(&wgpu::TextureDescriptor {
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
        white.as_image_copy(),
        &[255u8; 4],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        extent,
    );
    let view = white.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = gpu
        .device
        .create_sampler(&wgpu::SamplerDescriptor::default());
    for resource in &material.interface().resources {
        let name = resource.name.as_str();
        if resource.ty == wxsl::core::node::ValueType::Sampler {
            bindings.set_sampler(name, &sampler)
        } else {
            bindings.set_texture(name, &view)
        }
        .expect("the interface named this resource");
    }
    bindings
        .upload(&gpu.device, &gpu.queue)
        .expect("everything the graph declares is bound");
    bindings
}

/// Three tinted cubes over a ground plane, under a shadow-casting sun and
/// a point light: every instance reads its own transform *and* its own
/// declared attribute row, so a texel row read at the wrong offset is a
/// wrong colour or a cube in the wrong place.
fn scene_frames(gpu: &GpuContext) -> (Profile, Vec<Vec<u8>>) {
    let registry = wxsl::stdlib::registry();
    let graph: Graph = serde_json::from_str(include_str!("../assets/pbr_cube.wxsl.json"))
        .expect("the demo graph parses");
    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = renderer(gpu, &target);
    let material = Material::from_graph(&graph, &registry).expect("the demo graph compiles");
    let bindings = white_bindings(gpu, &mut renderer, &material);
    let cube = Mesh::cube(&gpu.device, 0.8);
    let ground = Mesh::plane(&gpu.device, 6.0);
    let tints: Vec<InstanceAttributes> = [
        [1.0, 0.35, 0.3],
        [0.3, 1.0, 0.45],
        [0.35, 0.5, 1.0],
        [1.0, 1.0, 1.0],
    ]
    .into_iter()
    .map(|tint| InstanceAttributes::new().with("instance_tint", Value::Vec3(tint)))
    .collect();
    let mut draws = DrawList::new();
    for (index, x) in [-1.1f32, 0.0, 1.1].into_iter().enumerate() {
        draws.push(
            DrawItem::new(&cube, &material)
                .with_transform(
                    Mat4::from_translation(Vec3::new(x, 0.4, 0.0))
                        * Mat4::from_rotation_y(0.4 + index as f32 * 0.5),
                )
                .with_bindings(&bindings)
                .with_attributes(&tints[index]),
        );
    }
    draws.push(
        DrawItem::new(&ground, &material)
            .with_bindings(&bindings)
            .with_attributes(&tints[3]),
    );
    let environment = Environment {
        camera: Camera {
            eye: Vec3::new(2.2, 2.6, 3.4),
            target: Vec3::new(0.0, 0.3, 0.0),
            aspect: 1.0,
            ..Camera::default()
        },
        lights: vec![
            Light::directional(Vec3::new(-0.5, 1.0, 0.4), Vec3::new(1.0, 0.95, 0.85), 2.2)
                .casting_shadow(4.0),
            Light::point(Vec3::new(1.8, 2.0, 1.6), Vec3::new(0.6, 0.7, 1.0), 14.0),
        ],
        ambient_sky: Vec3::new(0.2, 0.25, 0.35),
        ambient_ground: Vec3::new(0.05, 0.04, 0.03),
        ..Environment::default()
    };
    let mut frames = Vec::new();
    for pipeline in [StockPipeline::Forward, StockPipeline::Deferred] {
        renderer.set_pipeline(pipeline);
        frames.push(render(gpu, &mut renderer, &target, &draws, &environment));
    }
    (renderer.profile(), frames)
}

#[test]
fn the_stock_pipelines_render_the_same_on_a_webgl2_device() {
    let Some((full, webgl2)) = devices() else {
        return;
    };
    let full_errors = watch(&full);
    let webgl2_errors = watch(&webgl2);

    let (full_profile, reference) = scene_frames(&full);
    let (fallback_profile, fallback) = scene_frames(&webgl2);
    assert!(full_profile.is_full(), "{full_profile}");
    assert_eq!(
        fallback_profile,
        Profile::WEBGL2,
        "the limits decide the profile"
    );
    assert!(
        full_errors.lock().unwrap().is_empty(),
        "{:?}",
        full_errors.lock().unwrap()
    );
    assert!(
        webgl2_errors.lock().unwrap().is_empty(),
        "a storage buffer or a compute pipeline reached the WebGL2 device: {:?}",
        webgl2_errors.lock().unwrap()
    );

    for ((name, reference), fallback) in ["forward", "deferred"]
        .iter()
        .zip(&reference)
        .zip(&fallback)
    {
        // Something was drawn: the frame is not the clear colour.
        assert!(
            reference.chunks(4).any(|texel| texel != &reference[..4]),
            "{name}: the reference frame is empty"
        );
        assert_eq!(
            widest(reference, fallback),
            0,
            "{name}: instance rows from textures and the LUT from fragments \
             must reproduce the storage/compute frame exactly"
        );
    }
}

/// The LUT document: bake once by `pass.compute.wxsl.brdf_lut` into a
/// 64-texel table, view it every frame.
fn lut_document(renderer: &Renderer) -> Graph {
    let registry = wxsl::render::document_registry(renderer.effects());
    let mut graph = doc::document("lut");
    let table = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("lut")
            .with_setting(doc::SETTING_SIZE, "64x64")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let bake = graph.add(
        Node::new(format!("{}wxsl.brdf_lut", doc::PASS_COMPUTE_PREFIX))
            .with_label("lut bake")
            .with_setting(doc::SETTING_POLICY, "once"),
    );
    let view = graph.add(
        Node::new(doc::PASS_SCREEN)
            .with_label("lut view")
            .with_setting(doc::SETTING_EFFECT, "wxsl.lut_view"),
    );
    let present = graph.add_node(doc::PRESENT);
    for (from, to) in [
        ((table, "color"), (bake, "lut")),
        ((table, "color"), (view, "image")),
        ((view, "color"), (present, "surface")),
    ] {
        graph.wire(&registry, from, to).expect("lut wiring");
    }
    graph
}

/// The LUT through `pass.compute`, for whichever arm `gpu`'s profile runs.
fn lut_frame(gpu: &GpuContext) -> Vec<u8> {
    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = renderer(gpu, &target);
    renderer.add_effect(BRDF_LUT);
    renderer.add_effect(LUT_VIEW);
    let compiled = wxsl::render::compile_pipeline(
        &lut_document(&renderer),
        &wxsl::render::document_registry(renderer.effects()),
        renderer.effects(),
        renderer.pipeline_config(),
    )
    .expect("the LUT document compiles for this device");
    renderer
        .set_graph(compiled)
        .expect("the pass list runs here");
    let draws = DrawList::new();
    let environment = Environment::default();
    // Twice: the second frame shows what the `once` pass left behind.
    render(gpu, &mut renderer, &target, &draws, &environment);
    render(gpu, &mut renderer, &target, &draws, &environment)
}

#[test]
fn the_lut_bakes_the_same_through_either_arm() {
    let Some((full, webgl2)) = devices() else {
        return;
    };
    let webgl2_errors = watch(&webgl2);
    let compute = lut_frame(&full);
    let fragment = lut_frame(&webgl2);
    assert!(
        webgl2_errors.lock().unwrap().is_empty(),
        "{:?}",
        webgl2_errors.lock().unwrap()
    );
    assert!(
        compute.chunks(4).any(|texel| texel[0] > 32),
        "the compute arm baked nothing"
    );
    assert_eq!(
        widest(&compute, &fragment),
        0,
        "the two arms of the bake disagree"
    );
}

#[test]
fn a_compute_pass_list_built_for_another_profile_is_refused_by_name() {
    // A hand-built pass list skips the document compiler's lowering; the
    // renderer still refuses to hand a WebGL2 device a compute pass.
    let Some(gpu) = webgl2() else { return };
    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = renderer(&gpu, &target);
    renderer.add_effect(BRDF_LUT);
    renderer.add_effect(LUT_VIEW);
    let mut full = renderer.pipeline_config().clone();
    full.profile = Profile::FULL;
    let compiled = wxsl::render::compile_pipeline(
        &lut_document(&renderer),
        &wxsl::render::document_registry(renderer.effects()),
        renderer.effects(),
        &full,
    )
    .expect("the document compiles for a full device");
    match renderer.set_graph(compiled) {
        Err(RenderError::NativeOnly { pass, effect, .. }) => {
            assert_eq!(pass, "lut bake");
            assert_eq!(effect, "wxsl.brdf_lut");
        }
        other => panic!("expected the compute pass refused by name, got {other:?}"),
    }
}
