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
//! car; depth of field focuses on it, or nearer or farther by the `focus`
//! slider (`--focus PCT`), and the velocity buffer's motion blur
//! smears the road. The car mirrors a probe rendered from its centre every
//! frame, of everything but itself (ADR 0102). Its shadows are cast by a
//! decimated, welded copy without its glass,
//! `resources/glb/me_car_opt/me_car_opt.glb` when it is there
//! (`--shadow-car FILE`, or `none` for the car's own).
//!
//! In the window: the sliders set the speed, the hour, the motion blur,
//! the aperture, the focus, the chromatic aberration (an extra pass,
//! off at 0), the tonemap (filmic, ACES, Reinhard, Reinhard-Jodie), the
//! grade's saturation and the vertical field of view; drag elsewhere to look around the car, drag
//! with the right button to turn the camera on top of looking at it (a
//! right double-click straightens it), scroll to zoom, `R` recentres,
//! `Space` pauses, `Esc` quits. The `car color` slider (or `--paint NAME`)
//! repaints the body: a flaked hornet yellow, a metallic red, a smooth
//! clear-coated blue, white, matte black and an iridescent pearl. The
//! `neons` slider (or `--neon NAME`) lights an underglow all around the car:
//! off, yellow (the paint's orange), red, blue or white, its light on the
//! road baked once, dimmer the brighter the sky. The
//! `view` slider shows a surface debug view on the car instead of shading
//! it (ADR 0085): albedo, normal, depth, metallic-roughness, mip level, and
//! the rest a forward pass can show; `--view NAME` picks one for a
//! screenshot. The frame is forward. Shadows are hard by default; the `shadows` slider (or
//! `--soft-shadows`) gives the lights their source size back, for
//! contact-hardening penumbrae at a higher shading cost. The `fxaa` slider
//! (or `--no-fxaa`) turns the anti-aliasing off, to compare. The
//! `quality` slider (or `--quality x0|x0.5|x1|x2`) scales the car's probe
//! and the shadow atlas from 128² and 2048²; x0 turns both off, the car
//! then reflecting the sky alone. The
//! `screenshot` button under the sliders saves the next frame, without
//! them, as the first free `screenshots/drive-NNN.png` under the working
//! directory. The `save state` button beside it saves what the frame is
//! made of (the sliders, the view, the viewport's size and the car's
//! motion) as `screenshots/state-NNN.json`; `--state FILE` starts there,
//! and with `--screenshot` renders that frame at that size, after the one
//! before it so the motion blur matches.

mod car;
mod car_ground;
mod city;
mod flakes;
mod lights;
mod neon;
mod pipeline;
mod road;
mod sky;
mod state;
mod terrain;
mod ui;
mod world;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use glam::{Mat3, Mat4, Vec2, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};
use wxsl::core::lighting::{DebugStage, DebugView, LightingSet, DEFAULT_MODELS, OVERDRAW_VIEW};
use wxsl::core::Value;
use wxsl::render::environment::LocalProbe;
use wxsl::render::{
    Camera, DrawList, Environment, GpuContext, OffscreenTarget, RenderRequest, Renderer,
    ShaderLibrary, TargetConfig,
};

use car::Car;
use car_ground::CarGround;
use sky::DayLight;
use state::{Motion, Saved};
use ui::{Panel, Slider};
use world::World;

const USAGE: &str = "drive [--hour H] [--speed KMH] [--blur B] [--aperture F] [--focus PCT] [--aberration PCT] [--tonemap NAME] [--saturation S] [--fov DEG] [--fog F]
      [--exposure EV] [--environment EV] [--ground-ao A] [--car-ao A] [--at METRES] [--look RADIANS] [--pitch RADIANS] [--zoom Z]
      [--paint NAME] [--neon NAME] [--view NAME] [--soft-shadows] [--no-fxaa] [--quality x0|x0.5|x1|x2]
      [--screenshot out.png [--ui] [--frames N] [--profile]] [--state state.json]
      [--size WxH] [--car file.glb] [--shadow-car file.glb|none]
--state: start from a saved state (its sliders, view, motion and size; a --size after it wins);
        with --screenshot, render that frame, after the one before it for the motion blur
window: the sliders set speed, hour, motion blur, aperture, focus, chromatic aberration, tonemap, saturation, field of view, fog, exposure, the environment's intensity, the car's
        occlusion on the road and on itself, its paint, its neons, its debug view, hard or soft shadows, FXAA and the probe and shadow resolution; the screenshot button saves
        the frame to screenshots/drive-NNN.png, save state the scene to screenshots/state-NNN.json;
        drag elsewhere looks around the car, right-drag turns the camera (right double-click
        straightens it), scroll zooms, R recentres, Space pauses, Esc quits";

const SPEED: &str = "speed";
/// The car's best acceleration and braking, m/s².
const ACCELERATION: f32 = 4.5;
const BRAKING: f32 = 9.0;
/// How fast the speed closes on the slider's, per second, before the
/// limits: the last few km/h ease in.
const SPEED_GAIN: f32 = 1.5;
/// The deceleration the brake lights start at and are full by, m/s².
const BRAKE_LIGHT: (f32, f32) = (0.5, 3.0);
/// How long the body takes to settle into a pitch, seconds.
const SPRING_LAG: f32 = 0.25;
const HOUR: &str = "time of day";
const BLUR: &str = "motion blur";
const APERTURE: &str = "aperture";
/// The focus distance, a factor of the car's centre's, shown in %: a
/// state saved before it had one keeps 1.
const FOCUS: &str = "focus";
/// The chromatic aberration's pull at the frame's edge, uv, shown in %:
/// 0 leaves its pass out.
const ABERRATION: &str = "aberration";
/// The display transform, an index into [`pipeline::TONEMAPS`].
const TONEMAP: &str = "tonemap";
/// The grade's saturation, the last change to the radiance before the
/// tonemap: 0 grey, 1 as shaded.
const SATURATION: &str = "saturation";
/// The vertical field of view, degrees.
const FOV: &str = "fov";
const FOG: &str = "fog";
const EXPOSURE: &str = "exposure";
/// Stops on the environment's radiance alone: the sky's ambient and
/// reflections, the background, the fog's light, not the lamps or the
/// emissives.
const ENVIRONMENT: &str = "environment";
const GROUND_AO: &str = "ground AO";
const CAR_AO: &str = "car AO";
const PAINT: &str = "car color";
const NEON: &str = "neons";
const VIEW: &str = "view";
const SHADOWS: &str = "shadows";
const FXAA: &str = "fxaa";
const QUALITY: &str = "quality";
/// The `quality` slider's steps: the probe's and the shadow atlas's
/// scale, 0 turning both off.
const QUALITIES: [f32; 4] = [0.0, 0.5, 1.0, 2.0];
/// How long a second right click may wait to be a double click.
const DOUBLE_CLICK: f32 = 0.4;
/// The fog's density per metre at the ground, at the `fog` slider's 1:
/// about a third of the light survives a kilometre at ground level.
const FOG_DENSITY: f32 = 0.0012;

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
    /// The focus distance, a factor of the distance to the car's centre.
    focus: f32,
    /// The chromatic aberration, uv at the frame's edge.
    aberration: f32,
    /// The tonemap, an index into [`pipeline::TONEMAPS`].
    tonemap: f32,
    /// The grade's saturation.
    saturation: f32,
    /// The vertical field of view, degrees.
    fov: f32,
    /// The fog's density, relative to [`FOG_DENSITY`].
    fog: f32,
    /// Stops over the exposure the hour adapts to.
    exposure: f32,
    /// Stops on the environment's radiance.
    environment: f32,
    /// How much the car darkens the road under it, 0 to 1.
    ground_ao: f32,
    /// How much the car's PRT bake darkens the car itself, 0 to 1.
    car_ao: f32,
    at: f32,
    /// How far the view starts turned around the car, radians.
    look: f32,
    /// How far it starts tilted down onto the car, radians.
    pitch: f32,
    zoom: f32,
    /// The body paint, an index into [`car::PAINTS`].
    paint: f32,
    /// The underglow, an index into [`neon::NEONS`].
    neon: f32,
    /// The `view` slider's start: 0 shades, then one per [`debug_views`].
    view: f32,
    /// Print the screenshot's GPU time per pass.
    profile: bool,
    /// Start with the lights' soft, contact-hardening shadows rather than
    /// hard ones.
    soft_shadows: bool,
    /// Start without FXAA.
    no_fxaa: bool,
    /// The `quality` slider's start, an index into [`QUALITIES`].
    quality: f32,
    screenshot: Option<PathBuf>,
    /// Draw the sliders into the screenshot too.
    ui: bool,
    /// How many frames the screenshot renders first, timed.
    frames: u32,
    size: (u32, u32),
    car: PathBuf,
    /// A decimated copy of the car that casts its shadows in its place.
    shadow_car: Option<PathBuf>,
    /// A saved state to start from.
    state: Option<Saved>,
}

fn parse_options() -> Result<Options, Box<dyn Error>> {
    let mut options = Options {
        hour: 21.0,
        speed: 110.0,
        blur: 0.3,
        aperture: 8.0,
        focus: 1.0,
        aberration: 0.0,
        tonemap: 0.0,
        saturation: pipeline::SATURATION,
        fov: 55.0,
        fog: 0.7,
        exposure: 0.0,
        environment: 0.0,
        ground_ao: 0.8,
        car_ao: 0.8,
        at: 0.0,
        look: 0.0,
        pitch: 0.0,
        zoom: 1.0,
        paint: 0.0,
        neon: 0.0,
        view: 0.0,
        profile: false,
        soft_shadows: false,
        no_fxaa: false,
        quality: 2.0,
        screenshot: None,
        ui: false,
        frames: 4,
        size: (1600, 900),
        car: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/glb/me_car/me_car.glb"),
        shadow_car: Some(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../resources/glb/me_car_opt/me_car_opt.glb"),
        )
        .filter(|path| path.exists()),
        state: None,
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || arguments.next().ok_or(format!("{argument} needs a value"));
        match argument.as_str() {
            "--hour" => options.hour = value()?.parse()?,
            "--speed" => options.speed = value()?.parse()?,
            "--blur" => options.blur = value()?.parse()?,
            "--aperture" => options.aperture = value()?.parse()?,
            "--focus" => options.focus = value()?.parse::<f32>()? / 100.0,
            "--aberration" => options.aberration = value()?.parse::<f32>()? / 100.0,
            "--fog" => options.fog = value()?.parse()?,
            "--exposure" => options.exposure = value()?.parse()?,
            "--environment" => options.environment = value()?.parse()?,
            "--ground-ao" => options.ground_ao = value()?.parse()?,
            "--car-ao" => options.car_ao = value()?.parse()?,
            "--at" => options.at = value()?.parse()?,
            "--look" => options.look = value()?.parse()?,
            "--pitch" => options.pitch = value()?.parse()?,
            "--zoom" => options.zoom = value()?.parse::<f32>()?.clamp(0.3, 40.0),
            "--fov" => options.fov = value()?.parse()?,
            "--saturation" => options.saturation = value()?.parse()?,
            "--tonemap" => {
                let name = value()?;
                options.tonemap = pipeline::TONEMAPS
                    .iter()
                    .position(|(tonemap, ..)| tonemap.eq_ignore_ascii_case(&name))
                    .ok_or_else(|| {
                        let names: Vec<&str> =
                            pipeline::TONEMAPS.iter().map(|(name, ..)| *name).collect();
                        format!("--tonemap is one of: {}", names.join(", "))
                    })? as f32;
            }
            "--paint" => {
                let name = value()?;
                options.paint = car::PAINTS
                    .iter()
                    .position(|paint| paint.name == name)
                    .ok_or_else(|| {
                        let names: Vec<&str> = car::PAINTS.iter().map(|paint| paint.name).collect();
                        format!("--paint is one of: {}", names.join(", "))
                    })? as f32;
            }
            "--neon" => {
                let name = value()?;
                options.neon = neon::NEONS
                    .iter()
                    .position(|neon| neon.name == name)
                    .ok_or_else(|| {
                        let names: Vec<&str> = neon::NEONS.iter().map(|neon| neon.name).collect();
                        format!("--neon is one of: {}", names.join(", "))
                    })? as f32;
            }
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
            "--profile" => options.profile = true,
            "--soft-shadows" => options.soft_shadows = true,
            "--no-fxaa" => options.no_fxaa = true,
            "--quality" => {
                let name = value()?;
                options.quality = QUALITIES
                    .iter()
                    .position(|scale| format!("x{scale}") == name)
                    .ok_or("--quality is one of x0, x0.5, x1, x2")?
                    as f32;
            }
            "--screenshot" => options.screenshot = Some(PathBuf::from(value()?)),
            "--ui" => options.ui = true,
            "--frames" => options.frames = value()?.parse::<u32>()?.max(3),
            "--car" => options.car = PathBuf::from(value()?),
            "--shadow-car" => {
                options.shadow_car = Some(value()?)
                    .filter(|name| name != "none")
                    .map(PathBuf::from);
            }
            "--state" => {
                let saved = Saved::load(Path::new(&value()?))?;
                options.size = (saved.width, saved.height);
                options.state = Some(saved);
            }
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

/// The sliders at `options`, or at its saved state's values.
fn sliders(options: &Options) -> Result<Panel, Box<dyn Error>> {
    let mut panel = Panel::new(vec![
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
        Slider::new(FOCUS, 0.1, 3.0, options.focus, |value| {
            format!("{:.0}%", value * 100.0)
        }),
        Slider::new(ABERRATION, 0.0, 0.02, options.aberration, |value| {
            if value > 0.0 {
                format!("{:.2}%", value * 100.0)
            } else {
                "off".to_string()
            }
        }),
        Slider::new(
            TONEMAP,
            0.0,
            (pipeline::TONEMAPS.len() - 1) as f32,
            options.tonemap,
            |value| pipeline::TONEMAPS[value.round() as usize].0.to_string(),
        ),
        Slider::new(SATURATION, 0.0, 2.0, options.saturation, |value| {
            format!("{value:.2}")
        }),
        Slider::new(FOV, 20.0, 100.0, options.fov, |value| {
            format!("{value:.0}°")
        }),
        Slider::new(FOG, 0.0, 4.0, options.fog, |value| format!("{value:.2}")),
        Slider::new(EXPOSURE, -4.0, 4.0, options.exposure, |value| {
            format!("{value:+.1} EV")
        }),
        Slider::new(ENVIRONMENT, -4.0, 4.0, options.environment, |value| {
            format!("{value:+.1} EV")
        }),
        Slider::new(GROUND_AO, 0.0, 1.0, options.ground_ao, |value| {
            format!("{value:.2}")
        }),
        Slider::new(CAR_AO, 0.0, 1.0, options.car_ao, |value| {
            format!("{value:.2}")
        }),
        Slider::new(
            PAINT,
            0.0,
            (car::PAINTS.len() - 1) as f32,
            options.paint,
            |value| car::PAINTS[value.round() as usize].name.to_string(),
        ),
        Slider::new(
            NEON,
            0.0,
            (neon::NEONS.len() - 1) as f32,
            options.neon,
            |value| neon::NEONS[value.round() as usize].name.to_string(),
        ),
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
        Slider::new(
            QUALITY,
            0.0,
            (QUALITIES.len() - 1) as f32,
            options.quality,
            |value| format!("x{}", QUALITIES[value.round() as usize]),
        ),
    ]);
    if let Some(saved) = &options.state {
        saved.apply_sliders(&mut panel)?;
    }
    Ok(panel)
}

/// The scene, the renderer, and where the drive is.
struct Drive {
    renderer: Renderer,
    library: ShaderLibrary,
    world: World,
    car: Car,
    /// The car's occlusion on the road, when the car has a baked ground,
    /// and the strength last set on it.
    ground: Option<(CarGround, Option<f32>)>,
    /// The neons' light on the road.
    underglow: neon::Underglow,
    /// The PRT strength last set on the car's materials.
    car_ao: Option<f32>,
    /// Where the car is and how it goes; its speed follows the slider
    /// within [`ACCELERATION`] and [`BRAKING`].
    motion: Motion,
    /// The motion before the last frame that moved, and the seconds it
    /// took: what a saved state renders first.
    step: Option<(Motion, f32)>,
    /// The hour the sky was last baked for.
    baked_hour: Option<f32>,
    day: DayLight,
    /// Where the car and the camera were last frame, for the velocity pass.
    previous: Option<(Mat4, Camera)>,
    /// How far the view is turned around the car: yaw, pitch.
    orbit: Vec2,
    /// How far the camera is from the car, relative to the chase distance.
    zoom: f32,
    /// How far the camera is turned from looking at the car: yaw (alpha),
    /// pitch (beta), radians.
    rotation: Vec2,
    /// The paint last put on the car.
    paint: Option<usize>,
    /// The debug view the car's materials are compiled for, 0 shading.
    shown_view: i32,
    /// Whether the frame installed ends in FXAA.
    fxaa: bool,
    /// Whether the frame installed has the aberration pass.
    aberration: bool,
    /// The tonemap the frame installed ends in.
    tonemap: usize,
    /// The probe and shadow scale the frame installed has.
    quality: f32,
}

impl Drive {
    fn new(
        gpu: &GpuContext,
        target: TargetConfig,
        car: &Path,
        shadow_car: Option<&Path>,
        at: f32,
    ) -> Result<Self, Box<dyn Error>> {
        let started = Instant::now();
        let (registry, library) = world::libraries()?;
        let mut renderer = Renderer::new(&gpu.device, library.clone(), target)?;
        pipeline::add_effects(&mut renderer)?;
        renderer.set_lighting(LightingSet::new(DEFAULT_MODELS.iter().copied())?)?;
        let ground =
            CarGround::load(gpu, &mut renderer, &registry, car)?.map(|ground| (ground, None));
        let car = Car::open(
            gpu,
            &mut renderer,
            car,
            shadow_car,
            |renderer, refractive| {
                pipeline::install(renderer, refractive, false, true, false, 0, 1.0)
            },
        )?;
        let world = World::new(gpu, &mut renderer, &registry)?;
        let underglow = neon::Underglow::new(gpu, &mut renderer, &registry, &car.neon_strips())?;
        renderer.set_light_cookies(&gpu.device, Some(&lights::cookies(gpu)?));
        println!(
            "road {:.1} km, {} lamps, {} buildings, car {:.2} × {:.2} × {:.2} m, ready in {:.1?}",
            world.road.length() / 1000.0,
            world.lamps.len(),
            world.building_count(),
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
            ground,
            underglow,
            car_ao: None,
            motion: Motion {
                s: at,
                ..Motion::default()
            },
            step: None,
            baked_hour: None,
            day: DayLight::at(12.0),
            previous: None,
            orbit: Vec2::ZERO,
            zoom: 1.0,
            rotation: Vec2::ZERO,
            paint: None,
            shown_view: 0,
            fxaa: true,
            aberration: false,
            tonemap: 0,
            quality: 1.0,
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
        let before = self.motion;
        self.advance(panel, dt);
        if dt > 0.0 {
            self.step = Some((before, dt));
        }
        self.draw(gpu, view, aspect, panel, dt)
    }

    fn advance(&mut self, panel: &Panel, dt: f32) {
        let motion = &mut self.motion;
        // Towards the slider's speed, easing in near it, within what the
        // car can do; the first frame starts at it.
        let wanted = panel.value(SPEED) / 3.6;
        let current = motion.speed.unwrap_or(wanted);
        let acceleration = ((wanted - current) * SPEED_GAIN).clamp(-BRAKING, ACCELERATION);
        let speed = (current + acceleration * dt).max(0.0);
        motion.speed = Some(speed);
        motion.acceleration = acceleration;
        motion.sprung += (acceleration - motion.sprung) * (dt / SPRING_LAG).min(1.0);
        motion.s = (motion.s + speed * dt).rem_euclid(self.world.road.length());
        motion.odometer += speed * dt;
        motion.time += dt;
    }

    /// The state to save: this frame's, with the window's `size`.
    fn saved(&self, panel: &Panel, (width, height): (u32, u32)) -> Saved {
        let (previous, dt) = self.step.unwrap_or((self.motion, 0.0));
        Saved {
            width,
            height,
            sliders: state::sliders(panel),
            orbit: self.orbit.to_array(),
            zoom: self.zoom,
            rotation: self.rotation.to_array(),
            motion: self.motion,
            previous,
            dt,
        }
    }

    /// Render the motion as it is into `view`, `dt` seconds after the last
    /// frame.
    fn draw(
        &mut self,
        gpu: &GpuContext,
        view: &wgpu::TextureView,
        aspect: f32,
        panel: &Panel,
        dt: f32,
    ) -> Result<(), Box<dyn Error>> {
        let Motion {
            s,
            odometer,
            speed,
            acceleration,
            sprung,
            time,
        } = self.motion;
        let speed = speed.unwrap_or(0.0);
        let fxaa = panel.value(FXAA) >= 0.5;
        let aberration = panel.value(ABERRATION) > 0.0;
        let tonemap = panel.value(TONEMAP).round() as usize;
        let quality = QUALITIES[panel.value(QUALITY).round() as usize];
        if (fxaa, aberration, tonemap, quality)
            != (self.fxaa, self.aberration, self.tonemap, self.quality)
        {
            pipeline::install(
                &mut self.renderer,
                self.car.refractive,
                false,
                fxaa,
                aberration,
                tonemap,
                quality,
            )?;
            self.fxaa = fxaa;
            self.aberration = aberration;
            self.tonemap = tonemap;
            self.quality = quality;
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
            self.car_ao = None;
            self.paint = None;
            self.shown_view = shown;
        }
        let paint = panel.value(PAINT).round() as usize;
        if self.paint != Some(paint) {
            self.car
                .set_paint(gpu, &mut self.renderer, &car::PAINTS[paint])?;
            self.paint = Some(paint);
        }
        let car_ao = panel.value(CAR_AO);
        if self.car_ao != Some(car_ao) {
            self.car.set_prt_strength(gpu, car_ao)?;
            self.car_ao = Some(car_ao);
        }
        // The brake lights come on past a light lift off, full by a firm
        // stop.
        let braking =
            ((-acceleration - BRAKE_LIGHT.0) / (BRAKE_LIGHT.1 - BRAKE_LIGHT.0)).clamp(0.0, 1.0);
        self.car.set_braking(gpu, braking)?;
        // In the lane, right of the centre line: a wider bend to the left,
        // a tighter one to the right.
        let bend = self.world.road.curvature(s);
        let curvature = bend / (1.0 + bend * road::LANE);
        self.car.animate(gpu, time, odometer, curvature, sprung)?;

        let road = &self.world.road;
        let frame = road.frame(s);
        let transform = self.car.placement(&frame, road::LANE);
        let center = transform.transform_point3(Vec3::Y * self.car.size.y * 0.5);
        // Behind on the road itself, so the camera takes the bends with the
        // car; further back the faster it goes.
        let behind = road.frame(s - 6.0 - speed * 0.04);
        let target = center + frame.forward * 1.5 + frame.up * 0.2;
        let mut offset = behind.at(road::LANE, 2.0) - target;
        offset = Mat4::from_rotation_y(self.orbit.x).transform_vector3(offset);
        let side = offset.cross(Vec3::Y).normalize_or(Vec3::X);
        offset = Mat4::from_axis_angle(side, self.orbit.y).transform_vector3(offset) * self.zoom;
        // The turn, from the eye: yaw about the vertical, then pitch about
        // the camera's right.
        let mut look = Mat3::from_rotation_y(self.rotation.x) * -offset;
        let right = look.cross(Vec3::Y).normalize_or(Vec3::X);
        look = Mat3::from_axis_angle(right, self.rotation.y) * look;
        let camera = Camera {
            eye: target + offset,
            target: target + offset + look,
            up: Vec3::Y,
            fov_y: panel.value(FOV).to_radians(),
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
        if self.quality == 0.0 {
            for light in &mut lights {
                light.casts_shadow = false;
            }
        }
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
            exposure: self.day.exposure * panel.value(EXPOSURE).exp2(),
            time,
            previous_time: time - dt,
            // The car's reflections, from its own centre (ADR 0102).
            probe: (self.quality > 0.0).then_some(LocalProbe {
                center,
                near: 0.05,
                far: camera.far,
            }),
            shadow_atlas_size: self
                .renderer
                .shadow_atlas_size()
                .unwrap_or(wxsl::core::abi::SHADOW_ATLAS_SIZE),
            ..Environment::default()
        };
        pipeline::set_lens(
            &mut self.renderer,
            &camera,
            camera.eye.distance(center) * panel.value(FOCUS),
            panel.value(APERTURE),
        )?;
        pipeline::set_fog(&mut self.renderer, panel.value(FOG) * FOG_DENSITY)?;
        self.renderer
            .set_environment_scale(panel.value(ENVIRONMENT).exp2());
        let rows = pipeline::grade_rows(panel.value(SATURATION));
        for (name, row) in ["red", "green", "blue"].into_iter().zip(rows) {
            self.renderer
                .set_pass_param(pipeline::GRADE, name, Value::Vec4(row))?;
        }
        if self.aberration {
            self.renderer.set_pass_param(
                pipeline::ABERRATION,
                "strength",
                Value::F32(panel.value(ABERRATION)),
            )?;
        }
        if let Some((ground, shown)) = &mut self.ground {
            let strength = panel.value(GROUND_AO);
            if *shown != Some(strength) {
                ground.set_strength(gpu, strength)?;
                *shown = Some(strength);
            }
        }
        self.renderer.set_pass_param(
            pipeline::MOTION_BLUR,
            "strength",
            Value::F32(panel.value(BLUR)),
        )?;
        let mut draws: DrawList = self.world.draws().into_iter().collect();
        for item in self.car.draws(transform, previous_transform) {
            draws.push(item);
        }
        if let Some((ground, _)) = &self.ground {
            draws.push(ground.draw(transform * self.car.align()));
        }
        // Dimmer against a bright sky.
        let glow = neon::against_sky(self.day.sky);
        self.underglow.set_color(
            gpu,
            neon::NEONS[panel.value(NEON).round() as usize]
                .color
                .map(|color| color * glow),
        )?;
        for draw in self.underglow.draws(transform) {
            draws.push(draw);
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
        options.shadow_car.as_deref(),
        options.at,
    )?;
    drive.orbit = Vec2::new(options.look, options.pitch);
    drive.zoom = options.zoom;
    if let Some(saved) = &options.state {
        drive.orbit = Vec2::from_array(saved.orbit);
        drive.zoom = saved.zoom;
        drive.rotation = Vec2::from_array(saved.rotation);
    }
    let panel = sliders(options)?;
    let aspect = width as f32 / height as f32;
    // Frames at 60 Hz: the first runs the bakes, and the velocity pass
    // needs a previous frame to measure against. The rest are timed. A
    // saved state renders its frame before until the last, which is its
    // own.
    let mut timed = std::time::Duration::ZERO;
    let mut profile = Profile::default();
    for frame in 0..options.frames {
        // On after the first frame, which runs the bakes.
        if frame == 1 && options.profile && !profile.start(&gpu, &mut drive.renderer) {
            println!("this device has no timestamp queries: no pass timings");
        }
        let started = Instant::now();
        match &options.state {
            Some(saved) => {
                drive.motion = if frame + 1 == options.frames {
                    saved.motion
                } else {
                    saved.previous
                };
                drive.draw(&gpu, target.view(), aspect, &panel, saved.dt)?;
            }
            None => drive.frame(&gpu, target.view(), aspect, &panel, 1.0 / 60.0)?,
        }
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
    /// Whether the surface can be copied from, for the screenshot button.
    readable: bool,
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
    /// The right button is down outside the panel: the mouse turns the
    /// camera.
    turning: bool,
    /// When the right button was last pressed, for the double click.
    right_click: Option<Instant>,
    cursor: Vec2,
    last_frame: Instant,
    /// Frame times, for the status line.
    fps: f32,
    /// The screenshot button was pressed: save the next frame.
    capture: bool,
    /// The save button was pressed: save the next frame's state.
    save: bool,
    /// What the last capture did, and when, for the status line.
    captured: Option<(String, Instant)>,
}

impl App {
    fn create_state(&mut self, event_loop: &ActiveEventLoop) -> Result<State, Box<dyn Error>> {
        let (width, height) = self.options.size;
        let mut attributes = Window::default_attributes().with_title("wxsl drive");
        // A saved state's size is its viewport's, in pixels.
        attributes = match self.options.state {
            Some(_) => attributes.with_inner_size(winit::dpi::PhysicalSize::new(width, height)),
            None => attributes.with_inner_size(winit::dpi::LogicalSize::new(width, height)),
        };
        let window = Arc::new(event_loop.create_window(attributes)?);
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
        let readable = capabilities.usages.contains(wgpu::TextureUsages::COPY_SRC);
        let size = window.inner_size();
        let mut drive = Drive::new(
            &gpu,
            TargetConfig::new(size.width.max(1), size.height.max(1), format),
            &self.options.car,
            self.options.shadow_car.as_deref(),
            self.options.at,
        )?;
        if let Some(saved) = &self.options.state {
            drive.orbit = Vec2::from_array(saved.orbit);
            drive.zoom = saved.zoom;
            drive.rotation = Vec2::from_array(saved.rotation);
            drive.motion = saved.motion;
        }
        let overlay = ui::Overlay::new(&gpu.device, &drive.library)?;
        let mut state = State {
            window,
            surface,
            gpu,
            format,
            readable,
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
        // Before the sliders are drawn over it.
        if std::mem::take(&mut self.capture) {
            let message = if state.readable {
                match save_frame(&state.gpu, &frame.texture, state.format) {
                    Ok(file) => format!("saved {}", file.display()),
                    Err(error) => format!("cannot save the frame: {error}"),
                }
            } else {
                "this surface cannot be read back".to_string()
            };
            println!("{message}");
            self.captured = Some((message, Instant::now()));
        }
        if std::mem::take(&mut self.save) {
            let saved = state.drive.saved(&self.panel, (size.width, size.height));
            let message = match saved.save(Path::new(SCREENSHOTS)) {
                Ok(file) => format!("saved {}", file.display()),
                Err(error) => format!("cannot save the state: {error}"),
            };
            println!("{message}");
            self.captured = Some((message, Instant::now()));
        }
        let captured = match &self.captured {
            Some((message, at)) if at.elapsed().as_secs_f32() < 4.0 => format!(" · {message}"),
            _ => String::new(),
        };
        let status = format!(
            "{:.0} fps · {:.2} km into the loop{}{captured}",
            self.fps,
            state.drive.motion.s / 1000.0,
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

/// Where the screenshot button saves, from the working directory
/// (git-ignored).
const SCREENSHOTS: &str = "screenshots";

/// Read `texture`, the window's frame, back and save it as the first free
/// `screenshots/drive-NNN.png`.
fn save_frame(
    gpu: &GpuContext,
    texture: &wgpu::Texture,
    format: wgpu::TextureFormat,
) -> Result<PathBuf, Box<dyn Error>> {
    let bgra = match format {
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => false,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => true,
        other => return Err(format!("a {other:?} frame is not 8-bit RGBA").into()),
    };
    let (width, height) = (texture.width(), texture.height());
    let row = width * 4;
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = row.div_ceil(alignment) * alignment;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("drive screenshot"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("drive screenshot"),
        });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely())?;
    let mut pixels = Vec::with_capacity((row * height) as usize);
    for line in buffer.slice(..).get_mapped_range()?.chunks(padded as usize) {
        pixels.extend_from_slice(&line[..row as usize]);
    }
    buffer.unmap();
    if bgra {
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
    }
    let directory = Path::new(SCREENSHOTS);
    std::fs::create_dir_all(directory)?;
    let file = (1..)
        .map(|index| directory.join(format!("drive-{index:03}.png")))
        .find(|file| !file.exists())
        .expect("a free name");
    image::save_buffer(
        &file,
        &pixels,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(file)
}

fn configure_surface(state: &mut State, width: u32, height: u32) {
    let (width, height) = (width.max(1), height.max(1));
    let usage = if state.readable {
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC
    } else {
        wgpu::TextureUsages::RENDER_ATTACHMENT
    };
    state.surface.configure(
        &state.gpu.device,
        &wgpu::SurfaceConfiguration {
            usage,
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
                    self.capture |= layout.screenshot.contains(self.cursor);
                    self.save |= layout.save.contains(self.cursor);
                    self.looking = !self.panel.press(self.cursor, &layout);
                } else {
                    self.panel.release();
                    self.looking = false;
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Right,
                ..
            } => {
                if state == ElementState::Pressed {
                    self.turning = !self.layout().panel.contains(self.cursor);
                    let now = Instant::now();
                    let double = self
                        .right_click
                        .is_some_and(|last| now.duration_since(last).as_secs_f32() < DOUBLE_CLICK);
                    self.right_click = (!double).then_some(now);
                    if let (true, true, Some(state)) = (double, self.turning, self.state.as_mut()) {
                        state.drive.rotation = Vec2::ZERO;
                    }
                } else {
                    self.turning = false;
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
                } else if self.turning {
                    if let Some(state) = self.state.as_mut() {
                        // Look where the mouse goes.
                        let rotation = &mut state.drive.rotation;
                        rotation.x -= delta.x * 0.003;
                        rotation.y = (rotation.y - delta.y * 0.003).clamp(-1.4, 1.4);
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
                        state.drive.rotation = Vec2::ZERO;
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
        panel: sliders(&options)?,
        options,
        state: None,
        error: None,
        paused: false,
        looking: false,
        turning: false,
        right_click: None,
        cursor: Vec2::ZERO,
        last_frame: Instant::now(),
        fps: 60.0,
        capture: false,
        save: false,
        captured: None,
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
