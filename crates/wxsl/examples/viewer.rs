//! A glTF viewer: a `.glb` or `.gltf` file imported as a scene document
//! (ADR 0080), lit by an HDR environment, orbited with the mouse.
//!
//! Three demos name the Khronos sample models in `resources/glb/` (see its
//! README for their licenses); any other path opens that file.
//!
//! ```text
//! cargo run -p wxsl --features gltf --example viewer
//! cargo run -p wxsl --features gltf --example viewer -- car-concept
//! cargo run -p wxsl --features gltf --example viewer -- glam-velvet-sofa
//! cargo run -p wxsl --features gltf --example viewer -- path/to/model.glb
//! cargo run -p wxsl --features gltf --example viewer -- glam-velvet-sofa --screenshot out.png
//! cargo run -p wxsl --features gltf --release --example viewer -- --grid 30 --bench 200
//! ```
//!
//! `--grid N` repeats the model N×N times, and `--bench FRAMES` renders
//! that many frames headless, framed and from inside the grid, and prints
//! the frame times with the draws and draw calls (plan6 G3, ADR 0086).
//!
//! In the window: drag to orbit, scroll to zoom, `1`/`2`/`3` switch demo,
//! `Space` toggles the turntable, `Esc` quits. `--hdr <file.hdr>` replaces
//! the environment.
//!
//! The lighting is image-based only, unless the file carries
//! `KHR_lights_punctual` lights: the HDR is convolved into diffuse and GGX
//! cubes once, and drawn behind the model. Transmissive materials (the
//! car's glass) go through the refraction tier over the same environment.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use glam::{Mat4, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};
use wxsl::asset::ImportedScene;
use wxsl::core::scene::MeshSource;
use wxsl::render::environment::EnvironmentImage;
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::{Camera, Environment, RenderRequest, Renderer, TargetConfig};
use wxsl::scene::SceneResources;

/// The demos: a name, and the file under `resources/glb/`.
const DEMOS: &[(&str, &str)] = &[
    ("wxsl-car", "WXSLCar/wxslCar.glb"),
    ("wxsl-garage", "WXSLGarage/wxslGarage.glb"),
    ("glam-velvet-sofa", "GlamVelvetSofa/GlamVelvetSofa.glb"),
    ("damaged-helmet", "DamagedHelmet/DamagedHelmet.glb"),
    ("car-concept", "CarConcept/CarConcept.glb"),
];

const USAGE: &str = "viewer [demo|file.glb] [--hdr file.hdr] [--screenshot out.png] [--size WxH]
              [--grid N] [--bench FRAMES]
demos: damaged-helmet (default), car-concept, glam-velvet-sofa
window: drag orbits, scroll zooms, 1/2/3 switch demo, Space toggles the turntable, Esc quits";

fn resources_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources")
}

/// A demo name or a path, as a file.
fn model_path(name: &str) -> PathBuf {
    match DEMOS.iter().find(|(demo, _)| *demo == name) {
        Some((_, file)) => resources_dir().join("glb").join(file),
        None => PathBuf::from(name),
    }
}

struct Options {
    model: String,
    hdr: PathBuf,
    screenshot: Option<PathBuf>,
    size: (u32, u32),
    grid: u32,
    bench: Option<u32>,
}

fn parse_options() -> Result<Options, Box<dyn Error>> {
    let mut options = Options {
        model: DEMOS[0].0.to_string(),
        hdr: resources_dir().join("hdri/lakeside_sunrise_1k.hdr"),
        screenshot: None,
        size: (1280, 720),
        grid: 1,
        bench: None,
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || arguments.next().ok_or(format!("{argument} needs a value"));
        match argument.as_str() {
            "--hdr" => options.hdr = PathBuf::from(value()?),
            "--screenshot" => options.screenshot = Some(PathBuf::from(value()?)),
            "--grid" => options.grid = value()?.parse::<u32>()?.max(1),
            "--bench" => options.bench = Some(value()?.parse()?),
            "--size" => {
                let text = value()?;
                let (width, height) = text.split_once('x').ok_or("--size is WxH")?;
                options.size = (width.parse()?, height.parse()?);
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}`\n{USAGE}").into())
            }
            model => options.model = model.to_string(),
        }
    }
    Ok(options)
}

/// Read a Radiance `.hdr` and upload it as the IBL source.
fn load_hdr(gpu: &GpuContext, path: &Path) -> Result<EnvironmentImage, Box<dyn Error>> {
    let image = image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()?
        .to_rgb32f();
    let pixels: Vec<[f32; 3]> = image.pixels().map(|pixel| pixel.0).collect();
    Ok(wxsl::render::environment::upload_environment_image(
        &gpu.device,
        &gpu.queue,
        image.width(),
        image.height(),
        &pixels,
    )?)
}

/// The stock forward pipeline, with the HDR convolved into the
/// environment maps and drawn behind the scene. With `refractive`, the
/// `refractive` materials leave the opaque passes for the refraction tier
/// (ADR 0067), traced over the same environment.
fn add_ibl(
    renderer: &mut Renderer,
    hdr: &EnvironmentImage,
    refractive: bool,
) -> Result<(), Box<dyn Error>> {
    use wxsl::render::effect::{ibl, refraction};
    use wxsl::render::pass::{DrawSource, PassKind, Read, ResourceDesc};
    use wxsl::render::types::TextureFormat;

    renderer.set_pipeline(wxsl::render::StockPipeline::Forward);
    renderer.remove_import("viewer HDR");
    let mut graph = renderer.render_graph().clone();
    if refractive {
        for index in 0..graph.passes().len() {
            if let Some(PassKind::Geometry { source, .. }) =
                graph.pass_mut(index).map(|pass| &mut pass.kind)
            {
                *source = DrawSource::Scene(wxsl::core::scene::TagExpr::parse("!refractive")?);
            }
        }
    }
    let source = graph.resource(ResourceDesc::imported(
        "viewer HDR",
        TextureFormat::Rgba32Float,
    ));
    let maps = ibl::append_filtered_bake(&mut graph, source, "viewer IBL", 128, 16, 8);
    graph.declare_environment_maps(maps.diffuse, maps.specular);
    graph.set_environment_scale(hdr.scale);
    let tonemap = graph
        .passes()
        .iter()
        .position(
            |pass| matches!(&pass.kind, PassKind::Screen { effect } if effect == "wxsl.tonemap"),
        )
        .ok_or("the pipeline has no tonemap")?;
    let color = graph.passes()[tonemap].reads[0].resource;
    let depth = graph
        .passes()
        .iter()
        .filter_map(|pass| pass.depth.as_ref().map(|attachment| attachment.resource))
        .next_back()
        .ok_or("the pipeline has no depth")?;
    let mut background =
        ibl::append_background(&mut graph, color, depth, maps.radiance, "viewer background");
    if refractive {
        background = refraction::append(
            &mut graph,
            background,
            depth,
            maps.radiance,
            "viewer glass",
            2,
        );
    }
    graph.pass_mut(tonemap).ok_or("tonemap pass")?.reads[0] = Read::current(background);
    renderer.set_graph(graph)?;
    renderer.import_resource("viewer HDR", hdr.view.clone());
    Ok(())
}

/// One opened file, ready to draw.
struct Model {
    imported: ImportedScene,
    resources: SceneResources,
    center: Vec3,
    radius: f32,
}

impl Model {
    fn open(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        hdr: &EnvironmentImage,
        path: &Path,
        grid: u32,
    ) -> Result<Self, Box<dyn Error>> {
        let started = Instant::now();
        let mut imported = wxsl::asset::import_gltf(path)?;
        repeat(&mut imported, grid);
        println!(
            "{}: {} nodes, {} meshes, {} materials, {} textures ({:.0?})",
            path.display(),
            imported.scene.nodes.len(),
            imported.meshes.len(),
            imported.scene.materials.len(),
            imported.scene.textures.len(),
            started.elapsed()
        );
        for warning in &imported.warnings {
            println!("  warning: {warning}");
        }
        let refractive = imported
            .scene
            .materials
            .iter()
            .any(|material| material.config.tags.contains(wxsl::asset::TAG_REFRACTIVE));
        add_ibl(renderer, hdr, refractive)?;
        renderer.set_lighting(imported.lighting()?)?;
        let mut resources = SceneResources::load_imported(
            &gpu.device,
            &imported,
            &wxsl::stdlib::registry(),
            renderer.lighting(),
            &[],
        )?;
        resources.create_bindings(&gpu.device, &gpu.queue, renderer)?;
        resources.upload(&gpu.device, &gpu.queue)?;
        println!("  loaded in {:.0?}", started.elapsed());
        let (center, radius) = bounds(&imported);
        Ok(Model {
            imported,
            resources,
            center,
            radius,
        })
    }
}

/// Repeat every instance of `imported` on a `grid`×`grid` floor, spaced by
/// the model's size.
fn repeat(imported: &mut ImportedScene, grid: u32) {
    if grid <= 1 {
        return;
    }
    let (_, radius) = bounds(imported);
    let scene = &mut imported.scene;
    let originals = std::mem::take(&mut scene.instances);
    for row in 0..grid {
        for column in 0..grid {
            let offset =
                Mat4::from_translation(Vec3::new(column as f32, 0.0, row as f32) * radius * 2.5);
            for original in &originals {
                let mut copy = original.clone();
                let world = Mat4::from_cols_array(&scene.instance_world(original));
                copy.transform = (offset * world).to_cols_array();
                copy.node = None;
                scene.instances.push(copy);
            }
        }
    }
}

/// The centre and radius of every instance's vertices, in world space.
fn bounds(imported: &ImportedScene) -> (Vec3, f32) {
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for instance in &imported.scene.instances {
        let MeshSource::Embedded { index } = imported.scene.meshes[instance.mesh].source else {
            continue;
        };
        let world = Mat4::from_cols_array(&imported.scene.instance_world(instance));
        for vertex in &imported.meshes[index].vertices {
            let point = world.transform_point3(Vec3::from_array(vertex.position));
            min = min.min(point);
            max = max.max(point);
        }
    }
    if min.x > max.x {
        return (Vec3::ZERO, 1.0);
    }
    ((min + max) * 0.5, ((max - min).length() * 0.5).max(1e-3))
}

/// Where the camera is, around the model.
#[derive(Clone, Copy)]
struct Orbit {
    yaw: f32,
    pitch: f32,
    zoom: f32,
}

impl Default for Orbit {
    fn default() -> Self {
        Orbit {
            yaw: 0.6,
            pitch: 0.15,
            zoom: 1.0,
        }
    }
}

fn environment(model: &Model, orbit: Orbit, aspect: f32) -> Environment {
    let fov_y = 45f32.to_radians();
    let distance = model.radius / (fov_y * 0.5).sin() * orbit.zoom * 1.05;
    let direction = Vec3::new(
        orbit.yaw.sin() * orbit.pitch.cos(),
        orbit.pitch.sin(),
        orbit.yaw.cos() * orbit.pitch.cos(),
    );
    let lights = wxsl::scene::environment(&model.imported.scene, aspect)
        .map(|environment| environment.lights)
        .unwrap_or_default();
    Environment {
        camera: Camera {
            eye: model.center + direction * distance,
            target: model.center,
            fov_y,
            aspect,
            near: (distance - model.radius * 1.5).max(distance * 0.01),
            far: distance + model.radius * 4.0,
            ..Camera::default()
        },
        lights,
        ..Environment::default()
    }
}

fn renderer_for(gpu: &GpuContext, target: TargetConfig) -> Result<Renderer, Box<dyn Error>> {
    let mut renderer = Renderer::new(&gpu.device, wxsl::stdlib_library(), target)?;
    for effect in [
        wxsl::render::effect::ibl::EQUIRECT_TO_CUBE,
        wxsl::render::effect::ibl::DIFFUSE,
        wxsl::render::effect::ibl::SPECULAR,
        wxsl::render::effect::ibl::RESAMPLE,
    ] {
        renderer.add_effect(effect);
    }
    Ok(renderer)
}

fn screenshot(options: &Options, file: &Path) -> Result<(), Box<dyn Error>> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    let (width, height) = options.size;
    let target = OffscreenTarget::new(&gpu.device, width, height);
    let hdr = load_hdr(&gpu, &options.hdr)?;
    let mut renderer = renderer_for(&gpu, TargetConfig::new(width, height, target.format()))?;
    let model = Model::open(
        &gpu,
        &mut renderer,
        &hdr,
        &model_path(&options.model),
        options.grid,
    )?;
    let environment = environment(&model, Orbit::default(), width as f32 / height as f32);
    let draws = model.resources.draw_list();
    // The environment maps are baked by the first frame; render twice so
    // the saved one is the steady state.
    for _ in 0..2 {
        renderer.render(
            &gpu.device,
            &gpu.queue,
            &RenderRequest {
                view: target.view(),
                environment: &environment,
                draws: &draws,
            },
        )?;
    }
    gpu.wait();
    let pixels = target.read_rgba8(&gpu.device, &gpu.queue);
    image::save_buffer(
        file,
        &pixels,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )?;
    println!("saved {}", file.display());
    Ok(())
}

struct State {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    gpu: GpuContext,
    renderer: Renderer,
    format: wgpu::TextureFormat,
    hdr: EnvironmentImage,
    model: Model,
}

struct App {
    options: Options,
    state: Option<State>,
    error: Option<Box<dyn Error>>,
    orbit: Orbit,
    turntable: bool,
    dragging: bool,
    cursor: Option<(f64, f64)>,
    last_frame: Instant,
}

impl App {
    fn create_state(&mut self, event_loop: &ActiveEventLoop) -> Result<State, Box<dyn Error>> {
        let (width, height) = self.options.size;
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title(format!("wxsl viewer — {}", self.options.model))
                    .with_inner_size(winit::dpi::LogicalSize::new(width, height)),
            )?,
        );
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(Arc::clone(&window))?;
        let gpu = pollster::block_on(GpuContext::new(instance, Some(&surface)))?;
        // Linear: the tonemap encodes sRGB itself.
        let capabilities = surface.get_capabilities(&gpu.adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
            .unwrap_or(capabilities.formats[0]);
        let size = window.inner_size();
        let hdr = load_hdr(&gpu, &self.options.hdr)?;
        let mut renderer = renderer_for(
            &gpu,
            TargetConfig::new(size.width.max(1), size.height.max(1), format),
        )?;
        let model = Model::open(
            &gpu,
            &mut renderer,
            &hdr,
            &model_path(&self.options.model),
            self.options.grid,
        )?;
        let mut state = State {
            window,
            surface,
            gpu,
            renderer,
            format,
            hdr,
            model,
        };
        configure_surface(&mut state, size.width, size.height);
        println!("adapter: {}", state.gpu.adapter.get_info().name);
        Ok(state)
    }

    fn switch(&mut self, demo: usize) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let (name, _) = DEMOS[demo];
        match Model::open(
            &state.gpu,
            &mut state.renderer,
            &state.hdr,
            &model_path(name),
            self.options.grid,
        ) {
            Ok(model) => {
                state.model = model;
                self.options.model = name.to_string();
                self.orbit = Orbit::default();
                state.window.set_title(&format!("wxsl viewer — {name}"));
            }
            Err(error) => eprintln!("cannot open {name}: {error}"),
        }
    }

    fn render(&mut self) {
        let elapsed = self.last_frame.elapsed().as_secs_f32();
        self.last_frame = Instant::now();
        if self.turntable && !self.dragging {
            self.orbit.yaw += elapsed * 0.3;
        }
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let frame = match state.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                let size = state.window.inner_size();
                configure_surface(state, size.width, size.height);
                return;
            }
            _ => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let size = state.window.inner_size();
        let environment = environment(
            &state.model,
            self.orbit,
            size.width as f32 / size.height.max(1) as f32,
        );
        if let Err(error) = state.renderer.render(
            &state.gpu.device,
            &state.gpu.queue,
            &RenderRequest {
                view: &view,
                environment: &environment,
                draws: &state.model.resources.draw_list(),
            },
        ) {
            eprintln!("cannot render: {error}");
        }
        state.gpu.queue.present(frame);
    }
}

fn configure_surface(state: &mut State, width: u32, height: u32) {
    let (width, height) = (width.max(1), height.max(1));
    state.surface.configure(
        &state.gpu.device,
        &wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: state.format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        },
    );
    state.renderer.resize(&state.gpu.device, width, height);
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match self.create_state(event_loop) {
            Ok(state) => {
                self.state = Some(state);
                println!("{USAGE}");
            }
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(state) = self.state.as_mut() {
                    configure_surface(state, size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => self.dragging = state == ElementState::Pressed,
            WindowEvent::CursorMoved { position, .. } => {
                if let (true, Some((x, y))) = (self.dragging, self.cursor) {
                    self.orbit.yaw -= (position.x - x) as f32 * 0.008;
                    self.orbit.pitch =
                        (self.orbit.pitch + (position.y - y) as f32 * 0.008).clamp(-1.45, 1.45);
                }
                self.cursor = Some((position.x, position.y));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(position) => position.y as f32 / 40.0,
                };
                self.orbit.zoom = (self.orbit.zoom * 0.9f32.powf(lines)).clamp(0.2, 5.0);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state: ElementState::Pressed,
                        repeat: false,
                        ..
                    },
                ..
            } => match code {
                KeyCode::Escape | KeyCode::KeyQ => event_loop.exit(),
                KeyCode::Space => self.turntable = !self.turntable,
                KeyCode::Digit1 => self.switch(0),
                KeyCode::Digit2 => self.switch(1),
                KeyCode::Digit3 => self.switch(2),
                _ => {}
            },
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        if let Some(state) = self.state.as_ref() {
            state.window.request_redraw();
        }
    }
}

/// Render `frames` frames headless, framed and then from inside the grid,
/// and print what they cost.
fn bench(options: &Options, frames: u32) -> Result<(), Box<dyn Error>> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    let (width, height) = options.size;
    let target = OffscreenTarget::new(&gpu.device, width, height);
    let hdr = load_hdr(&gpu, &options.hdr)?;
    let mut renderer = renderer_for(&gpu, TargetConfig::new(width, height, target.format()))?;
    let model = Model::open(
        &gpu,
        &mut renderer,
        &hdr,
        &model_path(&options.model),
        options.grid,
    )?;
    let aspect = width as f32 / height as f32;
    let views = [
        ("framed", Orbit::default()),
        (
            "inside",
            Orbit {
                zoom: 0.3,
                ..Orbit::default()
            },
        ),
    ];
    for (name, orbit) in views {
        let environment = environment(&model, orbit, aspect);
        let (mut cpu, mut wall, mut gpu_ms) = (0.0, 0.0, Vec::new());
        // Two frames first: the environment bake and the shader compiles.
        for frame in 0..frames + 2 {
            let started = Instant::now();
            let draws = model.resources.draw_list();
            renderer.render(
                &gpu.device,
                &gpu.queue,
                &RenderRequest {
                    view: target.view(),
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            let recorded = started.elapsed().as_secs_f64();
            gpu.wait();
            if frame >= 2 {
                cpu += recorded;
                wall += started.elapsed().as_secs_f64();
                gpu_ms.extend(renderer.frame_time());
            }
        }
        let (mut draws, mut calls) = (0, 0);
        for pass in renderer.render_graph().passes() {
            draws += renderer.pass_draw_count(&pass.label).unwrap_or(0);
            calls += renderer.pass_call_count(&pass.label).unwrap_or(0);
        }
        let per = |total: f64| total * 1000.0 / f64::from(frames.max(1));
        let gpu_text = if gpu_ms.is_empty() {
            "no timestamps".to_string()
        } else {
            format!("{:.2} ms", gpu_ms.iter().sum::<f32>() / gpu_ms.len() as f32)
        };
        println!(
            "{name}: {} instances, {draws} draws in {calls} calls · cpu {:.2} ms · \
             cpu+wait {:.2} ms · gpu {gpu_text}",
            model.resources.instance_count(),
            per(cpu),
            per(wall),
        );
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = parse_options()?;
    if let Some(frames) = options.bench {
        return bench(&options, frames);
    }
    if let Some(file) = options.screenshot.clone() {
        return screenshot(&options, &file);
    }
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        options,
        state: None,
        error: None,
        orbit: Orbit::default(),
        turntable: true,
        dragging: false,
        cursor: None,
        last_frame: Instant::now(),
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
