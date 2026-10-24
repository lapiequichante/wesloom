//! Clustered local lights on a real device (ADR 0076).
//!
//! A ranged, unshadowed point or spot light leaves the scene uniform's
//! sixteen slots for the clustered list, and each fragment loops only over
//! the lights its cluster lists. Three things make that true rather than
//! plausible: the same lights draw the same picture through either path
//! (`Environment::cluster_lights` is the A/B switch), a frame carries far
//! more than sixteen of them, and a light contributes nothing past its
//! range — which is what lets the host bin it at all. The first is run on
//! the WebGL2-equivalent device too, where the lists are textures.
//!
//! Skips with no adapter, like every GPU test.

use glam::{Mat4, Vec3, Vec4Swizzles};
use wxsl::core::abi;
use wxsl::core::graph::Graph;
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::material::{Material, MaterialConfig};
use wxsl::render::{
    Camera, DrawItem, DrawList, Environment, Light, Mesh, RenderRequest, Renderer, StockPipeline,
    TargetConfig,
};

const SIZE: u32 = 128;

fn camera() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 6.0, 7.0),
        target: Vec3::ZERO,
        aspect: 1.0,
        ..Camera::default()
    }
}

fn environment(lights: Vec<Light>, cluster_lights: bool) -> Environment {
    Environment {
        camera: camera(),
        lights,
        cluster_lights,
        ambient_sky: Vec3::splat(0.02),
        ambient_ground: Vec3::splat(0.01),
        ..Environment::default()
    }
}

/// `count` coloured lamps on a square lattice over the ground, each
/// reaching `range` — small enough that most of the ground is outside
/// every one of them.
fn lattice(count: usize, range: f32) -> Vec<Light> {
    let side = (count as f32).sqrt().ceil() as usize;
    (0..count)
        .map(|index| {
            let (column, row) = (index % side, index / side);
            let at = |cell: usize| (cell as f32 + 0.5) / side as f32 * 8.0 - 4.0;
            let hue = index as f32 * 0.618;
            Light::point(
                Vec3::new(at(column), 0.05, at(row)),
                Vec3::new(
                    0.6 + 0.4 * hue.sin(),
                    0.6 + 0.4 * (hue + 2.1).sin(),
                    0.6 + 0.4 * (hue + 4.2).sin(),
                ),
                1.5,
            )
            .with_range(range)
        })
        .collect()
}

struct Scene {
    gpu: GpuContext,
    target: OffscreenTarget,
    renderer: Renderer,
    ground: Mesh,
    material: Material,
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
        let mut graph = Graph::new("lattice receiver");
        graph.add_node(abi::SURFACE_OUTPUT_ID);
        let material = Material::with_config(
            &graph,
            &wxsl::stdlib::registry(),
            &MaterialConfig::default(),
        )
        .expect("the graph compiles");
        Scene {
            gpu,
            target,
            renderer,
            ground,
            material,
        }
    }

    fn render(&mut self, environment: &Environment) -> Vec<u8> {
        let draws: DrawList<'_> =
            [DrawItem::new(&self.ground, &self.material).with_transform(Mat4::IDENTITY)]
                .into_iter()
                .collect();
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

/// Where a ground point lands in the image, and the pixel's channels.
fn pixel(image: &[u8], world: Vec3) -> [u8; 3] {
    let clip = camera().view_proj() * world.extend(1.0);
    let ndc = clip.xyz() / clip.w;
    let x = (((ndc.x * 0.5 + 0.5) * SIZE as f32) as u32).min(SIZE - 1);
    let y = (((0.5 - ndc.y * 0.5) * SIZE as f32) as u32).min(SIZE - 1);
    let index = ((y * SIZE + x) * 4) as usize;
    [image[index], image[index + 1], image[index + 2]]
}

fn largest_difference(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0)
}

/// Sixteen ranged lamps fit either path, so the grid and the uniform must
/// draw the same picture — forward and deferred, full and WebGL2 profile.
/// One step of rounding is the summation order, nothing more.
#[test]
fn the_grid_and_the_uniform_draw_the_same_lights() {
    let devices = [
        pollster::block_on(GpuContext::headless()).ok(),
        pollster::block_on(GpuContext::headless_webgl2()).ok(),
    ];
    if devices[0].is_none() {
        eprintln!("skipping GPU test: no adapter");
        return;
    }
    let lights = lattice(abi::MAX_LIGHTS, 1.6);
    for gpu in devices.into_iter().flatten() {
        let mut scene = Scene::new(gpu);
        for pipeline in [StockPipeline::Forward, StockPipeline::Deferred] {
            scene.renderer.set_pipeline(pipeline);
            let uniform = scene.render(&environment(lights.clone(), false));
            let clustered = scene.render(&environment(lights.clone(), true));
            let lit = pixel(
                &clustered,
                lights[5].position_or_direction * Vec3::new(1.0, 0.0, 1.0),
            );
            assert!(
                lit.iter().any(|&channel| channel > 40),
                "{pipeline:?}: lamp 5 lights the ground: {lit:?}"
            );
            let difference = largest_difference(&uniform, &clustered);
            assert!(
                difference <= 1,
                "{pipeline:?}: clustered and uniform lights differ by {difference}"
            );
        }
    }
}

/// A thousand lamps: any of them, far past the uniform's sixteen slots,
/// lights the ground under it, and ground between lamps — outside every
/// range — is exactly the ambient-only picture.
#[test]
fn a_thousand_local_lights_light_only_their_reach() {
    let Some(gpu) = pollster::block_on(GpuContext::headless()).ok() else {
        eprintln!("skipping GPU test: no adapter");
        return;
    };
    let mut scene = Scene::new(gpu);
    let lights = lattice(1024, 0.1);
    let grid = environment(lights.clone(), true).light_grid();
    assert_eq!(grid.lights.len(), 1024);
    assert_eq!(grid.dropped, 0);
    let dark = scene.render(&environment(Vec::new(), true));
    let lit = scene.render(&environment(lights.clone(), true));

    let under = |light: &Light| light.position_or_direction * Vec3::new(1.0, 0.0, 1.0);
    for index in [0, 17, 528, 911] {
        let (before, after) = (
            pixel(&dark, under(&lights[index])),
            pixel(&lit, under(&lights[index])),
        );
        assert!(
            after
                .iter()
                .zip(before)
                .any(|(after, before)| *after > before + 20),
            "lamp {index} lights the ground under it: {before:?} -> {after:?}"
        );
    }
    // Halfway between four lamps, about 0.18 from each: past every range (0.09 on the ground).
    let spacing = 8.0 / 32.0;
    let between = Vec3::new(-4.0 + spacing * 4.0, 0.0, -4.0 + spacing * 4.0);
    assert_eq!(pixel(&lit, between), pixel(&dark, between));
}

// Moving a drive lamp between the uniform and clustered lists must not
// change its contribution, even with overlapping cones and varying cells.
#[test]
fn profiled_spots_match_between_grid_and_uniform() {
    let Some(gpu) = pollster::block_on(GpuContext::headless()).ok() else {
        return;
    };
    let mut scene = Scene::new(gpu);
    use wxsl::render::wgpu;
    let profiles = scene.gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("spot profiles"),
        size: wgpu::Extent3d {
            width: 2,
            height: 1,
            depth_or_array_layers: 2,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    scene.gpu.queue.write_texture(
        profiles.as_image_copy(),
        &[255; 16],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(1),
        },
        profiles.size(),
    );
    let view = profiles.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    scene
        .renderer
        .set_light_cookies(&scene.gpu.device, Some(&view));
    for pipeline in [StockPipeline::Forward, StockPipeline::Deferred] {
        scene.renderer.set_pipeline(pipeline);
        for profiled in [false, true] {
            for mixed in [false, true] {
                let mut lights = [-2.0, 2.0]
                    .map(|x| {
                        let light = Light::spot(
                            Vec3::new(x, 3.0, 0.0),
                            Vec3::NEG_Y,
                            0.8,
                            1.3,
                            Vec3::ONE,
                            8.0,
                        )
                        .with_range(10.0);
                        if profiled {
                            light.with_profile(1)
                        } else {
                            light
                        }
                    })
                    .to_vec();
                if mixed {
                    // One uniform light beside locals, with a varying cell
                    // count where the small point light reaches the receiver.
                    lights.push(Light::directional(Vec3::Y, Vec3::ONE, 0.1));
                    lights.push(
                        Light::point(Vec3::new(0.0, 0.3, 0.0), Vec3::ONE, 0.05).with_range(0.7),
                    );
                }
                let uniform = scene.render(&environment(lights.clone(), false));
                let clustered = scene.render(&environment(lights, true));
                let gap = largest_difference(&uniform, &clustered);
                assert!(gap <= 1, "{pipeline:?}, profile {profiled}, mixed {mixed}: clustered spots differ by {gap}");
            }
        }
    }
}
