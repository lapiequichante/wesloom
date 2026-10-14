//! The workspace's demos in one window — or, with `--screenshot`, one
//! PNG each plus a contact sheet.
//!
//! Most demos here are the same PBR cube (`assets/pbr_cube.wxsl.json`)
//! through a *different pipeline*, because pipelines are the thing this
//! stretch of work made cheap: two stock presets, the minimal
//! single-pass document, and the deferred-plus-bloom chain that screen
//! effects make expressible as a document edit. The last three demos are
//! the plan2 P10–P12 proofs: a compute effect baking the BRDF LUT under
//! policy `once`, a storage buffer filled by compute and drawn as one
//! value per column, and the deferred pipeline with the subsurface
//! feature's channel in its G-buffer. `ibl` and `ibl-mirror` use a local
//! HDR image (or `--ibl-source sky`), convolved into diffuse and GGX cubes
//! and combined with the split-sum table, without direct lamps (ADR 0063). The
//! tenth, `fxaa`, is ADR 0040's: the stock forward chain with an
//! anti-aliasing pass appended, where *both* screen effects in it — the
//! anti-aliaser and the display transform every other demo also presents
//! through — are node graphs rather than shader files. The eleventh,
//! `bloom-tuned`, is ADR 0042's: the same bloom chain with its parameters
//! tuned through `set_pass_param` — a buffer write, no recompile. The bloom
//! in these demos is two passes, a horizontal Gaussian then a vertical one,
//! because the one-pass kernel's four-texel stride bands.
//! The `peel` demo is ADR 0047: the same PBR cube graph, with an `alpha`
//! uniform at 0.3, on a torus and a sphere that pass through each other,
//! composited by dual depth peeling.
//! `glass` is the optical counterpart (ADR 0068): IOR, front/back interfaces,
//! Beer absorption and HDR refraction; its torus budgets two crossings and
//! its sphere one, composed back to front. `--refraction-quality precise`
//! retains the expensive ray marcher for comparison.
//! Nothing below is a renderer feature. The bloom pipeline is a document
//! — lighting into a colour target, then the two bloom passes — compiled
//! by the same public `compile_pipeline` an application would call, and the
//! proof effects are registered the same way an application's would be.
//!
//! ```text
//! cargo run --example gallery                          # windowed
//! cargo run --example gallery -- --screenshot          # PNGs into ./gallery
//! cargo run --example gallery -- --list                # what's in it
//! ```
//!
//! In the window: `Left`/`Right` (or `[`/`]`) step through the demos,
//! `1`–`9` jumps, `Space` pauses the clock, `Esc` quits. The title bar
//! and the console name the demo and what it runs.
//!
//! In a browser (WebGPU; ADR 0075), starting on `?demo=<name>`, by
//! default `glass-probes`:
//!
//! ```sh
//! cargo build -p wxsl --example gallery --features webgl --target wasm32-unknown-unknown --release
//! wasm-bindgen --target web --out-dir target/gallery-web --no-typescript \
//!     target/wasm32-unknown-unknown/release/examples/gallery.wasm
//! cp crates/wxsl/examples/gallery.html target/gallery-web/index.html
//! python3 -m http.server -d target/gallery-web 8000
//! ```

// The screenshot and command-line paths are native-only.
#![cfg_attr(target_arch = "wasm32", allow(dead_code, unused_imports))]
use std::error::Error;
use std::f32::consts::FRAC_PI_2;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
// `std::time::Instant` panics on the web; this one reads the page's clock.
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

use glam::{Mat4, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};
use wxsl::core::abi;
use wxsl::core::graph::Graph;
use wxsl::core::graph::{AttributeDecl, Node, NodeId};
use wxsl::core::node::{Value, ValueType};
use wxsl::core::pipeline as doc;
use wxsl::core::scene::{Tags, TAG_OPAQUE, TAG_TRANSPARENT};
use wxsl::render::effect::refraction::Quality as RefractionQuality;
use wxsl::render::effect::{BRDF_LUT, LUT_VIEW, RAMP_FILL, RAMP_VIEW};
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::{
    compile_pipeline, Camera, DrawItem, DrawList, EffectRegistry, Environment, InstanceAttributes,
    Light, Mesh, PipelineConfig, RenderRequest, Renderer, StockPipeline, TargetConfig,
};

mod app;
mod demos;
mod documents;
mod environment;
mod glass_probes;
mod materials;
mod screenshots;
mod stage;
#[cfg(test)]
mod tests;

use app::*;
use demos::*;
use documents::*;
use environment::*;
use materials::*;
use screenshots::*;
use stage::*;

const USAGE: &str = "\
gallery — the wxsl demos in one window, or as screenshots

USAGE:
    cargo run --example gallery -- [OPTIONS]

OPTIONS:
    --screenshot [DIR]     Render every demo once, write <DIR>/<name>.png
                           plus a contact sheet, and exit (default: ./gallery)
    --size <WIDTHxHEIGHT>  Render size (default: 1280x720 windowed, 800x600
                           screenshots)
    --start <NAME>         Which demo to show first
    --ibl-source <PATH|sky> HDR environment for ibl (default: local resources/hdri HDR, otherwise sky)
    --refraction-quality <layered|precise> Solver for glass (default: layered; precise is expensive)
    --list                 List the demos and exit
    --export-pipeline PATH Export the single-pass pipeline document without a device
    -h, --help             Print this help

KEYS (windowed):
    Left / Right, [ / ]    previous / next demo
    1 .. 9                 jump to demo N
    Space                  pause the clock
    Left drag              orbit the camera
    Right / middle drag    pan the camera target
    Wheel                  zoom
    R                      reset this demo's camera
    Esc or Q               quit
";

#[cfg(target_arch = "wasm32")]
const WEB_HDR: &[u8] = include_bytes!("../../../../resources/hdri/lakeside_sunrise_1k.hdr");

/// The windowed gallery in a page (ADR 0075): `?demo=<name>` picks the
/// first demo, `glass-probes` by default. The demos need a full profile
/// (the IBL prefilters are native-only), so this wants WebGPU.
#[cfg(target_arch = "wasm32")]
fn main() {
    use winit::platform::web::EventLoopExtWebSys;
    std::panic::set_hook(Box::new(|info| say(&format!("panic: {info}"))));
    let search = web_sys::window()
        .and_then(|window| window.location().search().ok())
        .unwrap_or_default();
    let name = search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix("demo="))
        .unwrap_or("glass-probes")
        .to_string();
    let demos = demos();
    let start = demos.iter().position(|demo| demo.name == name).unwrap_or(0);
    let options = Options::parse(std::iter::empty()).expect("the defaults parse");
    let event_loop = EventLoop::new().expect("an event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.spawn_app(App::new(options, demos, start));
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn Error>> {
    let options = Options::parse(std::env::args().skip(1))?;
    if options.help {
        print!("{USAGE}");
        return Ok(());
    }
    if let Some(path) = options.export_pipeline {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&minimal_forward_document())?,
        )?;
        return Ok(());
    }
    let demos = demos();
    if options.list {
        println!("{} demos:\n", demos.len());
        for (index, demo) in demos.iter().enumerate() {
            println!(
                "  {:<18} {}",
                format!("{}) {}", index + 1, demo.name),
                demo.blurb
            );
        }
        return Ok(());
    }
    let start = options.start.as_deref().map(|name| {
        demos
            .iter()
            .position(|demo| demo.name == name)
            .unwrap_or_else(|| {
                eprintln!("error: no demo named `{name}` (see --list)");
                std::process::exit(2);
            })
    });

    match options.screenshot {
        Some(dir) => run_screenshots(
            options.size,
            &demos,
            &dir,
            &options.ibl_source,
            options.refraction_quality,
        ),
        None => run_windowed(options, demos, start.unwrap_or(0)),
    }
}

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

struct Options {
    ibl_source: String,
    refraction_quality: RefractionQuality,
    export_pipeline: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    size: Option<(u32, u32)>,
    start: Option<String>,
    list: bool,
    help: bool,
}

impl Options {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Options {
            #[cfg(target_arch = "wasm32")]
            ibl_source: "lakeside_sunrise_1k.hdr".into(),
            #[cfg(not(target_arch = "wasm32"))]
            ibl_source: {
                let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../resources/hdri/lakeside_sunrise_1k.hdr");
                if path.is_file() {
                    path.to_string_lossy().into_owned()
                } else {
                    "sky".into()
                }
            },
            export_pipeline: None,
            refraction_quality: RefractionQuality::Layered,
            screenshot: None,
            size: None,
            start: None,
            list: false,
            help: false,
        };
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("`{arg}` needs a value"));
            match arg.as_str() {
                "--ibl-source" => options.ibl_source = value()?,
                "--refraction-quality" => {
                    options.refraction_quality = value()?.parse().map_err(str::to_string)?
                }
                "--export-pipeline" => options.export_pipeline = Some(PathBuf::from(value()?)),
                "-h" | "--help" => options.help = true,
                "--list" => options.list = true,
                // `--screenshot` and `--screenshot=DIR` both work; the
                // bare flag is the one a person types.
                "--screenshot" => options.screenshot = Some(PathBuf::from("gallery")),
                other if other.starts_with("--screenshot=") => {
                    options.screenshot = Some(PathBuf::from(other["--screenshot=".len()..].trim()));
                }
                "--start" => options.start = Some(value()?),
                "--size" => {
                    let text = value()?;
                    let (width, height) = text
                        .split_once(['x', 'X'])
                        .ok_or_else(|| format!("`--size` wants WIDTHxHEIGHT, got `{text}`"))?;
                    options.size = Some((
                        width.trim().parse().map_err(|_| "bad width".to_string())?,
                        height
                            .trim()
                            .parse()
                            .map_err(|_| "bad height".to_string())?,
                    ));
                }
                other => return Err(format!("unexpected argument `{other}`")),
            }
        }
        Ok(options)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run_windowed(options: Options, demos: Vec<Demo>, start: usize) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(options, demos, start);
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
