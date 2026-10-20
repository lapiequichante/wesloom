//! wxsl drive: me_car on a procedural road that loops around a procedural,
//! slightly cyberpunk city, at any hour (ADR 0100).
//!
//! ```text
//! cargo run -p wxsl-drive --release
//! cargo run -p wxsl-drive --release -- --hour 21.5 --speed 140 --screenshot night.png
//! ```
//!
//! The sky is Rayleigh/Mie single scattering, baked into the environment
//! maps whenever the hour changes. By day the sun casts cascaded shadows;
//! at night the moon does, and street lamps and the car's headlights,
//! both shaped by IES profiles, light the road. The camera follows the
//! car; depth of field focuses on it and the velocity buffer's motion blur
//! smears the road.
//!
//! In the window: the sliders set the speed, the hour, the motion blur and
//! the aperture; drag elsewhere to look around the car, scroll to zoom,
//! `R` recentres,
//! `Space` pauses, `Esc` quits. The last slider shows a surface debug view
//! on the car instead of shading it (ADR 0085): albedo, normal, depth,
//! metallic-roughness, mip level, and the rest a forward pass can show;
//! `--view NAME` picks one for a screenshot. The `pipeline` slider swaps
//! the forward frame for a deferred one, to compare their cost
//! (`--deferred` for a timed screenshot); the debug views are forward
//! only. Shadows are hard by default; the `shadows` slider (or
//! `--soft-shadows`) gives the lights their source size back, for
//! contact-hardening penumbrae at a higher shading cost. The `fxaa` slider
//! (or `--no-fxaa`) turns the anti-aliasing off, to compare.

mod car;
mod city;
mod lights;
mod pipeline;
mod road;
mod sky;
mod terrain;
mod ui;
mod world;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use glam::{Mat4, Vec2, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};
use wxsl::core::lighting::{DebugStage, DebugView, LightingSet, DEFAULT_MODELS, OVERDRAW_VIEW};
use wxsl::core::Value;
use wxsl::render::{
    Camera, DrawList, Environment, GpuContext, OffscreenTarget, RenderRequest, Renderer,
    ShaderLibrary, TargetConfig,
};

use car::Car;
use sky::DayLight;
use ui::{Panel, Slider};
use world::World;

const USAGE: &str = "drive [--hour H] [--speed KMH] [--blur B] [--aperture F] [--at METRES]
      [--look RADIANS] [--pitch RADIANS] [--zoom Z] [--view NAME] [--deferred]
      [--soft-shadows] [--no-fxaa] [--screenshot out.png [--ui] [--frames N] [--profile]]
      [--size WxH] [--car file.glb]
window: the sliders set speed, hour, motion blur, aperture, the car's debug view,
        forward or deferred, hard or soft shadows and FXAA; drag elsewhere looks around
        the car, scroll zooms, R recentres, Space pauses, Esc quits";

const SPEED: &str = "speed";
const HOUR: &str = "time of day";
const BLUR: &str = "motion blur";
const APERTURE: &str = "aperture";
const VIEW: &str = "view";
const PIPELINE: &str = "pipeline";
const SHADOWS: &str = "shadows";
const FXAA: &str = "fxaa";

/// The surface debug views a forward pass can show (ADR 0085). Overdraw is
/// left out: it also needs the passes' blend and depth state rewritten.
fn debug_views() -> Vec<DebugView> {
    wxsl::core::lighting::debug_views()
        .into_iter()
        .filter(|view| view.stage != DebugStage::Deferred && view.label != OVERDRAW_VIEW)
        .collect()
}

/// The debug view the `view` slider's `value` picks, or `None` to shade.
fn debug_view(value: f32) -> Option<DebugView> {
    (value.round() as usize)
        .checked_sub(1)
        .and_then(|index| debug_views().get(index).copied())
}

struct Options {
    hour: f32,
    speed: f32,
    blur: f32,
    aperture: f32,
    at: f32,
    /// How far the view starts turned around the car, radians.
    look: f32,
    /// How far it starts tilted down onto the car, radians.
    pitch: f32,
    zoom: f32,
    /// The `view` slider's start: 0 shades, then one per [`debug_views`].
    view: f32,
    /// Start on the deferred frame rather than the forward one.
    deferred: bool,
    /// Print the screenshot's GPU time per pass.
    profile: bool,
    /// Start with the lights' soft, contact-hardening shadows rather than
    /// hard ones.
    soft_shadows: bool,
    /// Start without FXAA.
    no_fxaa: bool,
    screenshot: Option<PathBuf>,
    /// Draw the sliders into the screenshot too.
    ui: bool,
    /// How many frames the screenshot renders first, timed.
    frames: u32,
    size: (u32, u32),
    car: PathBuf,
}

fn parse_options() -> Result<Options, Box<dyn Error>> {
    let mut options = Options {
        hour: 21.0,
        speed: 110.0,
        blur: 1.0,
        aperture: 8.0,
        at: 0.0,
        look: 0.0,
        pitch: 0.0,
        zoom: 1.0,
        view: 0.0,
        deferred: false,
        profile: false,
        soft_shadows: false,
        no_fxaa: false,
        screenshot: None,
        ui: false,
        frames: 4,
        size: (1600, 900),
        car: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/glb/me_car/me_car.glb"),
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || arguments.next().ok_or(format!("{argument} needs a value"));
        match argument.as_str() {
            "--hour" => options.hour = value()?.parse()?,
            "--speed" => options.speed = value()?.parse()?,
            "--blur" => options.blur = value()?.parse()?,
            "--aperture" => options.aperture = value()?.parse()?,
            "--at" => options.at = value()?.parse()?,
            "--look" => options.look = value()?.parse()?,
            "--pitch" => options.pitch = value()?.parse()?,
            "--zoom" => options.zoom = value()?.parse::<f32>()?.clamp(0.3, 40.0),
            "--view" => {
                let name = value()?;
                let views = debug_views();
                options.view = match views.iter().position(|view| view.label == name) {
                    Some(index) => (index + 1) as f32,
                    None if name == "shaded" => 0.0,
                    None => {
                        let names: Vec<&str> = views.iter().map(|view| view.label).collect();
                        return Err(
                            format!("--view is shaded or one of: {}", names.join(", ")).into()
                        );
                    }
                };
            }
            "--deferred" => options.deferred = true,
            "--profile" => options.profile = true,
            "--soft-shadows" => options.soft_shadows = true,
            "--no-fxaa" => options.no_fxaa = true,
            "--screenshot" => options.screenshot = Some(PathBuf::from(value()?)),
            "--ui" => options.ui = true,
            "--frames" => options.frames = value()?.parse::<u32>()?.max(3),
            "--car" => options.car = PathBuf::from(value()?),
            "--size" => {
                let text = value()?;
                let (width, height) = text.split_once('x').ok_or("--size is WxH")?;
                options.size = (width.parse()?, height.parse()?);
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}`\n{USAGE}").into()),
        }
    }
    Ok(options)
}

fn sliders(options: &Options) -> Panel {
    Panel::new(vec![
        Slider::new(SPEED, 0.0, 260.0, options.speed, |value| {
            format!("{value:.0} km/h")
        }),
        Slider::new(HOUR, 0.0, 24.0, options.hour, |value| {
            let minutes = (value * 60.0).round() as u32 % (24 * 60);
            format!("{:02}:{:02}", minutes / 60, minutes % 60)
        }),
        Slider::new(BLUR, 0.0, 2.0, options.blur, |value| format!("{value:.2}")),
        Slider::new(APERTURE, 1.4, 16.0, options.aperture, |value| {
            format!("f/{value:.1}")
        }),
        Slider::new(
            VIEW,
            0.0,
            debug_views().len() as f32,
            options.view,
            |value| {
                debug_view(value)
                    .map_or("shaded", |view| view.label)
                    .to_string()
            },
        ),
        Slider::new(
            PIPELINE,
            0.0,
            1.0,
            if options.deferred { 1.0 } else { 0.0 },
            |value| if value >= 0.5 { "deferred" } else { "forward" }.to_string(),
        ),
        Slider::new(
            SHADOWS,
            0.0,
            1.0,
            if options.soft_shadows { 1.0 } else { 0.0 },
            |value| if value >= 0.5 { "soft" } else { "hard" }.to_string(),
        ),
        Slider::new(
            FXAA,
            0.0,
            1.0,
            if options.no_fxaa { 0.0 } else { 1.0 },
            |value| if value >= 0.5 { "on" } else { "off" }.to_string(),
        ),
    ])
}

/// The scene, the renderer, and where the drive is.
struct Drive {
    renderer: Renderer,
    library: ShaderLibrary,
    world: World,
    car: Car,
    /// Arc length along the road, metres.
    s: f32,
    /// How far the car has driven, for its wheels.
    odometer: f32,
    clip_time: f32,
    /// The hour the sky was last baked for.
    baked_hour: Option<f32>,
    day: DayLight,
    /// Where the car and the camera were last frame, for the velocity pass.
    previous: Option<(Mat4, Camera)>,
    /// How far the view is turned around the car: yaw, pitch.
    orbit: Vec2,
    /// How far the camera is from the car, relative to the chase distance.
    zoom: f32,
    /// The debug view the car's materials are compiled for, 0 shading.
    shown_view: i32,
    /// Whether the frame installed is the deferred one.
    deferred: bool,
    /// Whether the frame installed ends in FXAA.
    fxaa: bool,
}

impl Drive {
    fn new(
        gpu: &GpuContext,
        target: TargetConfig,
        car: &Path,
        at: f32,
    ) -> Result<Self, Box<dyn Error>> {
        let started = Instant::now();
        let (registry, library) = world::libraries()?;
        let mut renderer = Renderer::new(&gpu.device, library.clone(), target)?;
        pipeline::add_effects(&mut renderer)?;
        renderer.set_lighting(LightingSet::new(DEFAULT_MODELS.iter().copied())?)?;
        let car = Car::open(gpu, &mut renderer, car, |renderer, refractive| {
            pipeline::install(renderer, refractive, false, true)
        })?;
        let world = World::new(gpu, &mut renderer, &registry)?;
        renderer.set_light_cookies(&gpu.device, Some(&lights::cookies(gpu)?));
        println!(
            "road {:.1} km, {} lamps, car {:.2} × {:.2} × {:.2} m, ready in {:.1?}",
            world.road.length() / 1000.0,
            world.lamps.len(),
            car.size.x,
            car.size.y,
            car.size.z,
            started.elapsed()
        );
        Ok(Drive {
            renderer,
            library,
            world,
            car,
            s: at,
            odometer: 0.0,
            clip_time: 0.0,
            baked_hour: None,
            day: DayLight::at(12.0),
            previous: None,
            orbit: Vec2::ZERO,
            zoom: 1.0,
            shown_view: 0,
            deferred: false,
            fxaa: true,
        })
    }

    /// Advance by `dt` seconds under `panel`'s settings and render into
    /// `view`.
    fn frame(
        &mut self,
        gpu: &GpuContext,
        view: &wgpu::TextureView,
        aspect: f32,
        panel: &Panel,
        dt: f32,
    ) -> Result<(), Box<dyn Error>> {
        let speed = panel.value(SPEED) / 3.6;
        self.s = (self.s + speed * dt).rem_euclid(self.world.road.length());
        self.odometer += speed * dt;
        self.clip_time += dt;
        let deferred = panel.value(PIPELINE) >= 0.5;
        let fxaa = panel.value(FXAA) >= 0.5;
        if (deferred, fxaa) != (self.deferred, self.fxaa) {
            pipeline::install(&mut self.renderer, self.car.refractive, deferred, fxaa)?;
            self.deferred = deferred;
            self.fxaa = fxaa;
            // The new frame's sky and environment maps start unbaked.
            self.baked_hour = None;
        }
        let hour = panel.value(HOUR);
        if self.baked_hour != Some(hour) {
            self.day = DayLight::at(hour);
            for (name, value) in self.day.sky_parameters() {
                self.renderer.set_pass_param(sky::SKY_PASS, name, value)?;
            }
            self.renderer.invalidate_bakes();
            self.world.set_lights_on(gpu, self.day.lights_on)?;
            self.car.set_lights_on(gpu, self.day.lights_on)?;
            self.baked_hour = Some(hour);
        }
        let shown = debug_view(panel.value(VIEW)).map_or(0, |view| view.id);
        if shown != self.shown_view {
            self.car.set_debug_view(gpu, &mut self.renderer, shown)?;
            self.car.set_lights_on(gpu, self.day.lights_on)?;
            self.shown_view = shown;
        }
        self.car.animate(gpu, self.clip_time, self.odometer)?;

        let road = &self.world.road;
        let frame = road.frame(self.s);
        let transform = self.car.placement(&frame, road::LANE);
        let center = transform.transform_point3(Vec3::Y * self.car.size.y * 0.5);
        // Behind on the road itself, so the camera takes the bends with the
        // car; further back the faster it goes.
        let behind = road.frame(self.s - 6.0 - speed * 0.04);
        let target = center + frame.forward * 1.5 + frame.up * 0.2;
        let mut offset = behind.at(road::LANE, 2.0) - target;
        offset = Mat4::from_rotation_y(self.orbit.x).transform_vector3(offset);
        let side = offset.cross(Vec3::Y).normalize_or(Vec3::X);
        offset = Mat4::from_axis_angle(side, self.orbit.y).transform_vector3(offset) * self.zoom;
        let camera = Camera {
            eye: target + offset,
            target,
            up: Vec3::Y,
            fov_y: 55f32.to_radians(),
            aspect,
            // Depth resolution at the car goes as distance² / near: a near
            // fixed at 0.15 m let the paint z-fight the trim just under it
            // once the camera backs off. A tenth of the way to the car
            // keeps it a hundredth of a millimetre at any zoom, and still
            // draws the road under the camera.
            near: (offset.length() * 0.1).max(0.15),
            far: 9000.0,
            ..Camera::default()
        };
        let (previous_transform, previous_camera) = self.previous.unwrap_or((transform, camera));
        let mut lights = lights::lights(
            &self.day,
            &self.world.lamps,
            transform,
            &self.car.lamps(),
            camera.eye,
        );
        if panel.value(SHADOWS) < 0.5 {
            // Hard: no source size, so each shadow is one PCF lookup
            // rather than a blocker search and a wide filter (ADR 0098).
            // Only a tube shades with its radius, and the drive has none.
            for light in &mut lights {
                light.radius = 0.0;
            }
        }
        let environment = Environment {
            camera,
            previous_camera: Some(previous_camera),
            lights,
            ambient_sky: Vec3::splat(0.002),
            ambient_ground: Vec3::splat(0.001),
            exposure: self.day.exposure,
            time: self.clip_time,
            previous_time: self.clip_time - dt,
            ..Environment::default()
        };
        pipeline::set_lens(
            &mut self.renderer,
            &camera,
            camera.eye.distance(center),
            panel.value(APERTURE),
        )?;
        self.renderer.set_pass_param(
            pipeline::MOTION_BLUR,
            "strength",
            Value::F32(panel.value(BLUR)),
        )?;
        let mut draws: DrawList = self.world.draws().into_iter().collect();
        for item in self.car.draws(transform, previous_transform) {
            draws.push(item);
        }
        self.renderer.render(
            &gpu.device,
            &gpu.queue,
            &RenderRequest {
                view,
                environment: &environment,
                draws: &draws,
            },
        )?;
        self.previous = Some((transform, camera));
        Ok(())
    }
}

/// Per-pass GPU times summed over the timed frames, for `--profile`.
#[derive(Default)]
struct Profile {
    on: bool,
    /// Milliseconds and samples per pass label, in pass order.
    passes: Vec<(String, f32, u32)>,
}

impl Profile {
    /// Turn timestamp queries on; false when the device has none.
    fn start(&mut self, gpu: &GpuContext, renderer: &mut Renderer) -> bool {
        self.on = renderer.set_pass_timing(&gpu.device, &gpu.queue, true);
        self.on
    }

    /// Add the last frame read back. Timings trail the picture by a frame
    /// or two, which an average over many frames does not mind.
    fn sample(&mut self, renderer: &Renderer) {
        if !self.on {
            return;
        }
        for pass in renderer.render_graph().passes() {
            if self.passes.iter().any(|(label, ..)| *label == pass.label) {
                continue;
            }
            self.passes.push((pass.label.clone(), 0.0, 0));
        }
        for (label, sum, samples) in &mut self.passes {
            if let Some(time) = renderer.pass_time(label) {
                *sum += time;
                *samples += 1;
            }
        }
    }

    /// The passes by mean GPU time, with what they drew last frame.
    fn print(&self, renderer: &Renderer) {
        if !self.on {
            return;
        }
        let mut rows: Vec<(&str, f32)> = self
            .passes
            .iter()
            .filter(|(_, _, samples)| *samples > 0)
            .map(|(label, sum, samples)| (label.as_str(), sum / *samples as f32))
            .collect();
        rows.sort_by(|a, b| b.1.total_cmp(&a.1));
        let total: f32 = rows.iter().map(|(_, time)| time).sum();
        println!("GPU {total:.2} ms a frame over {} passes:", rows.len());
        println!(
            "  {:>8} {:>6} {:>6} {:>6} {:>10}  pass",
            "ms", "%", "draws", "calls", "triangles"
        );
        for (label, time) in rows.iter().filter(|(_, time)| *time >= 0.01) {
            println!(
                "  {time:>8.3} {:>5.1}% {:>6} {:>6} {:>10}  {label}",
                100.0 * time / total.max(1e-6),
                renderer.pass_draw_count(label).unwrap_or(0),
                renderer.pass_call_count(label).unwrap_or(0),
                renderer.pass_triangle_count(label).unwrap_or(0),
            );
        }
    }
}

fn screenshot(options: &Options, file: &Path) -> Result<(), Box<dyn Error>> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    let (width, height) = options.size;
    let target = OffscreenTarget::new(&gpu.device, width, height);
    let mut drive = Drive::new(
        &gpu,
        TargetConfig::new(width, height, target.format()),
        &options.car,
        options.at,
    )?;
    drive.orbit = Vec2::new(options.look, options.pitch);
    drive.zoom = options.zoom;
    let panel = sliders(options);
    // Frames at 60 Hz: the first runs the bakes, and the velocity pass
    // needs a previous frame to measure against. The rest are timed.
    let mut timed = std::time::Duration::ZERO;
    let mut profile = Profile::default();
    for frame in 0..options.frames {
        // On after the first frame, which may swap in the deferred frame.
        if frame == 1 && options.profile && !profile.start(&gpu, &mut drive.renderer) {
            println!("this device has no timestamp queries: no pass timings");
        }
        let started = Instant::now();
        drive.frame(
            &gpu,
            target.view(),
            width as f32 / height as f32,
            &panel,
            1.0 / 60.0,
        )?;
        gpu.wait();
        if frame >= 2 {
            timed += started.elapsed();
            profile.sample(&drive.renderer);
        }
    }
    println!(
        "{:.2} ms a frame at {width}×{height}",
        timed.as_secs_f64() * 1000.0 / f64::from(options.frames - 2)
    );
    profile.print(&drive.renderer);
    if options.ui {
        let mut overlay = ui::Overlay::new(&gpu.device, &drive.library)?;
        overlay.render(
            &gpu.device,
            &gpu.queue,
            target.view(),
            target.format(),
            (width, height),
            &panel,
            "headless",
            1.0,
        )?;
        gpu.wait();
    }
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
    format: wgpu::TextureFormat,
    drive: Drive,
    overlay: ui::Overlay,
}

struct App {
    options: Options,
    state: Option<State>,
    error: Option<Box<dyn Error>>,
    panel: Panel,
    paused: bool,
    /// The left button is down outside the panel: the mouse turns the view.
    looking: bool,
    cursor: Vec2,
    last_frame: Instant,
    /// Frame times, for the status line.
    fps: f32,
}

impl App {
    fn create_state(&mut self, event_loop: &ActiveEventLoop) -> Result<State, Box<dyn Error>> {
        let (width, height) = self.options.size;
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("wxsl drive")
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
        let drive = Drive::new(
            &gpu,
            TargetConfig::new(size.width.max(1), size.height.max(1), format),
            &self.options.car,
            self.options.at,
        )?;
        let overlay = ui::Overlay::new(&gpu.device, &drive.library)?;
        let mut state = State {
            window,
            surface,
            gpu,
            format,
            drive,
            overlay,
        };
        configure_surface(&mut state, size.width, size.height);
        println!("adapter: {}", state.gpu.adapter.get_info().name);
        Ok(state)
    }

    fn render(&mut self) {
        let elapsed = self.last_frame.elapsed().as_secs_f32().min(0.1);
        self.last_frame = Instant::now();
        self.fps = self.fps * 0.95 + 0.05 / elapsed.max(1e-4);
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
        let dt = if self.paused { 0.0 } else { elapsed };
        if let Err(error) = state.drive.frame(
            &state.gpu,
            &view,
            size.width as f32 / size.height.max(1) as f32,
            &self.panel,
            dt,
        ) {
            eprintln!("cannot render: {error}");
        }
        let status = format!(
            "{:.0} fps · {:.2} km into the loop{}",
            self.fps,
            state.drive.s / 1000.0,
            if self.paused { " · paused" } else { "" }
        );
        if let Err(error) = state.overlay.render(
            &state.gpu.device,
            &state.gpu.queue,
            &view,
            state.format,
            (size.width, size.height),
            &self.panel,
            &status,
            state.window.scale_factor() as f32,
        ) {
            eprintln!("cannot draw the sliders: {error}");
        }
        state.gpu.queue.present(frame);
    }

    fn layout(&self) -> ui::Layout {
        let scale = self
            .state
            .as_ref()
            .map_or(1.0, |state| state.window.scale_factor() as f32);
        ui::layout(self.panel.sliders.len(), scale)
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
    state
        .drive
        .renderer
        .resize(&state.gpu.device, width, height);
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
            } => {
                if state == ElementState::Pressed {
                    let layout = self.layout();
                    self.looking = !self.panel.press(self.cursor, &layout);
                } else {
                    self.panel.release();
                    self.looking = false;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let pointer = Vec2::new(position.x as f32, position.y as f32);
                let delta = pointer - self.cursor;
                self.cursor = pointer;
                if self.panel.dragging() {
                    let layout = self.layout();
                    self.panel.drag(pointer, &layout);
                } else if self.looking {
                    if let Some(state) = self.state.as_mut() {
                        let orbit = &mut state.drive.orbit;
                        orbit.x -= delta.x * 0.006;
                        orbit.y = (orbit.y + delta.y * 0.004).clamp(-0.35, 1.0);
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(position) => position.y as f32 / 40.0,
                };
                if let Some(state) = self.state.as_mut() {
                    state.drive.zoom = (state.drive.zoom * 0.9f32.powf(lines)).clamp(0.3, 40.0);
                }
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
                KeyCode::Space => self.paused = !self.paused,
                KeyCode::KeyR => {
                    if let Some(state) = self.state.as_mut() {
                        state.drive.orbit = Vec2::ZERO;
                        state.drive.zoom = 1.0;
                    }
                }
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

fn main() -> Result<(), Box<dyn Error>> {
    let options = parse_options()?;
    if let Some(file) = options.screenshot.clone() {
        return screenshot(&options, &file);
    }
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        panel: sliders(&options),
        options,
        state: None,
        error: None,
        paused: false,
        looking: false,
        cursor: Vec2::ZERO,
        last_frame: Instant::now(),
        fps: 60.0,
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
