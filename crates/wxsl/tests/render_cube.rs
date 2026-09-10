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
use wxsl::core::abi;
use wxsl::core::graph::Graph;
use wxsl::core::macros::{MacroSet, MacroValue};
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::material::Material;
use wxsl::render::{
    Camera, DrawItem, Environment, Light, RenderPath, RenderRequest, Renderer, TargetConfig,
};

const SIZE: u32 = 128;

fn demo_graph() -> Graph {
    serde_json::from_str(include_str!("../assets/pbr_cube.wxsl.json")).expect("parses")
}

fn test_environment() -> Environment {
    Environment {
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
        ..Environment::default()
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
    let registry = wxsl::stdlib::registry();
    let material =
        Material::from_graph_with_macros(&demo_graph(), &registry, macros).expect("compiles");
    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, target.format()),
    )
    .expect("the stdlib library satisfies the ABI");
    let mesh = wxsl::render::Mesh::cube(&gpu.device, 1.6);
    let environment = test_environment();
    let draws = wxsl::render::single_draw(
        DrawItem::new(&mesh, &material).with_transform(Mat4::from_rotation_y(0.6)),
    );

    let mut images = Vec::new();
    for path in paths {
        renderer.set_path(*path);
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
    let registry = wxsl::stdlib::registry();
    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, target.format()),
    )
    .unwrap();
    let mesh = wxsl::render::Mesh::cube(&gpu.device, 1.6);
    let environment = test_environment();
    let graph = demo_graph();

    let mut images = Vec::new();
    for octaves in [1, 6] {
        let mut macros = MacroSet::new();
        macros.set("WXSL_FBM_OCTAVES", MacroValue::Int(octaves));
        let material = Material::from_graph_with_macros(&graph, &registry, &macros).unwrap();
        let draws = wxsl::render::single_draw(
            DrawItem::new(&mesh, &material).with_transform(Mat4::from_rotation_y(0.6)),
        );
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

#[test]
fn every_instance_lands_where_its_own_transform_puts_it() {
    // The instance transforms are one storage buffer indexed by
    // `@builtin(instance_index)` (ADR 0021). If a draw read the wrong row —
    // or every draw read row zero, which is what a broken `first_instance`
    // looks like — both cubes would land in the same place.
    let Some(gpu) = gpu() else { return };
    let registry = wxsl::stdlib::registry();
    let material = Material::from_graph_with_macros(&demo_graph(), &registry, &MacroSet::new())
        .expect("compiles");
    let target = OffscreenTarget::new(&gpu.device, SIZE, SIZE);
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(SIZE, SIZE, target.format()),
    )
    .expect("the stdlib library satisfies the ABI");
    let mesh = wxsl::render::Mesh::cube(&gpu.device, 0.8);
    let environment = Environment {
        camera: Camera {
            eye: Vec3::new(0.0, 0.0, 6.0),
            aspect: 1.0,
            ..Camera::default()
        },
        ..test_environment()
    };

    let draws: wxsl::render::DrawList<'_> = [
        DrawItem::new(&mesh, &material)
            .with_transform(Mat4::from_translation(Vec3::new(-1.4, 0.0, 0.0))),
        DrawItem::new(&mesh, &material)
            .with_transform(Mat4::from_translation(Vec3::new(1.4, 0.0, 0.0))),
    ]
    .into_iter()
    .collect();

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
        .expect("renders two instances");
    gpu.wait();
    let image = target.read_rgba8(&gpu.device, &gpu.queue);

    let lit = |x: u32| {
        let texel = pixel(&image, x, SIZE / 2);
        texel[0] > 12 || texel[1] > 12 || texel[2] > 12
    };
    assert!(lit(SIZE / 4), "nothing on the left");
    assert!(lit(SIZE * 3 / 4), "nothing on the right");
    assert!(
        !lit(SIZE / 2),
        "the gap between the two cubes should be background"
    );
}

#[test]
fn a_persistent_resource_hands_a_pass_the_previous_frames_contents() {
    // The temporal half of the render graph, with real pixels: a resource
    // declared `Persistent { history: 2 }` is a ring, and what a pass reads
    // one frame back is what the frame before it wrote — not what this
    // frame is in the middle of writing (ADR 0021). Every temporal
    // technique there will ever be depends on exactly this.
    use wxsl::render::pass::{Attachment, Read, ResourceDesc, ScreenShader};
    use wxsl::render::{wgpu, PassDesc, RenderGraph, ResourcePool};

    let Some(gpu) = gpu() else { return };
    const EDGE: u32 = 4;
    let format = wgpu::TextureFormat::Rgba8Unorm;
    // One channel each, so a frame's contents are unmistakable.
    let colors = [
        wgpu::Color {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        },
        wgpu::Color {
            r: 0.0,
            g: 1.0,
            b: 0.0,
            a: 1.0,
        },
        wgpu::Color {
            r: 0.0,
            g: 0.0,
            b: 1.0,
            a: 1.0,
        },
        wgpu::Color {
            r: 1.0,
            g: 1.0,
            b: 0.0,
            a: 1.0,
        },
        wgpu::Color {
            r: 0.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        },
    ];
    let expected = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 0, 255],
        [0, 255, 255, 255],
    ];

    // The same pass list every frame, differing only in what it clears to.
    let build = |clear: wgpu::Color| {
        let mut graph = RenderGraph::new(format);
        let history = graph.resource(
            ResourceDesc::color("history", format)
                .persistent(2)
                .with_usage(wgpu::TextureUsages::COPY_SRC),
        );
        let pass = PassDesc::screen("accumulate", ScreenShader::DeferredLighting)
            .with_color(Attachment::clear(history, clear))
            .with_reads([Read::previous(history, 1)]);
        graph.pass(pass);
        (graph, history)
    };

    let mut pool = ResourcePool::new();
    for (frame, clear) in colors.iter().enumerate() {
        let (graph, history) = build(*clear);
        let schedule = graph.schedule().expect("schedules");
        assert_eq!(schedule.slots().len(), 3, "history: 2 is a ring of three");
        pool.configure(
            &gpu.device,
            &schedule,
            TargetConfig::new(EDGE, EDGE, format),
        );

        // What the pass is about to be handed as "one frame ago".
        if frame > 0 {
            let slot = schedule
                .slot(history, pool.frame(), 1)
                .expect("the ring is allocated");
            let texture = pool.texture(slot).expect("the pool created it");
            assert_eq!(
                read_texel(&gpu, texture),
                expected[frame - 1],
                "frame {frame} was handed the wrong frame's contents"
            );
        }

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        graph
            // Nothing is drawn: the clear is the whole of the pass, which
            // is all this test needs the contents to come from.
            .record(
                &gpu.device,
                &mut encoder,
                &schedule,
                &mut pool,
                &[],
                |_, _| Ok(()),
            )
            .expect("records");
        gpu.queue.submit([encoder.finish()]);
        gpu.wait();
    }
}

/// The first texel of `texture`, as RGBA8.
fn read_texel(gpu: &GpuContext, texture: &wgpu::Texture) -> [u8; 4] {
    use wxsl::render::wgpu;

    let row = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("texel readback"),
        size: u64::from(row),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.wait();
    let mapped = buffer
        .slice(..)
        .get_mapped_range()
        .expect("mapped after a blocking poll");
    let texel = [mapped[0], mapped[1], mapped[2], mapped[3]];
    drop(mapped);
    buffer.unmap();
    texel
}
