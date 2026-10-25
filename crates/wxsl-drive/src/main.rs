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
//! In the window: the sliders set the speed, the hour, the motion blur
//! and its quality (`--blur-quality off|fast|good`: the gather along
//! each pixel's motion, or the reconstruction filter, whose fast posts
//! smear over a still background too),
//! the aperture, the focus, the chromatic aberration (an extra pass,
//! off at 0), the tonemap (filmic, ACES, Reinhard, Reinhard-Jodie, WXSL, AgX, Hable), the
//! grade's saturation and the vertical field of view; drag elsewhere to look around the car, drag
//! with the right button to turn the camera on top of looking at it (a
//! right double-click straightens it), scroll to zoom, `R` recentres,
//! `Space` pauses, `Esc` quits. The `car color` slider (or `--paint NAME`)
//! repaints the body: a flaked hornet yellow, a metallic red, a smooth
//! clear-coated blue, white, matte black and an iridescent pearl. The
//! `window color` slider (or `--glass NAME`) tints the windows: the file's
//! own panes, smoked, me_car's iridescent film or a teal mirror. The
//! `neons` slider (or `--neon NAME`) lights an underglow all around the car:
//! off, yellow (the paint's orange), red, blue or white, its light on the
//! road baked once, dimmer the brighter the sky. The
//! `view` slider shows a surface debug view on the car instead of shading
//! it (ADR 0085): albedo, normal, depth, metallic-roughness, mip level, and
//! the rest a forward pass can show; `--view NAME` picks one for a
//! screenshot. The frame is forward. Shadows are hard by default; the `shadows` slider (or
//! `--soft-shadows`) gives the lights their source size back, for
//! contact-hardening penumbrae at a higher shading cost. The `fxaa` slider
//! (or `--no-fxaa`) turns the anti-aliasing off, to compare; the
//! `reflections` slider (or `--ssr coarse|medium|fine`) adds screen-space
//! reflections, the finer the costlier but the thinner what they catch
//! (a wiper), the forward prepass then writing a G-buffer for them. The
//! `halo` slider (or `--halo NAME`) picks what the lens draws around the
//! highlights: nothing, the bloom's glow, or the glow and a star's spikes
//! (cross, starburst, hexagonal, anamorphic); the `ray length` and
//! `ray opacity` sliders (or `--ray-length X`, `--ray-opacity X`) scale
//! how far a star's rays reach and how strongly they add. The
//! `quality` slider (or `--quality x0|x0.5|x1|x2|x4`) scales the car's probe
//! and the shadow atlas from 128² and 2048²; x0 turns both off, the car
//! then reflecting the sky alone. The
//! `screenshot` button under the sliders saves the next frame, without
//! them, as the first free `screenshots/drive-NNN.png` under the working
//! directory. The `save state` button beside it saves what the frame is
//! made of (the sliders, the view, the viewport's size and the car's
//! motion) as `screenshots/state-NNN.json`; `--state FILE` starts there,
//! and with `--screenshot` renders that frame at that size, after the one
//! before it so the motion blur matches.
//!
//! `--scenario FILE` animates the sliders and the camera from a JSON file
//! of keyframes in seconds (`scenario`), such as
//! `crates/wxsl-drive/scenarios/long-take.json`: the window plays it to
//! its music through ffplay, and `K` keys what was changed while paused.
//! With `--record DIR` it renders every frame at the file's rate and size
//! as `DIR/frame-NNNNN.png` and prints the ffmpeg line that makes the
//! video. The `headlights` slider turns the car's lights off or on
//! whatever the hour.

mod car;
mod car_ground;
mod city;
mod flakes;
mod halo;
mod lights;
mod neon;
mod pipeline;
mod road;
mod scenario;
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
use wxsl::render::effect::ssr::Quality;
use wxsl::render::environment::LocalProbe;
use wxsl::render::{
    Camera, DrawList, Environment, GpuContext, OffscreenTarget, RenderRequest, Renderer,
    ShaderLibrary, TargetConfig,
};

use car::Car;
use car_ground::CarGround;
use pipeline::Blur;
use sky::DayLight;
use state::{Motion, Saved};
use ui::{Panel, Slider};
use world::World;

const USAGE: &str = "drive [--hour H] [--speed KMH] [--blur B] [--aperture F] [--focus PCT] [--aberration PCT] [--tonemap NAME] [--saturation S] [--fov DEG] [--fog F]
      [--exposure EV] [--environment EV] [--ground-ao A] [--car-ao A] [--at METRES] [--look RADIANS] [--pitch RADIANS] [--zoom Z]
      [--paint NAME] [--glass NAME] [--neon NAME] [--view NAME] [--soft-shadows] [--no-fxaa] [--ssr off|coarse|medium|fine] [--halo NAME] [--blur-quality off|fast|good] [--ray-length X] [--ray-opacity X] [--quality x0|x0.5|x1|x2|x4]
      [--screenshot out.png [--ui] [--frames N] [--profile]] [--state state.json]
      [--scenario file.json [--record DIR [--from S] [--to S]]]
      [--size WxH] [--car file.glb] [--shadow-car file.glb|none]
--scenario: play a scenario's keyframes, to the music, in the window (Space plays and pauses,
        Left/Right step a frame, a second with Shift, Home goes back to the start, K keys what the
        sliders and the camera changed at that time into the file); with --record, record its frames
        to DIR/frame-NNNNN.png at its fps and size, from S to S seconds into the music
--state: start from a saved state (its sliders, view, motion and size; a --size after it wins);
        with --screenshot, render that frame, after the one before it for the motion blur
window: the sliders set speed, hour, motion blur, aperture, focus, chromatic aberration, tonemap, saturation, field of view, fog, exposure, the environment's intensity, the car's
        occlusion on the road and on itself, its paint, its windows, its neons, its headlights, its debug view, hard or soft shadows, FXAA, the screen-space reflections and the probe and shadow resolution; the screenshot button saves
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
/// The blur's [`pipeline::BLURS`]: off, the gather along each pixel's
/// motion, or the reconstruction filter.
const BLUR_QUALITY: &str = "motion blur quality";
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
const GLASS: &str = "window color";
const NEON: &str = "neons";
/// The car's headlights and tail lights: [`HEADLIGHT_MODES`].
const HEADLIGHTS: &str = "headlights";
/// On with the street lamps, as the hour has them, or off or on whatever
/// the hour.
const HEADLIGHT_MODES: [&str; 3] = ["auto", "off", "on"];
const VIEW: &str = "view";
const SHADOWS: &str = "shadows";
const FXAA: &str = "fxaa";
/// Screen-space reflections: off, then one step per [`SSR_QUALITIES`].
const SSR: &str = "reflections";
const SSR_QUALITIES: [(&str, Quality); 3] = [
    ("coarse", Quality::Coarse),
    ("medium", Quality::Medium),
    ("fine", Quality::Fine),
];
/// The lens' glare: [`pipeline::HALOS`] and [`pipeline::FLARES`].
const HALO: &str = "halo";
/// The glare the frame is first installed with: the bloom alone.
const DEFAULT_GLARE: pipeline::Glare = pipeline::Glare {
    bloom: true,
    star: None,
};
/// How far a star's rays reach, and how strongly they add, times what
/// the star sets.
const RAY_LENGTH: &str = "ray length";
const RAY_OPACITY: &str = "ray opacity";
const QUALITY: &str = "quality";
/// The `quality` slider's steps: the probe's and the shadow atlas's
/// scale, 0 turning both off; x4 is the largest atlas a pipeline may ask
/// for, 8192².
const QUALITIES: [f32; 5] = [0.0, 0.5, 1.0, 2.0, 4.0];
/// How far above the road the camera stays, metres.
const CAMERA_CLEARANCE: f32 = 0.3;
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

/// The reflections' quality the `reflections` slider's `value` picks, or
/// `None` for none.
fn ssr_quality(value: f32) -> Option<(&'static str, Quality)> {
    (value.round() as usize)
        .checked_sub(1)
        .and_then(|index| SSR_QUALITIES.get(index).copied())
}

/// The lens' glare the `halo` slider picks.
fn glare(panel: &Panel) -> pipeline::Glare {
    let (_, bloom, star) = pipeline::HALOS[panel.value(HALO).round() as usize];
    pipeline::Glare { bloom, star }
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
    /// The `motion blur quality` slider's start, an index into
    /// [`pipeline::BLURS`].
    blur_quality: f32,
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
    /// The windows' look, an index into [`car::GLASSES`].
    glass: f32,
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
    /// The `reflections` slider's start: 0 off, then one per
    /// [`SSR_QUALITIES`].
    ssr: f32,
    /// The `halo` slider's start, an index into [`pipeline::HALOS`].
    halo: f32,
    /// The `ray length` and `ray opacity` sliders' starts.
    ray_length: f32,
    ray_opacity: f32,
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
    /// A scenario to play and edit, or with `record`, to record.
    scenario: Option<PathBuf>,
    /// Where to record the scenario's frames.
    record: Option<PathBuf>,
    /// Part of the scenario to record, seconds into the music.
    from: Option<f32>,
    to: Option<f32>,
}

fn parse_options() -> Result<Options, Box<dyn Error>> {
    let mut options = Options {
        hour: 21.0,
        speed: 110.0,
        blur: 0.3,
        blur_quality: 1.0,
        aperture: 8.0,
        focus: 1.0,
        aberration: 0.002,
        tonemap: 4.0,
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
        glass: 0.0,
        neon: 3.0,
        view: 0.0,
        profile: false,
        soft_shadows: true,
        no_fxaa: true,
        ssr: 3.0,
        halo: 1.0,
        ray_length: 1.0,
        ray_opacity: 1.0,
        quality: 3.0,
        screenshot: None,
        ui: false,
        frames: 4,
        size: (1600, 900),
        car: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/glb/me_car/moi.glb"),
        shadow_car: Some(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../resources/glb/me_car_opt/me_car_opt.glb"),
        )
        .filter(|path| path.exists()),
        state: None,
        scenario: None,
        record: None,
        from: None,
        to: None,
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
            "--glass" => {
                let name = value()?;
                options.glass = car::GLASSES
                    .iter()
                    .position(|glass| glass.name == name)
                    .ok_or_else(|| {
                        let names: Vec<&str> =
                            car::GLASSES.iter().map(|glass| glass.name).collect();
                        format!("--glass is one of: {}", names.join(", "))
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
            "--ssr" => {
                let name = value()?;
                options.ssr = match SSR_QUALITIES
                    .iter()
                    .position(|(quality, _)| *quality == name)
                {
                    Some(index) => (index + 1) as f32,
                    None if name == "off" => 0.0,
                    None => return Err("--ssr is one of off, coarse, medium, fine".into()),
                };
            }
            "--halo" => {
                let name = value()?;
                options.halo = pipeline::HALOS
                    .iter()
                    .position(|(halo, ..)| *halo == name)
                    .ok_or(
                        "--halo is one of none, bloom, cross, starburst, hexagonal, anamorphic",
                    )? as f32;
            }
            "--blur-quality" => {
                let name = value()?;
                options.blur_quality = pipeline::BLURS
                    .iter()
                    .position(|(blur, _)| *blur == name)
                    .ok_or("--blur-quality is one of off, fast, good")?
                    as f32;
            }
            "--ray-length" => options.ray_length = value()?.parse()?,
            "--ray-opacity" => options.ray_opacity = value()?.parse()?,
            "--quality" => {
                let name = value()?;
                options.quality = QUALITIES
                    .iter()
                    .position(|scale| format!("x{scale}") == name)
                    .ok_or("--quality is one of x0, x0.5, x1, x2, x4")?
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
            "--scenario" => options.scenario = Some(PathBuf::from(value()?)),
            "--record" => options.record = Some(PathBuf::from(value()?)),
            "--from" => options.from = Some(value()?.parse()?),
            "--to" => options.to = Some(value()?.parse()?),
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
        Slider::new(
            BLUR_QUALITY,
            0.0,
            (pipeline::BLURS.len() - 1) as f32,
            options.blur_quality,
            |value| pipeline::BLURS[value.round() as usize].0.to_string(),
        ),
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
            GLASS,
            0.0,
            (car::GLASSES.len() - 1) as f32,
            options.glass,
            |value| car::GLASSES[value.round() as usize].name.to_string(),
        ),
        Slider::new(
            NEON,
            0.0,
            (neon::NEONS.len() - 1) as f32,
            options.neon,
            |value| neon::NEONS[value.round() as usize].name.to_string(),
        ),
        Slider::new(
            HEADLIGHTS,
            0.0,
            (HEADLIGHT_MODES.len() - 1) as f32,
            0.0,
            |value| HEADLIGHT_MODES[value.round() as usize].to_string(),
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
        Slider::new(SSR, 0.0, SSR_QUALITIES.len() as f32, options.ssr, |value| {
            ssr_quality(value)
                .map_or("off", |(name, _)| name)
                .to_string()
        }),
        Slider::new(
            HALO,
            0.0,
            (pipeline::HALOS.len() - 1) as f32,
            options.halo,
            |value| pipeline::HALOS[value.round() as usize].0.to_string(),
        ),
        Slider::new(RAY_LENGTH, 0.25, 2.0, options.ray_length, |value| {
            format!("×{value:.2}")
        }),
        Slider::new(RAY_OPACITY, 0.0, 2.0, options.ray_opacity, |value| {
            format!("×{value:.2}")
        }),
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
    /// How lit the car's lights were last set, 0 to 1.
    car_lights: Option<f32>,
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
    /// The windows' look last set.
    glass: Option<usize>,
    /// The debug view the car's materials are compiled for, 0 shading.
    shown_view: i32,
    /// The motion blur the frame installed has, with its passes, if any.
    motion_blur: Option<Blur>,
    /// Whether the frame installed ends in FXAA.
    fxaa: bool,
    /// Whether the frame installed has the aberration pass.
    aberration: bool,
    /// The screen-space reflections' quality the frame installed has.
    ssr: Option<Quality>,
    /// The lens' glare the frame installed has.
    glare: pipeline::Glare,
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
                pipeline::install(
                    renderer,
                    refractive,
                    false,
                    Some(Blur::Fast),
                    true,
                    false,
                    None,
                    DEFAULT_GLARE,
                    0,
                    1.0,
                )
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
            car_lights: None,
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
            glass: None,
            shown_view: 0,
            motion_blur: Some(Blur::Fast),
            fxaa: true,
            aberration: false,
            ssr: None,
            glare: DEFAULT_GLARE,
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
        size: (u32, u32),
        panel: &Panel,
        dt: f32,
    ) -> Result<(), Box<dyn Error>> {
        let before = self.motion;
        self.advance(panel, dt);
        if dt > 0.0 {
            self.step = Some((before, dt));
        }
        self.draw(gpu, view, size, panel, dt)
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

    /// Render the motion as it is into `view`, `size` pixels, `dt`
    /// seconds after the last frame.
    fn draw(
        &mut self,
        gpu: &GpuContext,
        view: &wgpu::TextureView,
        (width, height): (u32, u32),
        panel: &Panel,
        dt: f32,
    ) -> Result<(), Box<dyn Error>> {
        let aspect = width as f32 / height.max(1) as f32;
        let Motion {
            s,
            odometer,
            speed,
            acceleration,
            sprung,
            time,
        } = self.motion;
        let speed = speed.unwrap_or(0.0);
        // At 0, or off, the blur and everything only it reads are left out.
        let motion_blur = pipeline::BLURS[panel.value(BLUR_QUALITY).round() as usize]
            .1
            .filter(|_| panel.value(BLUR) > 0.0);
        let fxaa = panel.value(FXAA) >= 0.5;
        let aberration = panel.value(ABERRATION) > 0.0;
        let ssr = ssr_quality(panel.value(SSR)).map(|(_, quality)| quality);
        let tonemap = panel.value(TONEMAP).round() as usize;
        let quality = QUALITIES[panel.value(QUALITY).round() as usize];
        let glare = glare(panel);
        if (motion_blur, fxaa, aberration, ssr, glare, tonemap, quality)
            != (
                self.motion_blur,
                self.fxaa,
                self.aberration,
                self.ssr,
                self.glare,
                self.tonemap,
                self.quality,
            )
        {
            pipeline::install(
                &mut self.renderer,
                self.car.refractive,
                false,
                motion_blur,
                fxaa,
                aberration,
                ssr,
                glare,
                tonemap,
                quality,
            )?;
            self.motion_blur = motion_blur;
            self.fxaa = fxaa;
            self.aberration = aberration;
            self.ssr = ssr;
            self.glare = glare;
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
            self.baked_hour = Some(hour);
        }
        self.world.set_screen_height(gpu, height)?;
        let shown = debug_view(panel.value(VIEW)).map_or(0, |view| view.id);
        if shown != self.shown_view {
            self.car.set_debug_view(gpu, &mut self.renderer, shown)?;
            self.car_lights = None;
            self.car_ao = None;
            self.paint = None;
            self.glass = None;
            self.shown_view = shown;
        }
        let car_lights = match HEADLIGHT_MODES[panel.value(HEADLIGHTS).round() as usize] {
            "off" => 0.0,
            "on" => 1.0,
            _ => self.day.lights_on,
        };
        if self.car_lights != Some(car_lights) {
            self.car.set_lights_on(gpu, car_lights)?;
            self.car_lights = Some(car_lights);
        }
        let paint = panel.value(PAINT).round() as usize;
        if self.paint != Some(paint) {
            self.car
                .set_paint(gpu, &mut self.renderer, &car::PAINTS[paint])?;
            self.paint = Some(paint);
        }
        let glass = panel.value(GLASS).round() as usize;
        if self.glass != Some(glass) {
            self.car.set_glass(gpu, &car::GLASSES[glass])?;
            self.glass = Some(glass);
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
        // Never under the deck: the eye stays above the road's plane at the
        // car, however low the view is pitched.
        let height = (target + offset - frame.at(road::LANE, 0.0)).dot(frame.up);
        offset += frame.up * (CAMERA_CLEARANCE - height).max(0.0);
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
        // The shadow quality's x2 and x4 also take two and four times the
        // soft shadows' taps, for less grain.
        let taps = if self.quality >= 4.0 {
            4
        } else if self.quality >= 2.0 {
            2
        } else {
            1
        };
        for light in &mut lights {
            light.shadow_taps = taps;
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
        // The sun's disc and its flare, while it is up.
        let sun = if self.day.sun_up() {
            self.day.sun_color
        } else {
            Vec3::ZERO
        };
        pipeline::set_sun(&mut self.renderer, self.day.sun, sun)?;
        pipeline::set_rays(
            &mut self.renderer,
            self.glare.star,
            panel.value(RAY_LENGTH),
            panel.value(RAY_OPACITY),
        )?;
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
        let mut draws: DrawList = self.world.draws().into_iter().collect();
        if let Some(quality) = self.motion_blur {
            let strength = Value::F32(panel.value(BLUR));
            self.renderer
                .set_pass_param(pipeline::MOTION_BLUR, "strength", strength)?;
            if quality == Blur::Good {
                self.renderer
                    .set_pass_param(pipeline::MOTION_NEIGHBORS, "strength", strength)?;
            }
        }
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
                drive.draw(&gpu, target.view(), (width, height), &panel, saved.dt)?;
            }
            None => drive.frame(&gpu, target.view(), (width, height), &panel, 1.0 / 60.0)?,
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

/// Every slider and the camera, by [`scenario`]'s names.
fn values(panel: &Panel, drive: &Drive) -> scenario::Values {
    let mut values: scenario::Values = state::sliders(panel);
    let camera = [
        drive.orbit.x,
        drive.orbit.y,
        drive.zoom,
        drive.rotation.x,
        drive.rotation.y,
    ];
    for (name, value) in scenario::CAMERA.into_iter().zip(camera) {
        values.insert(name.to_string(), value);
    }
    values
}

/// Set the sliders and the camera to `values`.
fn set_values(values: &scenario::Values, panel: &mut Panel, drive: &mut Drive) {
    for slider in &mut panel.sliders {
        if let Some(value) = values.get(slider.label) {
            slider.value = value.clamp(slider.min, slider.max);
        }
    }
    let camera = |name: &str, default: f32| values.get(name).copied().unwrap_or(default);
    drive.orbit = Vec2::new(
        camera("look", drive.orbit.x),
        camera("pitch", drive.orbit.y),
    );
    drive.zoom = camera("zoom", drive.zoom).max(0.05);
    drive.rotation = Vec2::new(
        camera("turn", drive.rotation.x),
        camera("tilt", drive.rotation.y),
    );
}

/// Put the drive where `timeline` has it at `t`: the sliders, the camera
/// and the car's motion.
fn seek(
    scenario: &scenario::Scenario,
    timeline: &scenario::Timeline,
    baseline: &scenario::Values,
    t: f32,
    panel: &mut Panel,
    drive: &mut Drive,
) {
    set_values(&timeline.at(t, baseline), panel, drive);
    let mut motion = timeline.motion(scenario.start, scenario.at, t, baseline);
    motion.s = motion.s.rem_euclid(drive.world.road.length());
    drive.motion = motion;
}

/// Record `file`'s scenario as `directory/frame-NNNNN.png`, numbered from
/// its start, and print how ffmpeg makes the video.
fn record(options: &Options, file: &Path, directory: &Path) -> Result<(), Box<dyn Error>> {
    let scenario = scenario::Scenario::load(file)?;
    let mut panel = sliders(options)?;
    let timeline = scenario::Timeline::new(&scenario, &panel)?;
    let [width, height] = scenario.size;
    let gpu = pollster::block_on(GpuContext::headless())?;
    let target = OffscreenTarget::new(&gpu.device, width, height);
    let mut drive = Drive::new(
        &gpu,
        TargetConfig::new(width, height, target.format()),
        &options.car,
        options.shadow_car.as_deref(),
        scenario.at,
    )?;
    let baseline = values(&panel, &drive);
    std::fs::create_dir_all(directory)?;
    let fps = scenario.fps;
    let from = options.from.unwrap_or(scenario.start).max(scenario.start);
    let to = options.to.unwrap_or(scenario.end).min(scenario.end);
    let first = ((from - scenario.start) * fps).round() as u32;
    let last = ((to - scenario.start) * fps).ceil() as u32;
    let started = Instant::now();
    let mut drawn: Option<(f32, f32)> = None;
    // The first frame twice: once for the bakes and for the velocity pass
    // to measure against, which a frozen world leaves still.
    for (pass, index) in std::iter::once(first).chain(first..last).enumerate() {
        let t = scenario.start + index as f32 / fps;
        seek(&scenario, &timeline, &baseline, t, &mut panel, &mut drive);
        let time = drive.motion.time;
        let dt = drawn.map_or(0.0, |(_, drawn)| time - drawn);
        if drawn.is_some_and(|(before, _)| timeline.cuts(before, t)) {
            drive.previous = None;
        }
        drawn = Some((t, time));
        drive.draw(&gpu, target.view(), (width, height), &panel, dt)?;
        gpu.wait();
        if pass == 0 {
            continue;
        }
        let pixels = target.read_rgba8(&gpu.device, &gpu.queue);
        image::save_buffer(
            directory.join(format!("frame-{index:05}.png")),
            &pixels,
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )?;
        let done = index + 1 - first;
        if done % 25 == 0 || index + 1 == last {
            let elapsed = started.elapsed().as_secs_f32();
            println!(
                "{t:.2} s, frame {done}/{}, {:.0} ms a frame",
                last - first,
                elapsed * 1000.0 / done as f32
            );
        }
    }
    let audio = scenario
        .audio(file)
        .map(|audio| {
            format!(
                " -ss {} -i {} -map 0:v -map 1:a -c:a libmp3lame -b:a 320k -shortest",
                scenario.start + first as f32 / fps,
                audio.display()
            )
        })
        .unwrap_or_default();
    println!(
        "ffmpeg -framerate {fps} -start_number {first} -i {}/frame-%05d.png{audio} -c:v libx264 -preset slow -crf 16 -pix_fmt yuv420p {}.mp4",
        directory.display(),
        directory.display()
    );
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

/// A scenario played in the window, to its music, and keyed there.
struct Player {
    file: PathBuf,
    scenario: scenario::Scenario,
    timeline: scenario::Timeline,
    /// The sliders and the camera the scenario's keys go over.
    baseline: scenario::Values,
    /// Seconds into the music.
    t: f32,
    /// When it started playing, and from where.
    playing: Option<(Instant, f32)>,
    /// The music, played by ffplay from `t`.
    audio: Option<std::process::Child>,
    /// When the last frame drawn was, and the world's time then, for the
    /// velocity pass.
    drawn: Option<(f32, f32)>,
    /// The sliders and the camera are still to be put where `t` has them.
    stale: bool,
}

impl Player {
    fn new(file: &Path, panel: &Panel, drive: &Drive) -> Result<Self, Box<dyn Error>> {
        let scenario = scenario::Scenario::load(file)?;
        let timeline = scenario::Timeline::new(&scenario, panel)?;
        Ok(Player {
            file: file.to_path_buf(),
            t: scenario.start,
            scenario,
            timeline,
            baseline: values(panel, drive),
            playing: None,
            audio: None,
            drawn: None,
            stale: true,
        })
    }

    fn toggle(&mut self) {
        if self.playing.is_some() {
            self.pause();
            return;
        }
        if self.t >= self.scenario.end {
            self.t = self.scenario.start;
        }
        self.playing = Some((Instant::now(), self.t));
        let audio = self.scenario.audio(&self.file);
        self.audio = audio.and_then(|audio| {
            std::process::Command::new("ffplay")
                .args(["-nodisp", "-autoexit", "-loglevel", "quiet", "-ss"])
                .arg(self.t.to_string())
                .arg(&audio)
                .stdin(std::process::Stdio::null())
                .spawn()
                .map_err(|error| eprintln!("cannot play {}: {error}", audio.display()))
                .ok()
        });
    }

    fn pause(&mut self) {
        self.playing = None;
        if let Some(mut audio) = self.audio.take() {
            let _ = audio.kill();
            let _ = audio.wait();
        }
    }

    /// Go to `t`, paused, without a velocity from where it was.
    fn seek(&mut self, t: f32) {
        self.pause();
        self.t = t.clamp(self.scenario.start, self.scenario.end);
        self.stale = true;
        self.drawn = None;
    }

    /// Follow the clock while playing, up to the end.
    fn tick(&mut self) {
        let Some((since, from)) = self.playing else {
            return;
        };
        self.t = from + since.elapsed().as_secs_f32();
        if self.t >= self.scenario.end {
            self.t = self.scenario.end;
            self.pause();
        }
        self.stale = true;
    }

    /// Key, at the frame nearest `t`, every slider and camera value that
    /// differs from the scenario's there, and save the file.
    fn key(&mut self, panel: &Panel, drive: &Drive) -> Result<String, Box<dyn Error>> {
        let fps = self.scenario.fps;
        let t = (self.t * fps).round() / fps;
        let wanted = self.timeline.at(t, &self.baseline);
        let changes: std::collections::BTreeMap<String, scenario::Setting> = values(panel, drive)
            .into_iter()
            .filter(|(name, value)| {
                wanted
                    .get(name)
                    .is_none_or(|wanted| (wanted - value).abs() > 1e-4)
            })
            .map(|(name, value)| {
                let setting = scenario::setting(panel, &name, value);
                (name, setting)
            })
            .collect();
        if changes.is_empty() {
            return Ok(format!("nothing changed at {t:.2} s"));
        }
        let names: Vec<&str> = changes.keys().map(String::as_str).collect();
        let message = format!("keyed {} at {t:.2} s", names.join(", "));
        self.scenario.set(t, changes);
        self.timeline = scenario::Timeline::new(&self.scenario, panel)?;
        self.scenario.save(&self.file)?;
        Ok(message)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.pause();
    }
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
    /// The scenario being played, with `--scenario`.
    player: Option<Player>,
    /// Shift is down: the arrows step a second.
    shift: bool,
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
        if let Some(file) = &self.options.scenario {
            self.player = Some(Player::new(file, &self.panel, &drive)?);
        }
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
        // The frame's own size: mid-resize, the window's can already differ
        // from the surface's until `Resized` reconfigures it.
        let size = frame.texture.size();
        let rendered = match self.player.as_mut() {
            Some(player) => {
                player.tick();
                if std::mem::take(&mut player.stale) {
                    seek(
                        &player.scenario,
                        &player.timeline,
                        &player.baseline,
                        player.t,
                        &mut self.panel,
                        &mut state.drive,
                    );
                }
                let time = state.drive.motion.time;
                let dt = player.drawn.map_or(0.0, |(_, drawn)| time - drawn);
                if player
                    .drawn
                    .is_none_or(|(before, _)| player.timeline.cuts(before, player.t))
                {
                    state.drive.previous = None;
                }
                player.drawn = Some((player.t, time));
                state.drive.draw(
                    &state.gpu,
                    &view,
                    (size.width, size.height),
                    &self.panel,
                    dt,
                )
            }
            None => {
                let dt = if self.paused { 0.0 } else { elapsed };
                state.drive.frame(
                    &state.gpu,
                    &view,
                    (size.width, size.height),
                    &self.panel,
                    dt,
                )
            }
        };
        if let Err(error) = rendered {
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
        let status = match &self.player {
            Some(player) => format!(
                "{:.0} fps · {:.2} s{}{captured}",
                self.fps,
                player.t,
                if player.playing.is_some() {
                    ""
                } else {
                    " · paused"
                }
            ),
            None => format!(
                "{:.0} fps · {:.2} km into the loop{}{captured}",
                self.fps,
                state.drive.motion.s / 1000.0,
                if self.paused { " · paused" } else { "" }
            ),
        };
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
            WindowEvent::ModifiersChanged(modifiers) => self.shift = modifiers.state().shift_key(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state: ElementState::Pressed,
                        repeat,
                        ..
                    },
                ..
            } => match (code, self.player.as_mut()) {
                (KeyCode::ArrowLeft | KeyCode::ArrowRight, Some(player)) => {
                    let step = if self.shift {
                        1.0
                    } else {
                        1.0 / player.scenario.fps
                    };
                    let sign = if code == KeyCode::ArrowLeft {
                        -1.0
                    } else {
                        1.0
                    };
                    player.seek(player.t + sign * step);
                }
                _ if repeat => {}
                (KeyCode::Escape | KeyCode::KeyQ, _) => event_loop.exit(),
                (KeyCode::Space, Some(player)) => player.toggle(),
                (KeyCode::Space, None) => self.paused = !self.paused,
                (KeyCode::Home, Some(player)) => player.seek(player.scenario.start),
                (KeyCode::KeyK, Some(player)) => {
                    let message = match self.state.as_ref() {
                        Some(state) => player
                            .key(&self.panel, &state.drive)
                            .unwrap_or_else(|error| format!("cannot key: {error}")),
                        None => return,
                    };
                    println!("{message}");
                    self.captured = Some((message, Instant::now()));
                }
                (KeyCode::KeyR, _) => {
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
    if let (Some(file), Some(directory)) = (&options.scenario, &options.record) {
        return record(&options, file, directory);
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
        player: None,
        shift: false,
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
