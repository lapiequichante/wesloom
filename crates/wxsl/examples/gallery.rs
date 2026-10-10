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

use std::error::Error;
use std::f32::consts::FRAC_PI_2;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use glam::{Mat4, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};
use wxsl::core::abi;
use wxsl::core::graph::Graph;
use wxsl::core::graph::{AttributeDecl, Node, NodeId};
use wxsl::core::node::{Value, ValueType};
use wxsl::core::pipeline as doc;
use wxsl::core::scene::{Tags, TAG_OPAQUE, TAG_TRANSPARENT};
use wxsl::render::effect::{BRDF_LUT, LUT_VIEW, RAMP_FILL, RAMP_VIEW};
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::{
    compile_pipeline, Camera, DrawItem, DrawList, EffectRegistry, Environment, InstanceAttributes,
    Light, Mesh, PipelineConfig, RenderRequest, Renderer, StockPipeline, TargetConfig,
};

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
    --list                 List the demos and exit
    --export-pipeline PATH Export the single-pass pipeline document without a device
    -h, --help             Print this help

KEYS (windowed):
    Left / Right, [ / ]    previous / next demo
    1 .. 9                 jump to demo N
    Space                  pause the clock
    Esc or Q               quit
";

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
        Some(dir) => run_screenshots(options.size, &demos, &dir, &options.ibl_source),
        None => run_windowed(options, demos, start.unwrap_or(0)),
    }
}

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

struct Options {
    ibl_source: String,
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

// ---------------------------------------------------------------------------
// The demos
// ---------------------------------------------------------------------------

/// Where a demo's pass list comes from: a stock preset, or a document
/// built here and compiled by the public pipeline compiler. Every demo is
/// a document now — the compute proofs included, whose passes are
/// `pass.compute.<effect>` nodes since plan3 N3.
enum Pipeline {
    Stock(StockPipeline),
    Document(fn() -> Graph),
}

impl Pipeline {
    /// The pipeline's name, for titles and status lines.
    fn name(&self) -> String {
        match self {
            Pipeline::Stock(stock) => stock.name().to_string(),
            Pipeline::Document(build) => build().name().to_string(),
        }
    }
}

/// One entry in the gallery: a name, a pipeline, and how to light it.
struct Demo {
    name: &'static str,
    blurb: &'static str,
    pipeline: Pipeline,
    /// How many copies of the cube: one, or a row.
    instances: u32,
    /// The key light's intensity. The bloom demo needs a highlight that
    /// crosses the threshold — a scene lit normally has nothing to glow.
    key_intensity: f32,
    /// The material features the demo's pipeline enables (plan2 P12).
    features: &'static [&'static str],
    /// Which lighting model shades the shared cube, when the demo's is
    /// not the stock default. Naming a model demands the *set* carry it,
    /// so entering such a demo widens the renderer's lighting set and
    /// leaving restores the stock one — the recompile that costs is the
    /// demo switch itself.
    model: Option<&'static str>,
    /// A sky to light the demo by instead of the lights: sky above,
    /// bounce below, with the lamps switched off. What is left is
    /// `ambient_environment` alone, and therefore the environment-BRDF
    /// table it reads (ADR 0039).
    sky: Option<(Vec3, Vec3)>,
    /// The demo's effect-parameter overrides, as (pass label, parameter,
    /// value) — applied through `Renderer::set_pass_param` once the pass
    /// list is set. What an editor canvas's sliders would drive (ADR 0042).
    params: &'static [(&'static str, &'static str, f32)],
    /// A material graph of the demo's own, instead of the shared PBR cube
    /// — one demo, `bake`, needs a material that *declares a bake* (ADR
    /// 0045). `None` is the shared cube, and every other demo's image
    /// comes from it unchanged.
    material: Option<fn() -> Graph>,
    /// How the demo moves, when it moves at all: the transform at `t`,
    /// for the demos whose cube has a *velocity* — the previous-frame
    /// transform the velocity stage reads is this path one frame back.
    /// `None` is the static spin every other demo draws, and a draw with
    /// no previous is zero motion — today's behaviour (plan3 N2).
    motion: Option<Motion>,
    /// How many frames render before the captured one, so a temporal
    /// pipeline's screenshot shows a settled history rather than frame
    /// one's empty ring. The live view needs none of this: it runs
    /// continuously.
    warmup: u32,
}

/// One demo's way of moving: where the cube is at `t`, and the frame step
/// its previous-frame transform answers for in a screenshot. The step is
/// the shutter a motion blur is the average over — a screenshot has no
/// real frame times to measure, so the demo states one.
#[derive(Clone, Copy)]
struct Motion {
    step: f32,
    path: fn(f32) -> Mat4,
}

fn demos() -> Vec<Demo> {
    vec![
        Demo {
            name: "forward",
            blurb: "the stock forward pipeline: depth prepass, then shade",
            pipeline: Pipeline::Stock(StockPipeline::Forward),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "deferred",
            blurb: "the stock deferred pipeline: G-buffer, then a lighting pass",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "single-pass",
            blurb: "the minimal forward document: one pass, its own depth, no shadows",
            pipeline: Pipeline::Document(minimal_forward_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "deferred-bloom",
            blurb: "deferred lighting into a colour target, then a separable bloom — \
                    blur along x, then along y — as a document edit",
            pipeline: Pipeline::Document(deferred_bloom_document),
            instances: 1,
            // A highlight bright enough to cross bloom's threshold.
            key_intensity: 160.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "bloom-instances",
            blurb: "FXAA, then the separable bloom, six tinted copies from one draw list",
            pipeline: Pipeline::Document(bloom_instances_document),
            instances: 6,
            key_intensity: 160.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "bloom-tuned",
            blurb: "the same chain with bloom's parameters tuned live — set_pass_param \
                    moves threshold on the horizontal pass and strength on the vertical \
                    one, with no recompile",
            pipeline: Pipeline::Document(deferred_bloom_document),
            instances: 1,
            key_intensity: 160.0,
            features: &[],
            model: None,
            sky: None,
            // The threshold below the lit surface's radiance, so the whole
            // scene glows, and the strength above one, so the glow leads —
            // visibly not the default image, from the same document.
            params: &[
                ("bloom", "threshold", 0.35),
                ("bloom-y", "strength", 1.6),
            ],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "bloom-pyramid",
            blurb: "deferred lighting into a colour target, then `pass.bloom` — the \
                    whole pyramid, extract to combine, expanded from one document node \
                    (ADR 0065)",
            pipeline: Pipeline::Document(deferred_pyramid_document),
            instances: 1,
            key_intensity: 160.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "bloom-pyramid-tuned",
            blurb: "the pyramid with its parameters tuned live — the expansion's stable \
                    labels are the contract: threshold and knee on `bloom extract`, \
                    strength on `bloom combine`",
            pipeline: Pipeline::Document(deferred_pyramid_document),
            instances: 1,
            key_intensity: 160.0,
            features: &[],
            model: None,
            sky: None,
            params: &[
                ("bloom extract", "threshold", 0.35),
                ("bloom extract", "knee", 0.3),
                ("bloom combine", "strength", 1.6),
            ],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "dof",
            blurb: "`pass.dof` over a row of copies, focused on the middle one — signed \
                    circles of confusion, an occlusion-faded far blur, a dilated near \
                    composite (ADR 0065)",
            pipeline: Pipeline::Document(dof_document),
            instances: 3,
            key_intensity: 90.0,
            features: &[],
            model: None,
            sky: None,
            // The camera sits ~4.4 m from the origin; the row spreads
            // copies at roughly 3.7 and 6.0 m, so f/1.4 on an 85 mm puts
            // the outer copies visibly out of focus and the middle one
            // not at all.
            params: &[
                ("dof circles", "focus", 4.4),
                ("dof circles", "focal", 0.085),
                ("dof circles", "f_number", 1.4),
                ("dof circles", "near", 0.1),
                ("dof circles", "far", 100.0),
                ("dof near blur", "max_coc", 14.0),
            ],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "brdf-lut",
            blurb: "a compute effect bakes the split-sum BRDF LUT once (policy: once), \
                    and a per-frame view displays it — the execution-policy proof, \
                    as a document",
            pipeline: Pipeline::Document(brdf_lut_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "buffer-ramp",
            blurb: "a compute effect fills a storage buffer and a screen effect reads \
                    it as storage — buffers are graph resources, and the proof compiles \
                    from a document",
            pipeline: Pipeline::Document(buffer_ramp_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "subsurface",
            blurb: "the deferred pipeline with the subsurface feature: the G-buffer \
                    plan grows a channel the material packs (pixels unchanged until \
                    a model reads it — the seam is the point)",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &["subsurface"],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "cloth",
            blurb: "the same cube shaded by the cloth model: a roughness-wrapped                     diffuse and a Charlie sheen layer, entering the wide                     lighting set for the one demo that needs it",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            model: Some("wxsl.cloth"),
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "ibl",
            blurb: "no lamps: HDR image or single-scattering sky, diffuse convolution and GGX roughness mips with split-sum BRDF",
            pipeline: Pipeline::Stock(StockPipeline::Forward),
            instances: 1,
            key_intensity: 0.0,
            features: &[],
            model: None,
            sky: Some((Vec3::ZERO, Vec3::ZERO)),
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "ibl-mirror",
            blurb: "mirror sphere: roughness 0, metallic 1, white F0; lit only by the HDR environment",
            pipeline: Pipeline::Stock(StockPipeline::Forward),
            instances: 1,
            key_intensity: 0.0,
            features: &[],
            model: None,
            sky: Some((Vec3::ZERO, Vec3::ZERO)),
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "ibl-grid",
            blurb: "5x5 stationary spheres: roughness 0→1 left to right, metallic 0→1 front to back; matte grey floor, HDR image IBL only",
            pipeline: Pipeline::Stock(StockPipeline::Forward),
            instances: 25,
            key_intensity: 0.0,
            features: &[],
            model: None,
            sky: Some((Vec3::ZERO, Vec3::ZERO)),
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "ibl-grid-sky",
            blurb: "the same 5x5 roughness/metallic grid and matte grey floor, lit only by the Rayleigh/Mie sky",
            pipeline: Pipeline::Stock(StockPipeline::Forward),
            instances: 25,
            key_intensity: 0.0,
            features: &[],
            model: None,
            sky: Some((Vec3::ZERO, Vec3::ZERO)),
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "sky",
            blurb: "equirectangular single-scattering sky: Rayleigh/Mie, optical depth and planet shadow; also available as the IBL source",
            pipeline: Pipeline::Document(sky_document),
            instances: 1,
            key_intensity: 0.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "bake",
            blurb: "a twelve-octave noise term baked to a 256x256 table once (policy: \
                    once) and sampled back — ADR 0045's toggle: baked and inline \
                    draw the same picture, and only the cost moves",
            pipeline: Pipeline::Document(bake_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: Some(bake_material_graph),
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "fxaa",
            blurb: "the stock forward chain with an anti-aliasing pass appended — and                     both screen effects in it are node graphs, not shader files",
            pipeline: Pipeline::Document(fxaa_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "taa",
            blurb: "the spinning cube under TAA: a velocity pass, a history \
                    ring, and a resolve whose disocclusion answer is a clamp — \
                    the plan3 N2 chain as a document",
            pipeline: Pipeline::Document(taa_document),
            instances: 1,
            key_intensity: 22.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            // The cube spins, so its velocity is real; the history needs
            // frames behind it before the captured one shows what the
            // ring accumulated.
            motion: Some(Motion {
                step: 1.0 / 60.0,
                path: cube_transform,
            }),
            warmup: 24,
        },
        Demo {
            name: "motion-blur",
            blurb: "a cube swept across the frame, smeared along its own \
                    velocity — the first consumer of the velocity buffer that \
                    is not TAA",
            pipeline: Pipeline::Document(motion_blur_document),
            instances: 1,
            key_intensity: 22.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: Some(Motion {
                step: 1.0 / 60.0,
                path: swept_transform,
            }),
            warmup: 0,
        },
        Demo {
            name: "peel",
            blurb: "a torus and a blue sphere through each other, the PBR cube shader \
                    with its alpha uniform at 0.3, composited by dual depth peeling",
            pipeline: Pipeline::Document(peel_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "render-order",
            blurb: "three cards in one blended pass — an order -1 underlay, a sheet, \
                    and an order-1 hologram *behind it in depth* yet drawn after it: \
                    the group order is the draws' data (plan5 D4)",
            pipeline: Pipeline::Document(order_document),
            instances: 1,
            // The cards face away from the key light's angle; the point
            // is the composite, so light the sheet up.
            key_intensity: 90.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "sdf-aa",
            blurb: "an AA'd checker and an SDF disc whose edge is one screen-space \
                    pixel wide — checker_aa and sdf_coverage fed by screen_width, \
                    the fragment-only family (plan5 D6)",
            pipeline: Pipeline::Document(alpha_over_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            model: None,
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "iridescence",
            blurb: "a thin-film sphere: graph-authored thickness carried by an HDR \
                    model channel into the spectral specular light loop (ADR 0058)",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            model: Some("wxsl.iridescent"),
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
        Demo {
            name: "sheen",
            blurb: "a PBR sphere under graph-authored sheen: tint and roughness \
                    carried into the Charlie light loop and analytic ambient (ADR 0059)",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            model: Some("wxsl.sheen"),
            sky: None,
            params: &[],
            material: None,
            motion: None,
            warmup: 0,
        },
    ]
}

/// Optical sky preview, replacing the scene image before the display transform.
/// The rendered scene supplies an extent only, not background compositing or IBL.
fn sky_document() -> Graph {
    use wxsl::core::graph::{Node, SocketRef};
    let registry = doc::registry();
    let mut document = StockPipeline::Forward.document();
    document.set_name("single-scattering sky preview");
    let tonemap = document
        .nodes()
        .find(|(_, node)| {
            node.settings
                .get(doc::SETTING_EFFECT)
                .is_some_and(|id| id == "wxsl.tonemap")
        })
        .map(|(id, _)| id)
        .expect("stock tonemap");
    let input = SocketRef::new(tonemap, "image");
    let source = document
        .edge_into(&input)
        .expect("stock HDR image")
        .from
        .clone();
    document.disconnect(&registry, &input);
    let image = document.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("sky radiance")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let sky = document.add(
        Node::new(doc::PASS_SCREEN)
            .with_label("sky")
            .with_setting(doc::SETTING_EFFECT, "wxsl.sky"),
    );
    document
        .wire(
            &registry,
            (source.node, source.socket.as_str()),
            (sky, "image"),
        )
        .expect("extent input");
    document
        .wire(&registry, (image, "color"), (sky, "into"))
        .expect("linear radiance");
    document
        .wire(&registry, (image, "color"), (tonemap, "image"))
        .expect("display transform last");
    document
}

/// The stock forward document with one node appended: a `pass.screen`
/// running `fxaa`, reading what the tonemap wrote
/// ([ADR 0040](../../../docs/adr/0040-screen-domain-graphs-postprocess-is-a-material-over-the-frame.md)).
///
/// After the display transform on purpose: perceived edges are what alias,
/// and by that point in the chain the numbers are perceptual. The two
/// effects this document names are both *graphs* — see where the gallery
/// registers them — so what runs here is two generated modules, composed as
/// a document edit, with nothing in `wxsl-render` touched to allow either.
fn fxaa_document() -> Graph {
    use wxsl::core::graph::SocketRef;

    let registry = wxsl::core::pipeline::registry();
    let mut document = StockPipeline::Forward.document();
    document.set_name("forward + fxaa");
    let tonemap = document
        .nodes()
        .find(|(_, node)| {
            node.settings
                .get(doc::SETTING_EFFECT)
                .is_some_and(|spelled| {
                    wxsl::core::identity::resolve(spelled).as_ref() == "wxsl.tonemap"
                })
        })
        .map(|(id, _)| id)
        .expect("every stock document ends in the tonemap pass");
    let present = document
        .edge_from(&SocketRef::new(tonemap, "color"))
        .expect("the tonemap presents")
        .to
        .node;

    let encoded = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("encoded")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let fxaa = document.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("fxaa")
            .with_setting(doc::SETTING_EFFECT, "wxsl.fxaa"),
    );
    document.disconnect(&registry, &SocketRef::new(present, "surface"));
    for (from, to) in [
        ((encoded, "color"), (tonemap, "into")),
        ((encoded, "color"), (fxaa, "image")),
        ((fxaa, "color"), (present, "surface")),
    ] {
        document.wire(&registry, from, to).expect("gallery wiring");
    }
    document
}

/// The TAA chain as a document: the shipped deferred preset with a
/// velocity pass and a resolve composed onto it — three nodes, one
/// resource, one rewire, in a demo, because that is what a temporal
/// pipeline *is* now ([plan3 N2]).
///
/// The velocity pass loads the depth the G-buffer pass wrote — the same
/// `depth` output every material pass carries — and writes a `pair`
/// precision colour: motion is a difference, signed, and worth half
/// floats. The resolve is a derived `pass.screen.wxsl.taa` row, whose
/// sockets are its declaration's — `color`, `velocity`, and `history`,
/// the last read a frame back through the ring the resolve itself
/// writes, which is the whole reason the pass orders against the scene
/// and never against itself. The tonemap reads the ring.
fn taa_document() -> Graph {
    use wxsl::core::graph::SocketRef;

    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut document = StockPipeline::Deferred.document();
    document.set_name("deferred + taa");

    // The preset's nodes, by what they are.
    let scene = document
        .nodes()
        .find(|(_, node)| node.def == doc::SOURCE_SCENE)
        .map(|(id, _)| id)
        .expect("the preset draws a scene");
    let material = document
        .nodes()
        .find(|(_, node)| {
            node.def == doc::PASS_GEOMETRY
                && node
                    .settings
                    .get(doc::SETTING_STAGE)
                    .is_some_and(|stage| stage.as_str() == "gbuffer")
        })
        .map(|(id, _)| id)
        .expect("the deferred preset shades a G-buffer");
    let scene_color = document
        .nodes()
        .find(|(_, node)| node.def == doc::RESOURCE_COLOR)
        .map(|(id, _)| id)
        .expect("the deferred preset shades into a colour target");
    let tonemap = document
        .nodes()
        .find(|(_, node)| {
            node.settings
                .get(doc::SETTING_EFFECT)
                .is_some_and(|spelled| {
                    wxsl::core::identity::resolve(spelled).as_ref() == "wxsl.tonemap"
                })
        })
        .map(|(id, _)| id)
        .expect("every stock document ends in the tonemap pass");

    let velocity = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("velocity")
            .with_setting(doc::SETTING_PRECISION, "pair"),
    );
    let motion = document.add(
        wxsl::core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("velocity")
            .with_setting(doc::SETTING_STAGE, "velocity"),
    );
    let ring = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("taa")
            .with_setting(doc::SETTING_PRECISION, "hdr")
            .with_setting(doc::SETTING_HISTORY, "1"),
    );
    let taa = document.add(
        wxsl::core::graph::Node::new(format!("{}wxsl.taa", doc::PASS_SCREEN_PREFIX))
            .with_label("taa"),
    );

    document.disconnect(&registry, &SocketRef::new(tonemap, "image"));
    for (from, to) in [
        ((scene, "draws"), (motion, "draws")),
        ((material, "depth"), (motion, "depth")),
        ((velocity, "color"), (motion, "into")),
        ((scene_color, "color"), (taa, "color")),
        ((velocity, "color"), (taa, "velocity")),
        ((ring, "color"), (taa, "history")),
        ((ring, "color"), (taa, "into")),
        ((ring, "color"), (tonemap, "image")),
    ] {
        document.wire(&registry, from, to).expect("taa wiring");
    }
    document
}

/// The motion-blur chain as a document: the shipped forward preset with a
/// velocity pass and the blur composed between the shading and the
/// display transform. The blur reads *linear* radiance — ADR 0039's rule
/// is the reason it sits before the tonemap — and smears each fragment
/// along its own screen motion, which is what makes the smear follow the
/// object rather than a global direction.
fn motion_blur_document() -> Graph {
    use wxsl::core::graph::SocketRef;

    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut document = StockPipeline::Forward.document();
    document.set_name("forward + motion blur");

    let scene = document
        .nodes()
        .find(|(_, node)| node.def == doc::SOURCE_SCENE)
        .map(|(id, _)| id)
        .expect("the preset draws a scene");
    let prepass = document
        .nodes()
        .find(|(_, node)| {
            node.def == doc::PASS_GEOMETRY
                && node
                    .settings
                    .get(doc::SETTING_STAGE)
                    .is_some_and(|stage| stage.as_str() == "depth_only")
        })
        .map(|(id, _)| id)
        .expect("the forward preset starts with a depth prepass");
    let scene_color = document
        .nodes()
        .find(|(_, node)| node.def == doc::RESOURCE_COLOR)
        .map(|(id, _)| id)
        .expect("the forward preset shades into a colour target");
    let tonemap = document
        .nodes()
        .find(|(_, node)| {
            node.settings
                .get(doc::SETTING_EFFECT)
                .is_some_and(|spelled| {
                    wxsl::core::identity::resolve(spelled).as_ref() == "wxsl.tonemap"
                })
        })
        .map(|(id, _)| id)
        .expect("every stock document ends in the tonemap pass");

    let velocity = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("velocity")
            .with_setting(doc::SETTING_PRECISION, "pair"),
    );
    let motion = document.add(
        wxsl::core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("velocity")
            .with_setting(doc::SETTING_STAGE, "velocity"),
    );
    let blurred = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("blurred")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let blur = document.add(
        wxsl::core::graph::Node::new(format!("{}wxsl.motion_blur", doc::PASS_SCREEN_PREFIX))
            .with_label("motion blur"),
    );

    document.disconnect(&registry, &SocketRef::new(tonemap, "image"));
    for (from, to) in [
        ((scene, "draws"), (motion, "draws")),
        ((prepass, "depth"), (motion, "depth")),
        ((velocity, "color"), (motion, "into")),
        ((scene_color, "color"), (blur, "color")),
        ((velocity, "color"), (blur, "velocity")),
        ((blurred, "color"), (blur, "into")),
        ((blurred, "color"), (tonemap, "image")),
    ] {
        document.wire(&registry, from, to).expect("blur wiring");
    }
    document
}

/// The execution-policy proof as a document (ADR 0035): a compute effect
/// bakes the split-sum environment-BRDF LUT once into a stable target, and
/// a screen effect displays that target every frame. The bake is a
/// `pass.compute.brdf_lut` node — its `lut` socket is the declaration's,
/// the target a 64x64 fixed `resource.color`, the policy a setting.
fn brdf_lut_document() -> Graph {
    let effects = EffectRegistry::default().with(BRDF_LUT).with(LUT_VIEW);
    let registry = wxsl::render::document_registry(&effects);
    let mut document = wxsl_core::pipeline::document("brdf lut");
    let lut = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("brdf lut")
            .with_setting(doc::SETTING_PRECISION, "hdr")
            .with_setting(doc::SETTING_SIZE, "64x64"),
    );
    let bake = document.add(
        wxsl::core::graph::Node::new(format!("{}wxsl.brdf_lut", doc::PASS_COMPUTE_PREFIX))
            .with_label("lut bake")
            .with_setting(doc::SETTING_POLICY, "once"),
    );
    let view = document.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("lut view")
            .with_setting(doc::SETTING_EFFECT, "wxsl.lut_view"),
    );
    let present = document.add_node(doc::PRESENT);
    for (from, to) in [
        ((lut, "color"), (bake, "lut")),
        ((lut, "color"), (view, "image")),
        ((view, "color"), (present, "surface")),
    ] {
        document.wire(&registry, from, to).expect("lut wiring");
    }
    document
}

/// The buffer proof as a document (ADR 0036): a compute effect fills a
/// 256-entry storage buffer, a screen effect reads it as storage and draws
/// it, one value per column. The buffer is a `resource.buffer` both passes
/// wire from — the write declared on the compute node's `ramp` socket, the
/// read on the screen pass's `buffer` socket.
fn buffer_ramp_document() -> Graph {
    let effects = EffectRegistry::default().with(RAMP_FILL).with(RAMP_VIEW);
    let registry = wxsl::render::document_registry(&effects);
    let mut document = wxsl_core::pipeline::document("buffer ramp");
    let ramp = document.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_BUFFER)
            .with_label("ramp")
            .with_setting(doc::SETTING_BYTES, "1024"),
    );
    let fill = document.add(
        wxsl::core::graph::Node::new(format!("{}wxsl.ramp_fill", doc::PASS_COMPUTE_PREFIX))
            .with_label("fill ramp"),
    );
    let view = document.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("show ramp")
            .with_setting(doc::SETTING_EFFECT, "wxsl.ramp_view"),
    );
    let present = document.add_node(doc::PRESENT);
    for (from, to) in [
        ((ramp, "buffer"), (fill, "ramp")),
        ((ramp, "buffer"), (view, "buffer")),
        ((view, "color"), (present, "surface")),
    ] {
        document.wire(&registry, from, to).expect("ramp wiring");
    }
    document
}

/// The bake demo's material: the graph is a document, `bake_term.wxsl.json`
/// — the same move `pbr_cube` makes, plus the `bakes` declaration that says
/// node 5's value becomes a table ([ADR 0045](../../../docs/adr/0045-a-bake-is-an-effect-over-a-material-subgraph.md)).
/// `Stage` compiles the *same parsed graph* into the effect's shader and
/// the material, which is the point: one authoring, two compilations.
fn bake_material_graph() -> Graph {
    let graph: Graph = serde_json::from_str(include_str!("../assets/bake_term.wxsl.json"))
        .expect("the bake material document parses");
    graph
        .validate(&wxsl::stdlib::registry())
        .expect("the bake material document validates");
    graph
}

/// The bake demo's pipeline: the deferred chain, with the bake — an
/// imported colour target labelled with the declaration's texture name and
/// a `pass.compute.<effect>` node writing it under policy `once` — added
/// *before* the material pass. That placement is not style: the material
/// samples the table through its own bind group, which is a dependency the
/// scheduler cannot see, and declaration order is its tie-break. A bake
/// written after the pass that samples it would feed it an empty table for
/// one frame ([ADR 0045](../../../docs/adr/0045-a-bake-is-an-effect-over-a-material-subgraph.md)).
/// The effect itself is `Stage::new`'s, generated from the material
/// subgraph; a document names it by id like any other.
fn bake_document() -> Graph {
    let bake = bake_effect();
    let effects = EffectRegistry::default().with(bake);
    let registry = wxsl::render::document_registry(&effects);
    let mut graph = wxsl_core::pipeline::document("deferred + bake");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let lights = graph.add_node(doc::SOURCE_LIGHTS);
    let shadows = graph.add_node(doc::PASS_SHADOW);
    let table = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("roughness_bake")
            .with_setting(doc::SETTING_PRECISION, "hdr")
            .with_setting(doc::SETTING_IMPORTED, "true"),
    );
    let bake_pass = graph.add(
        wxsl::core::graph::Node::new(format!("{}demo.bake_roughness", doc::PASS_COMPUTE_PREFIX))
            .with_label("roughness bake")
            .with_setting(doc::SETTING_POLICY, "once"),
    );
    let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
    let material = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("deferred material")
            .with_setting(doc::SETTING_STAGE, "gbuffer"),
    );
    let lighting =
        graph.add(wxsl::core::graph::Node::new(doc::PASS_SCREEN).with_label("deferred lighting"));
    let present = graph.add_node(doc::PRESENT);
    let mut wire = |from: (NodeId, &str), to: (NodeId, &str)| {
        graph.wire(&registry, from, to).expect("bake demo wiring");
    };
    wire((scene, "draws"), (shadows, "draws"));
    wire((lights, "shadows"), (shadows, "into"));
    wire((table, "color"), (bake_pass, "bake"));
    wire((scene, "draws"), (material, "draws"));
    wire((gbuffer, "gbuffer"), (material, "gbuffer"));
    wire((gbuffer, "gbuffer"), (lighting, "gbuffer"));
    wire((lighting, "color"), (present, "surface"));
    // The head pass is redirected into an HDR target and presents through
    // the display transform, exactly as every stock chain ends.
    present_through_tonemap(&mut graph, lighting, present);
    graph
}

/// The bake effect, generated from the material subgraph — what
/// `Stage::new` registers on the renderer and `bake_document` names. One
/// generation here, one in `Stage::new`: both cheap, both from the same
/// declaration, and the variant cache keys on the source's hash so there
/// is no question of them disagreeing.
fn bake_effect() -> wxsl::render::effect::Effect {
    wxsl::render::effect::Effect::from_bake(
        "demo.bake_roughness",
        "roughness bake",
        "Evaluate the material's baked term over its 256x256 table.",
        &bake_material_graph(),
        "roughness_bake",
        &wxsl::core::macros::MacroSet::new(),
        &wxsl::stdlib::registry(),
    )
    .expect("the bake material generates its effect")
}

/// The minimal pipeline there is: a scene, one lit material pass with a
/// depth target of its own, present. Three nodes and a resource — what a
/// pipeline document looks like with everything optional left out.
fn minimal_forward_document() -> Graph {
    let registry = wxsl::core::pipeline::registry();
    let mut graph = wxsl::core::pipeline::document("single pass");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let pass = graph.add_node(doc::PASS_GEOMETRY);
    let present = graph.add_node(doc::PRESENT);
    let mut wire = |from: (NodeId, &str), to: (NodeId, &str)| {
        graph.wire(&registry, from, to).expect("gallery wiring");
    };
    wire((scene, "draws"), (pass, "draws"));
    wire((depth, "depth"), (pass, "depth"));
    wire((pass, "color"), (present, "surface"));
    // `wire` borrows the graph, so its last use has to come before the
    // helper's own wiring.
    present_through_tonemap(&mut graph, pass, present);
    graph
}

/// The `tonemap` effect on the end of `graph`'s chain: the display
/// transform every pipeline that presents to a screen needs
/// ([ADR 0039](../../../docs/adr/0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md)).
///
/// `head` is the pass currently writing the frame's target. It is given an
/// HDR colour target to write instead, and the tonemap reads that and
/// presents. Three nodes and a rewire, in a demo, because that is exactly
/// what the stock presets do — a chain is a chain wherever it is built.
fn present_through_tonemap(graph: &mut Graph, head: NodeId, present: NodeId) {
    // The document registry, not the static one: a head whose effect has
    // more than one image — the separable bloom's vertical pass — is a
    // derived row, and its `into` socket is not on the fixed pass.
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let hdr = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("linear")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let tonemap = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("tonemap")
            .with_setting(doc::SETTING_EFFECT, "wxsl.tonemap"),
    );
    graph.disconnect(
        &registry,
        &wxsl::core::graph::SocketRef::new(present, "surface"),
    );
    for (from, to) in [
        ((hdr, "color"), (head, "into")),
        ((hdr, "color"), (tonemap, "image")),
        ((tonemap, "color"), (present, "surface")),
    ] {
        graph.wire(&registry, from, to).expect("gallery wiring");
    }
}

/// The shipped deferred preset with a separable bloom composed onto it.
/// Lighting writes an HDR target; [`separable_bloom`] blurs the bright
/// part along x and then along y and adds it back. The one-pass bloom's
/// taps sit four texels apart, which bands, so these demos do not use it.
fn deferred_bloom_document() -> Graph {
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut graph = wxsl::core::pipeline::document("deferred bloom");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let lights = graph.add_node(doc::SOURCE_LIGHTS);
    let shadows = graph.add_node(doc::PASS_SHADOW);
    let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
    let material = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("deferred material")
            .with_setting(doc::SETTING_STAGE, "gbuffer"),
    );
    // HDR on purpose: bloom thresholds *linear radiance*, and the
    // highlight it is for is brighter than white — an 8-bit intermediate
    // would clamp it to exactly the threshold and the glow would be a
    // rumour. The same honesty the `linear` target before the tonemap has.
    let scene_color = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let lighting =
        graph.add(wxsl::core::graph::Node::new(doc::PASS_SCREEN).with_label("deferred lighting"));
    let present = graph.add_node(doc::PRESENT);
    let mut wire = |from: (NodeId, &str), to: (NodeId, &str)| {
        graph.wire(&registry, from, to).expect("gallery wiring");
    };
    wire((scene, "draws"), (shadows, "draws"));
    wire((lights, "shadows"), (shadows, "into"));
    wire((scene, "draws"), (material, "draws"));
    wire((gbuffer, "gbuffer"), (material, "gbuffer"));
    wire((gbuffer, "gbuffer"), (lighting, "gbuffer"));
    wire((scene_color, "color"), (lighting, "into"));
    let bloom = separable_bloom(&mut graph, scene_color);
    graph
        .wire(&registry, (bloom, "color"), (present, "surface"))
        .expect("gallery wiring");
    // Bloom thresholds *linear* radiance, so it belongs before the display
    // transform — which is the physically correct order, and the one the
    // move in ADR 0039 made expressible.
    present_through_tonemap(&mut graph, bloom, present);
    graph
}

/// Horizontal Gaussian of the bright part of `source`, then the same
/// Gaussian along y, added back onto `source`. Returns the vertical pass,
/// which is the head of the chain.
///
/// `threshold` and `knee` are on the pass labelled `bloom`. `strength` is
/// on the pass labelled `bloom-y`. The glow resource is HDR: the bright
/// signal is linear radiance, and an 8-bit intermediate would clamp it.
fn separable_bloom(graph: &mut Graph, source: NodeId) -> NodeId {
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let glow = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("glow")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let bloom_x = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("bloom")
            .with_setting(doc::SETTING_EFFECT, "wxsl.bloom_x"),
    );
    let bloom_y = graph.add(
        wxsl::core::graph::Node::new(format!("{}wxsl.bloom_y", doc::PASS_SCREEN_PREFIX))
            .with_label("bloom-y"),
    );
    for (from, to) in [
        ((source, "color"), (bloom_x, "image")),
        ((glow, "color"), (bloom_x, "into")),
        ((source, "color"), (bloom_y, "color")),
        ((glow, "color"), (bloom_y, "glow")),
    ] {
        graph.wire(&registry, from, to).expect("gallery wiring");
    }
    bloom_y
}

/// The shipped deferred preset with a bloom pyramid composed onto it —
/// [`doc::PASS_BLOOM`], the whole pyramid in one document node (ADR 0065)
/// where [`deferred_bloom_document`] wires the separable pair by hand.
/// Lighting writes an HDR target; the node's expansion thresholds it into
/// a half-resolution first level, boxes it down three more, folds the
/// gathered energy back up, and the combine adds it onto the image. The
/// generated passes tune like any other: `threshold` on `bloom extract`,
/// `strength` on `bloom combine`.
fn deferred_pyramid_document() -> Graph {
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut graph = wxsl::core::pipeline::document("deferred bloom pyramid");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let lights = graph.add_node(doc::SOURCE_LIGHTS);
    let shadows = graph.add_node(doc::PASS_SHADOW);
    let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
    let material = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("deferred material")
            .with_setting(doc::SETTING_STAGE, "gbuffer"),
    );
    // HDR on purpose, as in [`deferred_bloom_document`]: the pyramid's
    // threshold reads linear radiance, and an 8-bit intermediate would
    // clamp the highlight the glow is for.
    let scene_color = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let lighting =
        graph.add(wxsl::core::graph::Node::new(doc::PASS_SCREEN).with_label("deferred lighting"));
    let pyramid = graph.add(wxsl::core::graph::Node::new(doc::PASS_BLOOM).with_label("bloom"));
    let present = graph.add_node(doc::PRESENT);
    let mut wire = |from: (NodeId, &str), to: (NodeId, &str)| {
        graph.wire(&registry, from, to).expect("gallery wiring");
    };
    wire((scene, "draws"), (shadows, "draws"));
    wire((lights, "shadows"), (shadows, "into"));
    wire((scene, "draws"), (material, "draws"));
    wire((gbuffer, "gbuffer"), (material, "gbuffer"));
    wire((gbuffer, "gbuffer"), (lighting, "gbuffer"));
    wire((scene_color, "color"), (lighting, "into"));
    wire((scene_color, "color"), (pyramid, "image"));
    present_through_tonemap(&mut graph, pyramid, present);
    graph
}

/// The shipped forward preset with [`doc::PASS_DOF`] composed onto it —
/// depth of field, focused on the middle copy of the row (ADR 0065's
/// second expansion). The prepass's depth is what the circles of
/// confusion measure; the chain writes an HDR target the tonemap reads.
/// The lens' numbers ride the node's settings; the expansion's stable
/// labels — `dof circles`, `dof far blur`, `dof near blur` — are what
/// `set_pass_param` would tune through.
fn dof_document() -> Graph {
    use wxsl::core::graph::SocketRef;
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut graph = StockPipeline::Forward.document();
    let scene = graph
        .nodes()
        .find(|(_, node)| node.def == doc::RESOURCE_COLOR)
        .map(|(id, _)| id)
        .expect("forward has a colour target");
    let depth = graph
        .nodes()
        .find(|(_, node)| node.def == doc::RESOURCE_DEPTH)
        .map(|(id, _)| id)
        .expect("forward has a depth target");
    let tonemap = graph
        .nodes()
        .find(|(_, node)| node.def == doc::PASS_SCREEN)
        .map(|(id, _)| id)
        .expect("forward ends in tonemap");
    let dof = graph.add(wxsl::core::graph::Node::new(doc::PASS_DOF).with_label("dof"));
    let post = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("post dof")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    graph
        .disconnect(&registry, &SocketRef::new(tonemap, "image"))
        .expect("tonemap was fed");
    for (from, to) in [
        ((scene, "color"), (dof, "image")),
        ((depth, "depth"), (dof, "depth")),
        ((post, "color"), (dof, "into")),
        ((post, "color"), (tonemap, "image")),
    ] {
        graph.wire(&registry, from, to).expect("dof wiring");
    }
    graph
}

/// The deferred bloom pipeline with FXAA inserted before the separable
/// bloom: deferred lighting writes into an HDR target, FXAA anti-aliases
/// luminance edges in linear radiance, then the horizontal and vertical
/// bloom passes run, and the result is presented through tonemap.
fn bloom_instances_document() -> Graph {
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut graph = wxsl::core::pipeline::document("deferred bloom instances with fxaa");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let lights = graph.add_node(doc::SOURCE_LIGHTS);
    let shadows = graph.add_node(doc::PASS_SHADOW);
    let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
    let material = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("deferred material")
            .with_setting(doc::SETTING_STAGE, "gbuffer"),
    );
    let scene_color = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let lighting =
        graph.add(wxsl::core::graph::Node::new(doc::PASS_SCREEN).with_label("deferred lighting"));
    let antialiased = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("antialiased")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let fxaa = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("fxaa")
            .with_setting(doc::SETTING_EFFECT, "wxsl.fxaa"),
    );
    let present = graph.add_node(doc::PRESENT);
    let mut wire = |from: (NodeId, &str), to: (NodeId, &str)| {
        graph.wire(&registry, from, to).expect("gallery wiring");
    };
    wire((scene, "draws"), (shadows, "draws"));
    wire((lights, "shadows"), (shadows, "into"));
    wire((scene, "draws"), (material, "draws"));
    wire((gbuffer, "gbuffer"), (material, "gbuffer"));
    wire((gbuffer, "gbuffer"), (lighting, "gbuffer"));
    wire((scene_color, "color"), (lighting, "into"));
    wire((scene_color, "color"), (fxaa, "image"));
    wire((antialiased, "color"), (fxaa, "into"));
    let bloom = separable_bloom(&mut graph, antialiased);
    graph
        .wire(&registry, (bloom, "color"), (present, "surface"))
        .expect("gallery wiring");
    present_through_tonemap(&mut graph, bloom, present);
    graph
}

/// Put the renderer on a demo's pass list.
///
/// Stock pipelines go through `set_pipeline`; documents through the
/// public compiler and `set_graph`; hand-built pass lists through
/// `set_graph` directly — the same moves an application makes, and why a
/// gallery demo is not a renderer feature. Features (plan2 P12) are
/// applied first, because they reshape the G-buffer every pipeline
/// variant builds against.
fn apply(demo: &Demo, renderer: &mut Renderer) -> Result<(), Box<dyn Error>> {
    if renderer.features() != demo.features {
        renderer.set_features(demo.features)?;
    }
    // Layer models fill the portable budget alone; other demos share the
    // historical wide set. Compare sets before rebuilding the renderer.
    let set = match demo.model {
        Some(name @ ("wxsl.iridescent" | "wxsl.sheen")) => {
            wxsl::core::lighting::LightingSet::single(
                *wxsl::core::lighting::DEFAULT_MODELS
                    .iter()
                    .find(|model| model.name == name)
                    .expect("shipped layer model"),
            )
        }
        Some(_) => wxsl::core::lighting::default_set()?,
        None => wxsl::core::lighting::default_single_set(),
    };
    if renderer.lighting() != &set {
        renderer.set_lighting(set)?;
    }
    // The bake table's view rides only with its demo: the next pass list
    // declares no such resource, and a view nothing declares is an error
    // by name — so the demo that owns it takes it back when it leaves.
    if demo.material.is_none() {
        renderer.remove_import("roughness_bake");
    }
    match &demo.pipeline {
        Pipeline::Stock(stock) => {
            renderer.set_pipeline(*stock);
        }
        Pipeline::Document(build) => {
            let document = build();
            let graph = compile_pipeline(
                &document,
                // The registry documents validate against: the shipped
                // vocabulary plus a `pass.compute.<effect>` node per
                // registered compute effect (plan3 N3). A superset of the
                // static registry, so every document that compiled before
                // still does.
                &wxsl::render::document_registry(renderer.effects()),
                renderer.effects(),
                &PipelineConfig::new(renderer.target()),
            )
            .map_err(|error| {
                format!(
                    "the `{}` document does not compile: {error}",
                    document.name()
                )
            })?;
            renderer
                .set_graph(graph)
                .map_err(|error| format!("the `{}` pass list does not run: {error}", demo.name))?;
        }
    }
    // The demo's parameter overrides, through the same call an editor
    // canvas's sliders would drive — the GPU sees them at the next frame,
    // and nothing recompiles (ADR 0042).
    for (pass, name, value) in demo.params {
        renderer
            .set_pass_param(pass, name, Value::F32(*value))
            .map_err(|error| {
                format!(
                    "the `{}` demo could not tune `{name}` on `{pass}`: {error}",
                    demo.name
                )
            })?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
/// Opaque colour and depth, then dual depth peeling of the peeled tier,
/// then the sorted tier over the composite, then the same tonemap every
/// other demo presents through.
///
/// The layer count is the default of `wxsl_peel_layers`, four: a ray
/// through the torus and the sphere meets four surfaces, and four is also
/// what the baseline path can peel inside the eight-pass budget. The
/// plate behind the pair asks for no layers at all (plan5 D5), so no peel
/// pass reads it: a sorted-tier pass composites it over the peel
/// composite after them, alpha-over, and the tonemap reads what it left.
fn peel_document() -> Graph {
    let registry = wxsl::core::pipeline::registry();
    let mut graph = wxsl::core::pipeline::document("dual depth peel");
    // The opaque pass draws this source. The peel ignores it and draws
    // `transparent` itself, so the two passes never draw the same instance.
    let scene = graph.add(Node::new(doc::SOURCE_SCENE).with_setting(doc::SETTING_TAGS, TAG_OPAQUE));
    let transparents =
        graph.add(Node::new(doc::SOURCE_SCENE).with_setting(doc::SETTING_TAGS, TAG_TRANSPARENT));
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let color = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("scene")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let shade = graph.add(Node::new(doc::PASS_GEOMETRY).with_label("opaque"));
    let peel = graph.add(Node::new(doc::PASS_PEEL).with_label("peel"));
    let peeled = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("peeled")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    // The sorted tier: the plate, and only the plate (`layers = sorted`
    // is the tier no peel reads). It loads the peel composite rather than
    // clearing it, and its declaration after the peel is what puts it in
    // front of what the pair left.
    let sorted = graph.add(
        Node::new(doc::PASS_GEOMETRY)
            .with_label("sorted transparents")
            .with_setting(doc::SETTING_LAYERS, "sorted")
            .with_setting(doc::SETTING_SORT, "back to front")
            .with_setting(doc::SETTING_BLEND, "alpha over"),
    );
    let tonemap = graph.add(
        Node::new(doc::PASS_SCREEN)
            .with_label("tonemap")
            .with_setting(doc::SETTING_EFFECT, "wxsl.tonemap"),
    );
    let present = graph.add_node(doc::PRESENT);
    for (from, to) in [
        ((scene, "draws"), (shade, "draws")),
        ((depth, "depth"), (shade, "depth")),
        ((color, "color"), (shade, "into")),
        ((shade, "depth"), (peel, "depth")),
        ((shade, "color"), (peel, "scene")),
        ((peeled, "color"), (peel, "into")),
        ((shade, "depth"), (sorted, "depth")),
        ((transparents, "draws"), (sorted, "draws")),
        ((peeled, "color"), (sorted, "into")),
        ((peeled, "color"), (tonemap, "image")),
        ((tonemap, "color"), (present, "surface")),
    ] {
        graph
            .wire(&registry, from, to)
            .expect("the peel document wires");
    }
    graph
}

/// One blended pass over the frame (plan5 D3/D4): everything tagged
/// `transparent`, straight-alpha over, and — with `sorted` — the pass's
/// own back-to-front sort, the order the draws' `render_order` groups
/// ride. The pass writes the frame target as its first writer, so the
/// clear colour is the background the composite sits on.
fn blended_document(name: &str, sorted: bool) -> Graph {
    let registry = wxsl::core::pipeline::registry();
    let mut graph = wxsl::core::pipeline::document(name);
    let scene =
        graph.add(Node::new(doc::SOURCE_SCENE).with_setting(doc::SETTING_TAGS, TAG_TRANSPARENT));
    let depth = graph.add_node(doc::RESOURCE_DEPTH);
    let mut pass = Node::new(doc::PASS_GEOMETRY)
        .with_label("blended")
        .with_setting(doc::SETTING_BLEND, "alpha over");
    if sorted {
        pass = pass.with_setting(doc::SETTING_SORT, "back to front");
    }
    let blended = graph.add(pass);
    let present = graph.add_node(doc::PRESENT);
    for (from, to) in [
        ((scene, "draws"), (blended, "draws")),
        ((depth, "depth"), (blended, "depth")),
        ((blended, "color"), (present, "surface")),
    ] {
        graph
            .wire(&registry, from, to)
            .expect("the blended document wires");
    }
    graph
}

fn order_document() -> Graph {
    blended_document("render order", true)
}

fn alpha_over_document() -> Graph {
    blended_document("alpha over", false)
}

// The scene — one PBR cube, the pbr_cube demo's, lit per demo
// ---------------------------------------------------------------------------

fn demo_environment(demo: &Demo, aspect: f32, time: f32) -> Environment {
    // A sky demo turns the lamps off: what is left in the image is the
    // ambient term and nothing else, so the roughness and grazing-angle
    // response it gets out of the baked table is the whole picture.
    let lights = match demo.sky {
        Some(_) => Vec::new(),
        None => vec![
            // Key: warm and close. The bloom demos turn this up far enough
            // that the specular highlight crosses the effect's threshold.
            Light::point(
                Vec3::new(2.6, 3.0, 2.2),
                Vec3::new(1.0, 0.86, 0.72),
                demo.key_intensity,
            ),
            Light::point(Vec3::new(-3.0, 1.2, -1.6), Vec3::new(0.5, 0.65, 1.0), 18.0),
            Light::directional(Vec3::new(-0.4, 0.7, -1.0), Vec3::new(0.7, 0.75, 0.9), 1.1),
        ],
    };
    let (ambient_sky, ambient_ground) = demo
        .sky
        .unwrap_or((Vec3::new(0.14, 0.19, 0.28), Vec3::new(0.05, 0.04, 0.035)));
    Environment {
        camera: if matches!(demo.name, "ibl-grid" | "ibl-grid-sky") {
            ibl_grid_camera(aspect)
        } else {
            Camera {
                eye: Vec3::new(2.4, 1.9, 3.2),
                target: Vec3::ZERO,
                aspect,
                ..Camera::default()
            }
        },
        lights,
        ambient_sky,
        ambient_ground,
        exposure: 1.0,
        time,
        previous_time: time,
        // Every gallery camera is fixed, so the velocity stage sees
        // object motion only — and `None` says exactly that.
        previous_camera: None,
    }
}

/// Front row is dielectric; increasing depth increases metallic. Every row
/// contains the exact roughness endpoints and three evenly spaced values.
fn ibl_grid_samples() -> [(Vec3, f32, f32); 25] {
    std::array::from_fn(|index| {
        let column = index % 5;
        let row = index / 5;
        (
            Vec3::new((column as f32 - 2.0) * 2.2, 0.85, (2.0 - row as f32) * 2.2),
            column as f32 / 4.0,
            row as f32 / 4.0,
        )
    })
}

fn ibl_grid_camera(aspect: f32) -> Camera {
    Camera {
        eye: Vec3::new(0.0, 12.0, 14.0),
        target: Vec3::new(0.0, 0.5, 0.0),
        aspect,
        ..Camera::default()
    }
}

/// One shader, independent uniform blocks: no 25 compile-time material variants.
fn ibl_grid_material_graph() -> Graph {
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("IBL roughness/metallic grid");
    let out = graph.add_node("output.surface");
    graph.set_param(out, "base_color", Value::Vec3([0.65, 0.24, 0.08]));
    for name in ["roughness", "metallic"] {
        let parameter = graph.add(
            Node::new("param.value")
                .with_setting("name", name)
                .with_param("value", Value::F32(0.0)),
        );
        graph
            .set_generic(&registry, parameter, "T", ValueType::F32)
            .expect("scalar parameter");
        graph
            .wire(&registry, (parameter, "out"), (out, name))
            .expect("surface scalar");
    }
    graph
}

fn ibl_grid_floor_graph() -> Graph {
    let mut graph = Graph::new("matte grey IBL floor");
    let out = graph.add_node("output.surface");
    graph.set_param(out, "base_color", Value::Vec3([0.3; 3]));
    graph.set_param(out, "roughness", Value::F32(1.0));
    graph.set_param(out, "metallic", Value::F32(0.0));
    graph
}

/// A procedural 64x64 warm checker, in linear space, for whatever the
/// material's graph declares — the same supply-what-was-declared move
/// `pbr_cube` makes.
fn demo_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::TextureView, wgpu::Sampler) {
    const SIDE: u32 = 64;
    let mut texels = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let square = ((x / 8) + (y / 8)) % 2 == 0;
            let grain = (((x * 7 + y * 13) % 11) as f32 / 11.0 - 0.5) * 0.06;
            let base = if square { 0.82 } else { 0.30 };
            let level = ((base + grain).clamp(0.0, 1.0) * 255.0) as u8;
            texels.extend_from_slice(&[
                level,
                (level as f32 * 0.94) as u8,
                (level as f32 * 0.86) as u8,
                255,
            ]);
        }
    }
    let extent = wgpu::Extent3d {
        width: SIDE,
        height: SIDE,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gallery checker"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        &texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIDE * 4),
            rows_per_image: Some(SIDE),
        },
        extent,
    );
    (
        texture.create_view(&wgpu::TextureViewDescriptor::default()),
        device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("gallery sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        }),
    )
}

/// Bind everything the material's graph declared, by name.
fn demo_bindings(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut Renderer,
    material: &wxsl::render::Material,
    texture: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> Result<wxsl::render::MaterialBindings, Box<dyn Error>> {
    let mut bindings = renderer.material_bindings(device, material);
    for resource in &material.interface().resources {
        let name = resource.name.as_str();
        if resource.ty == wxsl::core::node::ValueType::Sampler {
            bindings.set_sampler(name, sampler)?;
        } else {
            bindings.set_texture(name, texture)?;
        }
    }
    bindings.upload(device, queue)?;
    Ok(bindings)
}

impl BakeStage {
    /// Parse the bake material, compile it, create the table its
    /// declaration names — the host owns the texture; the pass list only
    /// writes through it — and bind the material's declared resources to
    /// it. The declaration is the one place size and precision live, so
    /// the texture is created from it and the pipeline's `resource.color`
    /// is expected to agree.
    fn new(gpu: &GpuContext, renderer: &mut Renderer) -> Result<Self, Box<dyn Error>> {
        let graph = bake_material_graph();
        let decl = graph
            .bake("roughness_bake")
            .expect("the demo material declares its table");
        let material = wxsl::render::Material::with_lighting(
            &graph,
            &wxsl::stdlib::registry(),
            &wxsl::render::material::MaterialConfig::default(),
            renderer.lighting(),
        )?;
        let format = wxsl::render::gbuffer_format(decl.precision);
        let [width, height] = decl.size;
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("roughness bake table"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            // Written by the bake pass as storage, sampled by the material
            // as a texture — one texture, both halves of the bake.
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("roughness bake sampler"),
            // The domain is 0..1 — the table covers exactly that — so an
            // out-of-range uv clamps to the edge rather than tiling noise
            // that was never baked.
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let mut bindings = renderer.material_bindings(&gpu.device, &material);
        for resource in &material.interface().resources {
            let name = resource.name.as_str();
            if resource.bake.is_some() && resource.ty == wxsl::core::node::ValueType::Sampler {
                bindings.set_sampler(name, &sampler)?;
            } else if resource.bake.is_some() {
                bindings.set_texture(name, &view)?;
            }
        }
        bindings.upload(&gpu.device, &gpu.queue)?;
        Ok(BakeStage {
            material,
            bindings,
            view,
        })
    }
}

/// One distinct tint per copy, hue-swept; a single copy stays white, the
/// identity for the graph's multiply.
fn instance_tints(count: u32) -> Vec<InstanceAttributes> {
    let count = count.max(1);
    (0..count)
        .map(|index| {
            let tint = if count == 1 {
                Vec3::ONE
            } else {
                let hue = index as f32 / count as f32 * std::f32::consts::TAU;
                Vec3::new(hue.cos(), (hue + 2.09).cos(), (hue + 4.19).cos()) * 0.4 + 0.6
            };
            InstanceAttributes::new().with("instance_tint", Value::Vec3(tint.to_array()))
        })
        .collect()
}

/// The PBR cube graph, plus an `alpha` uniform.
///
/// The cube's surface output leaves `alpha` at its default of 1, which is
/// inlined. A `param.value` is a uniform the host can move without
/// recompiling; this demo pins it at 0.3 and tags the material
/// `transparent`, which is the only tag `pass.peel` draws.
fn peel_material_graph(base: &Graph) -> Result<Graph, Box<dyn Error>> {
    let registry = wxsl::stdlib::registry();
    let mut graph = base.clone();
    graph.set_name("peel surfaces");
    let output = graph
        .nodes()
        .find(|(_, node)| node.def == abi::SURFACE_OUTPUT_ID)
        .map(|(id, _)| id)
        .expect("the PBR cube graph ends at output.surface");
    let alpha = graph.add(
        Node::new("param.value")
            .with_label("alpha")
            .with_setting("name", "alpha")
            .with_param("value", Value::F32(0.3)),
    );
    graph
        .set_generic(&registry, alpha, "T", ValueType::F32)
        .map_err(|error| format!("the alpha uniform does not resolve: {error}"))?;
    graph
        .wire(&registry, (alpha, "out"), (output, "alpha"))
        .map_err(|error| format!("the alpha uniform does not wire: {error}"))?;
    graph
        .validate(&registry)
        .map_err(|error| format!("the peel material does not validate: {error}"))?;
    Ok(graph)
}

fn cube_transform(time: f32) -> Mat4 {
    Mat4::from_rotation_y(time * 0.45) * Mat4::from_rotation_x(time * 0.21)
}

/// The fragment-only family as one surface (plan5 D6): `checker_aa`
/// carries the pattern, and an SDF disc's alpha takes its width from
/// `screen_width` — the exact caller-side recipe `sdf_coverage`'s doc
/// describes. Blended by the demo's document, so the disc's edge is real
/// coverage, not a hard step.
fn sdf_aa_material_graph() -> Result<Graph, Box<dyn Error>> {
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("sdf aa");
    let uv = graph.add_node("input.uv");
    let checker = graph
        .add(Node::new("generative.checker_aa").with_param("cells", Value::Vec2([24.0, 24.0])));
    let pattern = graph.add_node("convert.splat");
    let centre = graph.add(Node::new("const.value").with_param("value", Value::Vec2([0.5, 0.5])));
    let p = graph.add_node("math.subtract");
    let circle = graph.add(Node::new("sdf.circle").with_param("radius", Value::F32(0.36)));
    let width = graph.add_node("math.screen_width");
    let coverage = graph.add_node("sdf.coverage");
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph
        .set_generic(&registry, pattern, "T", ValueType::Vec3)
        .map_err(|error| format!("the splat does not resolve: {error}"))?;
    for side in ["A", "B"] {
        graph
            .set_generic(&registry, p, side, ValueType::Vec2)
            .map_err(|error| format!("the subtract does not resolve: {error}"))?;
    }
    for (from, to) in [
        ((uv, "out"), (checker, "uv")),
        ((checker, "out"), (pattern, "value")),
        ((uv, "out"), (p, "a")),
        ((centre, "out"), (p, "b")),
        ((p, "out"), (circle, "p")),
        ((circle, "out"), (width, "value")),
        ((circle, "out"), (coverage, "distance")),
        ((width, "out"), (coverage, "width")),
        ((pattern, "out"), (output, "base_color")),
        ((coverage, "out"), (output, "alpha")),
    ] {
        graph
            .wire(&registry, from, to)
            .map_err(|error| format!("the sdf-aa graph does not wire: {error}"))?;
    }
    graph
        .validate(&registry)
        .map_err(|error| format!("the sdf-aa material does not validate: {error}"))?;
    Ok(graph)
}

/// A thin-film sphere whose graph authors thickness, not reflected colour.
fn iridescent_material_graph() -> Result<Graph, Box<dyn Error>> {
    let registry = wxsl::stdlib::registry();
    let mut graph = Graph::new("iridescence");
    // The thickness field lives in *object* space — sampled from the
    // world position it would stay put while the sphere turns underneath
    // it, and the film would appear to swim across the surface instead of
    // rotating with it — and only the vertex stage has object space, so
    // the field crosses through a declared interpolant (ADR 0027).
    let position = graph.add_node("input.object_position");
    let scale = graph.add(Node::new("math.multiply").with_param("b", Value::Vec3([1.1, 1.1, 1.1])));
    let noise = graph.add_node("generative.simplex3");
    let thickness = graph.add(
        Node::new("math.remap")
            .with_param("in_min", Value::F32(-1.0))
            .with_param("in_max", Value::F32(1.0))
            .with_param("out_min", Value::F32(240.0))
            .with_param("out_max", Value::F32(880.0)),
    );
    graph.declare_attribute(AttributeDecl::computed("film_thickness", ValueType::F32));
    let varying =
        graph.add(Node::new(abi::VARYING_OUTPUT_ID).with_setting("name", "film_thickness"));
    let thickness_in =
        graph.add(Node::new("input.attribute").with_setting("name", "film_thickness"));
    let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
    graph.set_param(output, "base_color", Value::Vec3([0.42, 0.38, 0.36]));
    graph.set_param(output, "metallic", Value::F32(0.85));
    graph.set_param(output, "roughness", Value::F32(0.3));
    graph.set_param(output, "iridescence_strength", Value::F32(1.0));
    for side in ["A", "B"] {
        graph
            .set_generic(&registry, scale, side, ValueType::Vec3)
            .map_err(|error| format!("the scale does not resolve: {error}"))?;
    }
    graph
        .set_generic(&registry, thickness, "T", ValueType::F32)
        .map_err(|error| format!("the remap does not resolve: {error}"))?;
    for interpolant in [&varying, &thickness_in] {
        graph
            .set_generic(&registry, *interpolant, "T", ValueType::F32)
            .map_err(|error| format!("the interpolant does not resolve: {error}"))?;
    }
    for (from, to) in [
        ((position, "out"), (scale, "a")),
        ((scale, "out"), (noise, "p")),
        ((noise, "out"), (thickness, "value")),
        ((thickness, "out"), (varying, abi::SOCKET_VARYING)),
        ((thickness_in, "out"), (output, "iridescence_thickness")),
    ] {
        graph
            .wire(&registry, from, to)
            .map_err(|error| format!("the iridescence graph does not wire: {error}"))?;
    }
    graph
        .validate(&registry)
        .map_err(|error| format!("the iridescent material does not validate: {error}"))?;
    Ok(graph)
}

/// The motion-blur demo's path: a sweep across the frame, fast enough
/// that one frame's motion is a visible smear, and slow enough to stay
/// on screen.
fn swept_transform(time: f32) -> Mat4 {
    let travel = (time * 0.9).sin() * 2.2;
    Mat4::from_translation(Vec3::new(travel, 0.0, 0.0)) * Mat4::from_rotation_y(time * 1.8)
}

/// How a frame's cube moves: the demo's motion, if it has one, and the
/// frame step its previous-frame transform answers for — one bundle so
/// the draw-list builder stays readable.
#[derive(Clone, Copy)]
struct MotionPlan {
    motion: Option<Motion>,
    step: f32,
}

fn cube_draws<'a>(
    mesh: &'a Mesh,
    material: &'a wxsl::render::Material,
    bindings: &'a wxsl::render::MaterialBindings,
    tints: &'a [InstanceAttributes],
    count: u32,
    time: f32,
    plan: MotionPlan,
) -> DrawList<'a> {
    (0..count.max(1))
        .map(|index| {
            let offset = index as f32 - (count.max(1) - 1) as f32 * 0.5;
            let place = Mat4::from_translation(Vec3::new(offset * 2.4, 0.0, 0.0));
            let mut item = DrawItem::new(mesh, material)
                .with_transform(match plan.motion {
                    Some(motion) => place * (motion.path)(time),
                    None => place * cube_transform(time),
                })
                .with_bindings(bindings);
            // Where the cube was one frame ago, for the velocity stage's
            // previous-instance row. Only a demo whose cube *moves*
            // states it: every other draw stays `None`, which the frame
            // group reads as this frame's own transform — zero motion,
            // and the behaviour every frame had before the row existed.
            if let Some(motion) = plan.motion {
                item = item.with_previous(place * (motion.path)((time - plan.step).max(0.0)));
            }
            if let Some(tint) = tints.get(index as usize) {
                item = item.with_attributes(tint);
            }
            item
        })
        .collect()
}

/// Everything a frame needs, shared by every demo: one renderer, one
/// mesh, one material, one set of bindings.
struct Stage {
    ibl_source: wxsl::render::environment::EnvironmentImage,
    ibl_sky: bool,
    /// The screen graph normalizes UV by its input image's size. The sky
    /// bake's extent input must match its output, independent of HDR dimensions.
    sky_extent: wgpu::TextureView,
    gpu: GpuContext,
    renderer: Renderer,
    /// The scene's graph, recompiled when a demo's feature set differs
    /// from the one the current material was resolved against.
    scene_graph: Graph,
    material: wxsl::render::Material,
    mesh: Mesh,
    bindings: wxsl::render::MaterialBindings,
    tints: Vec<InstanceAttributes>,
    /// The demo texture and sampler, kept so a rebuilt material's
    /// bindings can be refilled.
    texture: wgpu::TextureView,
    sampler: wgpu::Sampler,
    /// The feature set the current material was resolved against.
    material_features: &'static [&'static str],
    material_model: Option<&'static str>,
    /// The peel demo's surfaces: the cube graph with the alpha uniform,
    /// on a torus and a sphere that occupy the same space.
    peel_material: wxsl::render::Material,
    peel_bindings: wxsl::render::MaterialBindings,
    /// Same material, with the `tint` uniform set blue.
    sphere_bindings: wxsl::render::MaterialBindings,
    sphere: Mesh,
    /// The sorted tier (plan5 D5): the same peel surface with no layer
    /// budget, so no peel pass reads it; the tiered document composites
    /// it over the pair through the sorted-tier pass.
    plate_material: wxsl::render::Material,
    plate_bindings: wxsl::render::MaterialBindings,
    plate: Mesh,
    torus: Mesh,
    /// The render-order demo's three cards: the order `-1` underlay, the
    /// order-0 sheet, and the order-1 hologram behind it in depth yet
    /// drawn after it — the group order is data (plan5 D4).
    order: [DemoMaterial; 3],
    /// The sdf-aa demo's blended pattern surface (plan5 D6): an AA'd
    /// checker and an SDF disc whose edge is `screen_width`-wide
    /// coverage.
    sdf_aa: DemoMaterial,
    /// The iridescence demo's thin-film sphere (plan4's ticket 6).
    iridescent: DemoMaterial,
    /// Graph-authored sheen tint and roughness (ADR 0059).
    sheen: DemoMaterial,
    /// White conductor, no procedural texture or direct lamps.
    mirror: DemoMaterial,
    grid_material: wxsl::render::Material,
    grid_bindings: Vec<wxsl::render::MaterialBindings>,
    grid_floor: DemoMaterial,
    grid_floor_mesh: Mesh,
    /// The bake demo's own material and the table its bake pass writes
    /// through (ADR 0045): the graph is parsed once, the effect and the
    /// material are compiled from it, the texture is the *material's*
    /// (the host creates and owns it; the pass list only writes through
    /// it), and the view rides the renderer as an import under the
    /// declaration's name.
    bake: Option<BakeStage>,
}

/// The bake demo's half of the stage. The view is kept because the import
/// it feeds is *per demo*: the demos before and after take the view back
/// (`apply` removes it), and each bake frame hands it over again.
struct BakeStage {
    material: wxsl::render::Material,
    bindings: wxsl::render::MaterialBindings,
    view: wgpu::TextureView,
}

/// One demo's own material and its bindings — the bake stage's shape,
/// for the demos that need a material of their own but no bake table.
struct DemoMaterial {
    material: wxsl::render::Material,
    bindings: wxsl::render::MaterialBindings,
}

impl DemoMaterial {
    fn new(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        texture: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        graph: &Graph,
        config: &wxsl::render::material::MaterialConfig,
    ) -> Result<Self, Box<dyn Error>> {
        let material =
            wxsl::render::Material::with_config(graph, &wxsl::stdlib::registry(), config)?;
        let bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            renderer,
            &material,
            texture,
            sampler,
        )?;
        Ok(DemoMaterial { material, bindings })
    }
}

impl Stage {
    fn new(
        gpu: GpuContext,
        target: TargetConfig,
        ibl_source: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let registry = wxsl::stdlib::registry();
        let scene_graph: Graph =
            serde_json::from_str(include_str!("../assets/pbr_cube.wxsl.json"))?;
        scene_graph.validate(&registry)?;
        let material = wxsl::render::Material::from_graph(&scene_graph, &registry)?;
        let mut renderer = Renderer::new(&gpu.device, wxsl::stdlib_library(), target)?;
        for effect in [
            wxsl::render::effect::ibl::EQUIRECT_TO_CUBE,
            wxsl::render::effect::ibl::DIFFUSE,
            wxsl::render::effect::ibl::SPECULAR,
            wxsl::render::effect::ibl::RESAMPLE,
        ] {
            renderer.add_effect(effect);
        }
        let ibl_sky = ibl_source == "sky";
        let sky_extent = gpu
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("gallery sky extent"),
                size: wgpu::Extent3d {
                    width: 1024,
                    height: 512,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let ibl_source = if ibl_sky {
            wxsl::render::environment::upload_environment_image(
                &gpu.device,
                &gpu.queue,
                1,
                1,
                &[[0.0; 3]],
            )?
        } else {
            let reader = image::ImageReader::open(ibl_source)?.with_guessed_format()?;
            if reader.format() != Some(image::ImageFormat::Hdr) {
                return Err("--ibl-source expects a Radiance .hdr image or sky".into());
            }
            let image = reader.decode()?.to_rgb32f();
            let pixels: Vec<[f32; 3]> = image.pixels().map(|pixel| pixel.0).collect();
            println!(
                "IBL HDR: {ibl_source} ({}x{}, max radiance {:.3})",
                image.width(),
                image.height(),
                pixels.iter().flatten().copied().fold(0.0_f32, f32::max)
            );
            wxsl::render::environment::upload_environment_image(
                &gpu.device,
                &gpu.queue,
                image.width(),
                image.height(),
                &pixels,
            )?
        };
        // The proof effects (ADRs 0035–0037) ship as descriptors; an
        // application registers them, which is the whole of "add a
        // compute pass" now.
        renderer.add_effect(BRDF_LUT);
        renderer.add_effect(LUT_VIEW);
        renderer.add_effect(RAMP_FILL);
        renderer.add_effect(RAMP_VIEW);
        // And the effects that are *graphs* (ADRs 0040, 0060). `tonemap`
        // registers under the id every stock document already names, so
        // every demo above presents through a generated module from here
        // on — which is the claim, and the fact that none of the images
        // move is the evidence.
        let nodes = wxsl::stdlib::registry();
        renderer.add_effect(wxsl::effects::tonemap(&nodes)?);
        renderer.add_effect(wxsl::effects::fxaa(&nodes)?);
        renderer.add_effect(wxsl::effects::sky(&nodes)?);
        // The bake effect is generated from the material subgraph (ADR
        // 0045) — registration is where generation happens, so a cone that
        // reads something a bake cannot evaluate is a failure here rather
        // than at the first frame.
        renderer.add_effect(bake_effect());
        let mesh = Mesh::cube(&gpu.device, 1.6);
        let (texture, sampler) = demo_texture(&gpu.device, &gpu.queue);
        let bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            &mut renderer,
            &material,
            &texture,
            &sampler,
        )?;
        let bake = BakeStage::new(&gpu, &mut renderer)?;
        let peel_graph = peel_material_graph(&scene_graph)?;
        // The peeled pair asks for the demo's whole layer budget (plan5
        // D5): transparency is tiered now, and a transparent with no
        // budget is nobody the peel reads.
        let peel_material = wxsl::render::Material::with_config(
            &peel_graph,
            &registry,
            &wxsl::render::material::MaterialConfig::default()
                .with_tags(Tags::from_iter([TAG_TRANSPARENT]))
                .with_shadows(false, false)
                .with_max_layers(4),
        )?;
        let plate_material = wxsl::render::Material::with_config(
            &peel_graph,
            &registry,
            &wxsl::render::material::MaterialConfig::default()
                .with_tags(Tags::from_iter([TAG_TRANSPARENT]))
                .with_shadows(false, false),
        )?;
        let peel_bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            &mut renderer,
            &peel_material,
            &texture,
            &sampler,
        )?;
        let mut sphere_bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            &mut renderer,
            &peel_material,
            &texture,
            &sampler,
        )?;
        sphere_bindings.set("tint", Value::Vec3([0.05, 0.22, 1.0]))?;
        sphere_bindings.upload(&gpu.device, &gpu.queue)?;
        let mut plate_bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            &mut renderer,
            &plate_material,
            &texture,
            &sampler,
        )?;
        // Green, so the tier reads as itself in the picture.
        plate_bindings.set("tint", Value::Vec3([0.1, 0.9, 0.2]))?;
        plate_bindings.upload(&gpu.device, &gpu.queue)?;
        let sphere = Mesh::sphere(&gpu.device, 0.85);
        let torus = Mesh::torus(&gpu.device, 1.15, 0.38);
        let plate = Mesh::plane(&gpu.device, 3.4);
        // The render-order demo's cards, from the peel graph so the tint
        // and alpha uniforms drive them: underlay, sheet, hologram.
        let mut cards: Vec<DemoMaterial> = Vec::new();
        for (order, tint, alpha) in [
            (-1, [0.2, 0.2, 0.2], 1.0),
            (0, [0.1, 0.9, 0.2], 0.5),
            (1, [1.0, 0.15, 0.1], 0.9),
        ] {
            let config = wxsl::render::material::MaterialConfig::default()
                .with_tags(Tags::from_iter([TAG_TRANSPARENT]))
                .with_shadows(false, false)
                .with_render_order(order);
            let mut card = DemoMaterial::new(
                &gpu,
                &mut renderer,
                &texture,
                &sampler,
                &peel_graph,
                &config,
            )?;
            card.bindings.set("tint", Value::Vec3(tint))?;
            card.bindings.set("alpha", Value::F32(alpha))?;
            card.bindings.upload(&gpu.device, &gpu.queue)?;
            cards.push(card);
        }
        let order: [DemoMaterial; 3] = cards
            .try_into()
            .unwrap_or_else(|_| unreachable!("exactly three cards were pushed"));
        let sdf_graph = sdf_aa_material_graph()?;
        let sdf_aa = DemoMaterial::new(
            &gpu,
            &mut renderer,
            &texture,
            &sampler,
            &sdf_graph,
            &wxsl::render::material::MaterialConfig::default()
                .with_tags(Tags::from_iter([TAG_TRANSPARENT]))
                .with_shadows(false, false),
        )?;
        let iridescent_graph = iridescent_material_graph()?;
        let film_set = wxsl::core::lighting::LightingSet::single(
            *wxsl::core::lighting::DEFAULT_MODELS
                .iter()
                .find(|model| model.name == "wxsl.iridescent")
                .expect("shipped film model"),
        );
        let film_material = wxsl::render::Material::with_lighting(
            &iridescent_graph,
            &registry,
            &wxsl::render::material::MaterialConfig::default().with_model("iridescent"),
            &film_set,
        )?;
        let film_bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            &mut renderer,
            &film_material,
            &texture,
            &sampler,
        )?;
        let iridescent = DemoMaterial {
            material: film_material,
            bindings: film_bindings,
        };
        let mut sheen_scene: wxsl::core::scene::Scene =
            serde_json::from_str(include_str!("../assets/sheen.scene.json"))?;
        let sheen_entry = sheen_scene.materials.remove(0);
        let sheen_set = wxsl::core::lighting::LightingSet::single(
            *wxsl::core::lighting::DEFAULT_MODELS
                .iter()
                .find(|model| model.name == "wxsl.sheen")
                .expect("shipped sheen model"),
        );
        let sheen_material = wxsl::render::Material::with_lighting(
            &sheen_entry.graph,
            &registry,
            &sheen_entry.config,
            &sheen_set,
        )?;
        let sheen_bindings = demo_bindings(
            &gpu.device,
            &gpu.queue,
            &mut renderer,
            &sheen_material,
            &texture,
            &sampler,
        )?;
        let sheen = DemoMaterial {
            material: sheen_material,
            bindings: sheen_bindings,
        };
        let mut mirror_graph = Graph::new("mirror sphere");
        let output = mirror_graph.add_node("output.surface");
        mirror_graph.set_param(output, "base_color", Value::Vec3([1.0; 3]));
        mirror_graph.set_param(output, "metallic", Value::F32(1.0));
        mirror_graph.set_param(output, "roughness", Value::F32(0.0));
        let mirror = DemoMaterial::new(
            &gpu,
            &mut renderer,
            &texture,
            &sampler,
            &mirror_graph,
            &wxsl::render::material::MaterialConfig::default().with_shadows(false, false),
        )?;
        let grid_config =
            wxsl::render::material::MaterialConfig::default().with_shadows(false, false);
        let grid_material = wxsl::render::Material::with_config(
            &ibl_grid_material_graph(),
            &registry,
            &grid_config,
        )?;
        let mut grid_bindings = Vec::new();
        for (_, roughness, metallic) in ibl_grid_samples() {
            let mut bindings = renderer.material_bindings(&gpu.device, &grid_material);
            bindings.set("roughness", Value::F32(roughness))?;
            bindings.set("metallic", Value::F32(metallic))?;
            bindings.upload(&gpu.device, &gpu.queue)?;
            grid_bindings.push(bindings);
        }
        let grid_floor = DemoMaterial::new(
            &gpu,
            &mut renderer,
            &texture,
            &sampler,
            &ibl_grid_floor_graph(),
            &grid_config,
        )?;
        let grid_floor_mesh = Mesh::plane(&gpu.device, 14.0);
        Ok(Stage {
            ibl_source,
            ibl_sky,
            sky_extent,
            gpu,
            renderer,
            scene_graph,
            material,
            mesh,
            bindings,
            tints: instance_tints(1),
            texture,
            sampler,
            material_features: &[],
            material_model: None,
            peel_material,
            peel_bindings,
            plate_material,
            plate_bindings,
            plate,
            sphere_bindings,
            sphere,
            torus,
            order,
            sdf_aa,
            iridescent,
            sheen,
            mirror,
            grid_material,
            grid_bindings,
            grid_floor,
            grid_floor_mesh,
            bake: Some(bake),
        })
    }

    /// Put the scene's material on `features`' plan, if the demo's
    /// differs from the current one (plan2 P12). A material is resolved
    /// against the plan of the pipeline it will draw under — that is the
    /// handshake the frame compile checks.
    fn ensure_material(
        &mut self,
        features: &'static [&'static str],
        model: Option<&'static str>,
    ) -> Result<(), Box<dyn Error>> {
        if self.material_features == features && self.material_model == model {
            return Ok(());
        }
        let registry = wxsl::stdlib::registry();
        let material = wxsl::render::Material::with_lighting(
            &self.scene_graph,
            &registry,
            &wxsl::render::material::MaterialConfig {
                features: wxsl::core::lighting::feature_requests(features)?,
                model: model.map(str::to_string),
                ..wxsl::render::material::MaterialConfig::default()
            },
            self.renderer.lighting(),
        )?;
        self.bindings = demo_bindings(
            &self.gpu.device,
            &self.gpu.queue,
            &mut self.renderer,
            &material,
            &self.texture,
            &self.sampler,
        )?;
        self.material = material;
        self.material_features = features;
        self.material_model = model;
        Ok(())
    }

    /// Render one frame of `demo` at `time`, into `view`, where `step`
    /// is how long the frame before it took — the shutter the velocity
    /// stage's previous-instance row answers for. A screenshot states
    /// the demo's own step; the live loop states what it measured.
    fn render(
        &mut self,
        demo: &Demo,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        time: f32,
        step: f32,
    ) -> Result<(), Box<dyn Error>> {
        let uses_ibl = matches!(
            demo.name,
            "ibl" | "ibl-mirror" | "ibl-grid" | "ibl-grid-sky"
        );
        let use_sky = self.ibl_sky || demo.name == "ibl-grid-sky";
        let source_changed = uses_ibl
            && use_sky
                != self
                    .renderer
                    .render_graph()
                    .resource_by_label("gallery sky")
                    .is_some();
        if !uses_ibl || self.renderer.render_graph().environment_maps().is_none() || source_changed
        {
            self.renderer.remove_import("gallery HDR");
            apply(demo, &mut self.renderer)?;
            if uses_ibl {
                use wxsl::render::pass::{
                    Attachment, Extent, PassDesc, Policy, Read, ResourceDesc,
                };
                use wxsl::render::types::{Color, TextureFormat};
                let mut graph = self.renderer.render_graph().clone();
                let imported = graph.resource(ResourceDesc::imported(
                    "gallery HDR",
                    if use_sky {
                        TextureFormat::Rgba8Unorm
                    } else {
                        TextureFormat::Rgba32Float
                    },
                ));
                let source = if use_sky {
                    let sky = graph.resource(
                        ResourceDesc::color("gallery sky", TextureFormat::Rgba16Float)
                            .with_extent(Extent::Fixed {
                                width: 1024,
                                height: 512,
                            })
                            .persistent(0),
                    );
                    graph.pass(
                        PassDesc::screen("gallery sky", "wxsl.sky")
                            .with_reads([Read::current(imported)])
                            .with_color(Attachment::clear(sky, Color::BLACK))
                            .with_policy(Policy::Once),
                    );
                    sky
                } else {
                    imported
                };
                let maps = wxsl::render::effect::ibl::append_filtered_bake(
                    &mut graph,
                    source,
                    "gallery IBL",
                    128,
                    16,
                    8,
                );
                graph.declare_environment_maps(maps.diffuse, maps.specular);
                graph.set_environment_scale(if use_sky { 1.0 } else { self.ibl_source.scale });
                let tonemap = graph
                    .passes()
                    .iter()
                    .position(|pass| {
                        matches!(&pass.kind,
                    wxsl::render::pass::PassKind::Screen { effect } if effect == "wxsl.tonemap")
                    })
                    .ok_or("IBL chain needs tonemap")?;
                let color = graph.passes()[tonemap].reads[0].resource;
                let depth = graph
                    .passes()
                    .iter()
                    .filter_map(|pass| pass.depth.as_ref().map(|a| a.resource))
                    .next_back()
                    .ok_or("IBL chain needs opaque depth")?;
                let background = wxsl::render::effect::ibl::append_background(
                    &mut graph,
                    color,
                    depth,
                    maps.radiance,
                    "gallery environment background",
                );
                graph.pass_mut(tonemap).unwrap().reads[0] = Read::current(background);
                self.renderer.set_graph(graph)?;
                self.renderer.import_resource(
                    "gallery HDR",
                    if use_sky {
                        self.sky_extent.clone()
                    } else {
                        self.ibl_source.view.clone()
                    },
                );
            }
        }
        self.ensure_material(demo.features, demo.model)?;
        self.tints = instance_tints(demo.instances);
        let environment = demo_environment(demo, width as f32 / height.max(1) as f32, time);
        if matches!(demo.name, "ibl-grid" | "ibl-grid-sky") {
            let mut draws = DrawList::new();
            draws.push(
                DrawItem::new(&self.grid_floor_mesh, &self.grid_floor.material)
                    .with_bindings(&self.grid_floor.bindings),
            );
            for ((position, _, _), bindings) in ibl_grid_samples().iter().zip(&self.grid_bindings) {
                draws.push(
                    DrawItem::new(&self.sphere, &self.grid_material)
                        .with_transform(Mat4::from_translation(*position))
                        .with_bindings(bindings),
                );
            }
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if demo.name == "peel" {
            let tint =
                InstanceAttributes::new().with("instance_tint", Value::Vec3([1.0, 1.0, 1.0]));
            // The torus, the sphere, and the sorted tier's plate. The
            // sphere sits in the torus's tube so a ray through the pair
            // meets both surfaces, and the plate hangs behind and below
            // them — the working case for the tiers (plan5 D5): a sorted
            // transparent the peel composite does not cover. It is
            // submitted *first*, so only the sorted pass's own
            // back-to-front sort and the peel's depth windows decide what
            // lands in front.
            let spin = Mat4::from_rotation_y(time * 0.35);
            let surfaces = [
                (
                    &self.plate,
                    &self.plate_material,
                    &self.plate_bindings,
                    spin * Mat4::from_translation(Vec3::new(-0.9, -1.15, -1.9))
                        * Mat4::from_rotation_x(1.2),
                ),
                (
                    &self.torus,
                    &self.peel_material,
                    &self.peel_bindings,
                    spin * Mat4::from_rotation_x(0.7),
                ),
                (
                    &self.sphere,
                    &self.peel_material,
                    &self.sphere_bindings,
                    spin * Mat4::from_translation(Vec3::new(0.72, 0.08, 0.12)),
                ),
            ];
            let draws: DrawList = surfaces
                .into_iter()
                .map(|(mesh, material, bindings, place)| {
                    DrawItem::new(mesh, material)
                        .with_transform(place)
                        .with_bindings(bindings)
                        .with_attributes(&tint)
                })
                .collect();
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if demo.name == "render-order" {
            // Three draws, one blended pass: the underlay at order -1,
            // the sheet at order 0, the hologram at order 1 *behind the
            // sheet in depth* — drawn after it anyway, because the group
            // order is the draws' data, not the submission's (plan5 D4).
            // The pass sorts back to front, so the composite is the
            // groups in ascending order whatever order they arrive in.
            // The hologram is a *sphere* on purpose: the tier's assumption
            // is no self-overlap, and a convex draw honours it (a torus
            // folds against itself — that is what `pass.peel` is for).
            let [underlay, sheet, hologram] = &self.order;
            // The tint uniform is the graph's colour here; the per-instance
            // attribute it declares is the peel block's white identity.
            let tint =
                InstanceAttributes::new().with("instance_tint", Value::Vec3([1.0, 1.0, 1.0]));
            let stand = Mat4::from_rotation_x(FRAC_PI_2);
            let items = [
                DrawItem::new(&self.plate, &underlay.material)
                    .with_transform(Mat4::from_translation(Vec3::new(0.0, 0.0, -1.8)) * stand)
                    .with_bindings(&underlay.bindings)
                    .with_attributes(&tint),
                DrawItem::new(&self.plate, &sheet.material)
                    .with_transform(Mat4::from_translation(Vec3::new(0.0, 0.0, 0.5)) * stand)
                    .with_bindings(&sheet.bindings)
                    .with_attributes(&tint),
                DrawItem::new(&self.sphere, &hologram.material)
                    .with_transform(
                        Mat4::from_translation(Vec3::new(0.0, 0.0, -0.6))
                            * Mat4::from_rotation_y(time * 0.6),
                    )
                    .with_bindings(&hologram.bindings)
                    .with_attributes(&tint),
            ];
            let draws: DrawList = items.into_iter().collect();
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if demo.name == "sdf-aa" {
            // One card, blended: the AA'd checker and the SDF disc whose
            // edge is one screen-space pixel wide (plan5 D6).
            let draws: DrawList = [DrawItem::new(&self.plate, &self.sdf_aa.material)
                .with_transform(
                    Mat4::from_translation(Vec3::new(0.0, 0.0, 0.4))
                        * Mat4::from_rotation_x(FRAC_PI_2),
                )
                .with_bindings(&self.sdf_aa.bindings)]
            .into_iter()
            .collect();
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        if matches!(demo.name, "iridescence" | "sheen" | "ibl-mirror") {
            let layer = if demo.name == "ibl-mirror" {
                &self.mirror
            } else if demo.name == "sheen" {
                &self.sheen
            } else {
                &self.iridescent
            };
            let draws: DrawList = [DrawItem::new(&self.sphere, &layer.material)
                .with_transform(cube_transform(time * 0.5))
                .with_bindings(&layer.bindings)]
            .into_iter()
            .collect();
            self.renderer.render(
                &self.gpu.device,
                &self.gpu.queue,
                &RenderRequest {
                    view,
                    environment: &environment,
                    draws: &draws,
                },
            )?;
            return Ok(());
        }
        // A demo with a material of its own draws it; every other demo
        // draws the shared cube. The bake material declares no per-instance
        // attributes, so its tints are empty and one copy draws.
        let (material, bindings, tints, instances) = if demo.material.is_some() {
            let bake = self.bake.as_ref().expect("the bake stage was built");
            // The table's view rides with its demo: every other demo took
            // it back, and a view nothing declares is an error by name.
            self.renderer
                .import_resource("roughness_bake", bake.view.clone());
            (&bake.material, &bake.bindings, &[][..], 1)
        } else {
            (
                &self.material,
                &self.bindings,
                &self.tints[..],
                demo.instances,
            )
        };
        let draws = cube_draws(
            &self.mesh,
            material,
            bindings,
            tints,
            instances,
            time,
            MotionPlan {
                motion: demo.motion,
                step,
            },
        );
        self.renderer.render(
            &self.gpu.device,
            &self.gpu.queue,
            &RenderRequest {
                view,
                environment: &environment,
                draws: &draws,
            },
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Screenshots
// ---------------------------------------------------------------------------

/// One rendered demo, on its way to a PNG and the contact sheet.
struct Shot {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

fn run_screenshots(
    size: Option<(u32, u32)>,
    demos: &[Demo],
    dir: &Path,
    ibl_source: &str,
) -> Result<(), Box<dyn Error>> {
    let (width, height) = size.unwrap_or((800, 600));
    let gpu = pollster::block_on(GpuContext::headless())?;
    println!("adapter: {}", gpu.adapter.get_info().name);
    let target = OffscreenTarget::new(&gpu.device, width, height);
    let mut stage = Stage::new(
        gpu,
        TargetConfig::new(width, height, target.format()),
        ibl_source,
    )?;

    std::fs::create_dir_all(dir)?;
    let mut shots: Vec<Shot> = Vec::new();
    for demo in demos {
        // A fixed time, so two runs produce identical images. A temporal
        // pipeline's demo renders its warmup first — the same step, one
        // frame at a time — so the captured frame shows a settled
        // history instead of frame one's empty ring.
        if let Some(motion) = demo.motion {
            for frame in 0..demo.warmup {
                stage.render(
                    demo,
                    target.view(),
                    width,
                    height,
                    0.6 - (demo.warmup - frame) as f32 * motion.step,
                    motion.step,
                )?;
            }
        }
        let step = demo.motion.map_or(0.0, |motion| motion.step);
        stage.render(demo, target.view(), width, height, 0.6, step)?;
        stage.gpu.wait();
        let pixels = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
        let file = dir.join(format!("{}.png", demo.name));
        image::save_buffer(
            &file,
            &pixels,
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )?;
        println!(
            "{:>16}: {} covered pixels, mean luminance {:.4} -> {}",
            demo.name,
            covered_pixels(&pixels),
            mean_luminance(&pixels),
            file.display()
        );
        shots.push(Shot {
            width,
            height,
            pixels,
        });
    }

    let sheet = dir.join("gallery.png");
    write_contact_sheet(&shots, &sheet)?;
    println!(
        "\ncontact sheet -> {} ({} demos)",
        sheet.display(),
        shots.len()
    );
    Ok(())
}

/// Tile every shot into one image, dark gutters between — the "see their
/// result" file, reviewable at a glance.
fn write_contact_sheet(shots: &[Shot], path: &Path) -> Result<(), Box<dyn Error>> {
    const GUTTER: u32 = 16;
    let cell_w = shots.iter().map(|shot| shot.width).max().unwrap_or(1);
    let cell_h = shots.iter().map(|shot| shot.height).max().unwrap_or(1);
    let cols = ((shots.len() as f32).sqrt().ceil() as u32).max(1);
    let rows = shots.len() as u32 / cols + u32::from(shots.len() as u32 % cols != 0);
    let sheet_w = cols * cell_w + (cols + 1) * GUTTER;
    let sheet_h = rows * cell_h + (rows + 1) * GUTTER;
    let mut sheet = vec![22u8; (sheet_w * sheet_h * 4) as usize];
    for index in (0..sheet.len()).step_by(4) {
        sheet[index + 3] = 255;
    }
    for (position, shot) in shots.iter().enumerate() {
        let position = position as u32;
        let x0 = GUTTER + (position % cols) * (cell_w + GUTTER);
        let y0 = GUTTER + (position / cols) * (cell_h + GUTTER);
        for y in 0..shot.height {
            let from = (y * shot.width * 4) as usize;
            let to = (((y0 + y) * sheet_w + x0) * 4) as usize;
            let row = (shot.width * 4) as usize;
            sheet[to..to + row].copy_from_slice(&shot.pixels[from..from + row]);
        }
    }
    image::save_buffer(
        path,
        &sheet,
        sheet_w,
        sheet_h,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(())
}

fn covered_pixels(pixels: &[u8]) -> usize {
    pixels
        .chunks_exact(4)
        .filter(|texel| texel[0] > 12 || texel[1] > 12 || texel[2] > 12)
        .count()
}

fn mean_luminance(pixels: &[u8]) -> f32 {
    let sum: f32 = pixels
        .chunks_exact(4)
        .map(|texel| {
            (0.2126 * texel[0] as f32 + 0.7152 * texel[1] as f32 + 0.0722 * texel[2] as f32) / 255.0
        })
        .sum();
    sum / (pixels.len() / 4) as f32
}

// ---------------------------------------------------------------------------
// Windowed
// ---------------------------------------------------------------------------

/// Everything that only exists once there is a window.
struct State {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    stage: Stage,
    format: wgpu::TextureFormat,
}

struct App {
    options: Options,
    demos: Vec<Demo>,
    current: usize,
    state: Option<State>,
    started: Instant,
    paused_at: Option<f32>,
    error: Option<Box<dyn Error>>,
    /// When the previous frame rendered, so a temporal demo's velocity
    /// answers for the *measured* frame time rather than an assumed one.
    last_frame: Option<Instant>,
}

impl App {
    fn elapsed(&self) -> f32 {
        self.paused_at
            .unwrap_or_else(|| self.started.elapsed().as_secs_f32())
    }

    fn announce(&self) {
        let demo = &self.demos[self.current];
        let stats = self
            .state
            .as_ref()
            .map(|state| {
                format!(
                    "{} shader variants, {} pipelines",
                    state.stage.renderer.variant_count(),
                    state.stage.renderer.pipeline_count()
                )
            })
            .unwrap_or_default();
        println!(
            "[{}/{}] {} — {}  |  pipeline: {}  |  {stats}",
            self.current + 1,
            self.demos.len(),
            demo.name,
            demo.blurb,
            demo.pipeline.name(),
        );
        if let Some(state) = self.state.as_ref() {
            state
                .window
                .set_title(&format!("wxsl gallery — {}", demo.name));
        }
    }

    /// Step to the demo at `index`, wrapping, and switch the renderer over.
    fn show(&mut self, index: usize) {
        let index = index % self.demos.len();
        if index == self.current {
            return;
        }
        self.current = index;
        if let Some(state) = self.state.as_mut() {
            if let Err(error) = apply(&self.demos[index], &mut state.stage.renderer) {
                eprintln!("cannot switch to `{}`: {error}", self.demos[index].name);
            }
        }
        self.announce();
    }

    fn render(&mut self) {
        let time = self.elapsed();
        let step = self
            .last_frame
            .replace(std::time::Instant::now())
            .map_or(1.0 / 60.0, |last| {
                last.elapsed().as_secs_f32().clamp(1.0 / 240.0, 0.1)
            });
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
            other => {
                if !matches!(
                    other,
                    wgpu::CurrentSurfaceTexture::Occluded | wgpu::CurrentSurfaceTexture::Timeout
                ) {
                    eprintln!("dropped a frame: {other:?}");
                }
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let size = state.window.inner_size();
        let demo = &self.demos[self.current];
        if let Err(error) = state.stage.render(
            demo,
            &view,
            size.width.max(1),
            size.height.max(1),
            time,
            step,
        ) {
            eprintln!("cannot render: {error}");
        }
        state.stage.gpu.queue.present(frame);
    }
}

fn configure_surface(state: &mut State, width: u32, height: u32) {
    let width = width.max(1);
    let height = height.max(1);
    state.surface.configure(
        &state.stage.gpu.device,
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
        .stage
        .renderer
        .resize(&state.stage.gpu.device, width, height);
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
                self.announce();
            }
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(state) = self.state.as_mut() {
                    configure_surface(state, size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state: ElementState::Pressed,
                        repeat: false,
                        ..
                    },
                ..
            } => self.on_key(code, event_loop),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = self.state.as_ref() {
            state.window.request_redraw();
        }
    }
}

impl App {
    fn create_state(&mut self, event_loop: &ActiveEventLoop) -> Result<State, Box<dyn Error>> {
        let (width, height) = self.options.size.unwrap_or((1280, 720));
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("wxsl gallery")
                    .with_inner_size(winit::dpi::LogicalSize::new(width, height)),
            )?,
        );

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(Arc::clone(&window))?;
        let gpu = pollster::block_on(GpuContext::new(instance, Some(&surface)))?;

        // A non-sRGB surface format: the shading function encodes sRGB
        // itself, and every effect downstream sees the frame's own
        // encoding.
        let capabilities = surface.get_capabilities(&gpu.adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
            .unwrap_or_else(|| {
                eprintln!("warning: no linear surface format available; colours will be bright");
                capabilities.formats[0]
            });

        let size = window.inner_size();
        let mut stage = Stage::new(
            gpu,
            TargetConfig::new(size.width, size.height, format),
            &self.options.ibl_source,
        )?;
        apply(&self.demos[self.current], &mut stage.renderer)?;
        let mut state = State {
            window,
            surface,
            stage,
            format,
        };
        configure_surface(&mut state, size.width, size.height);
        Ok(state)
    }

    fn on_key(&mut self, code: KeyCode, event_loop: &ActiveEventLoop) {
        match code {
            KeyCode::Escape | KeyCode::KeyQ => event_loop.exit(),
            KeyCode::ArrowRight | KeyCode::BracketRight => self.show(self.current + 1),
            KeyCode::ArrowLeft | KeyCode::BracketLeft => {
                self.show(self.current + self.demos.len() - 1);
            }
            KeyCode::Space => {
                self.paused_at = match self.paused_at {
                    // Resuming keeps the phase, so the cube does not jump.
                    Some(paused) => {
                        self.started = Instant::now() - std::time::Duration::from_secs_f32(paused);
                        None
                    }
                    None => Some(self.elapsed()),
                };
            }
            other => {
                let digit = match other {
                    KeyCode::Digit1 => Some(1),
                    KeyCode::Digit2 => Some(2),
                    KeyCode::Digit3 => Some(3),
                    KeyCode::Digit4 => Some(4),
                    KeyCode::Digit5 => Some(5),
                    KeyCode::Digit6 => Some(6),
                    KeyCode::Digit7 => Some(7),
                    KeyCode::Digit8 => Some(8),
                    KeyCode::Digit9 => Some(9),
                    _ => None,
                };
                if let Some(digit) = digit {
                    if digit <= self.demos.len() {
                        self.show(digit - 1);
                    }
                }
            }
        }
    }
}

fn run_windowed(options: Options, demos: Vec<Demo>, start: usize) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        options,
        demos,
        current: start,
        state: None,
        started: Instant::now(),
        paused_at: None,
        error: None,
        last_frame: None,
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_has_exact_independent_ranges_and_a_matte_grey_floor() {
        let samples = ibl_grid_samples();
        for (index, (position, roughness, metallic)) in samples.iter().enumerate() {
            assert_eq!(*roughness, (index % 5) as f32 / 4.0);
            assert_eq!(*metallic, (index / 5) as f32 / 4.0);
            assert_eq!(position.y, 0.85);
        }
        let registry = wxsl::stdlib::registry();
        let material =
            wxsl::render::Material::from_graph(&ibl_grid_material_graph(), &registry).unwrap();
        assert_eq!(material.interface().params.fields().len(), 2);
        assert!(
            material.interface().resources.is_empty(),
            "no texture aliasing in the diagnostic grid"
        );
        let floor = ibl_grid_floor_graph();
        floor.validate(&registry).unwrap();
        let output = floor
            .node(floor.outputs(&registry).unwrap().surface)
            .unwrap();
        assert_eq!(output.params["roughness"], Value::F32(1.0));
        assert_eq!(output.params["metallic"], Value::F32(0.0));
        assert_eq!(output.params["base_color"], Value::Vec3([0.3; 3]));
        let demo = demos()
            .into_iter()
            .find(|demo| demo.name == "ibl-grid")
            .unwrap();
        let environment = demo_environment(&demo, 4.0 / 3.0, 0.0);
        assert!(environment.lights.is_empty());
        assert_eq!(environment.ambient_sky, Vec3::ZERO);
        assert_eq!(environment.ambient_ground, Vec3::ZERO);
    }

    #[test]
    fn grid_is_stable_and_switching_sources_cannot_reuse_the_wrong_environment() {
        let gpu = match pollster::block_on(GpuContext::headless()) {
            Ok(gpu) => gpu,
            Err(error) => {
                eprintln!("skipping GPU test: {error}");
                return;
            }
        };
        let target = OffscreenTarget::new(&gpu.device, 192, 144);
        let mut stage =
            Stage::new(gpu, TargetConfig::new(192, 144, target.format()), "sky").unwrap();
        let demos = demos();
        let grid = demos.iter().find(|demo| demo.name == "ibl-grid").unwrap();
        let sky_grid = demos
            .iter()
            .find(|demo| demo.name == "ibl-grid-sky")
            .unwrap();
        // A small valid upload checks source-independent sky UVs without
        // depending on a user's local HDR file or its equirectangular extent.
        stage.ibl_source = wxsl::render::environment::upload_environment_image(
            &stage.gpu.device,
            &stage.gpu.queue,
            1,
            1,
            &[[1.5, 2.0, 3.0]],
        )
        .unwrap();
        stage.ibl_sky = false;
        let mut images = Vec::new();
        for demo in [grid, sky_grid, grid] {
            stage
                .render(demo, target.view(), 192, 144, 0.0, 0.0)
                .unwrap();
            stage.gpu.wait();
            let first = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
            assert_eq!(
                stage
                    .renderer
                    .render_graph()
                    .resource_by_label("gallery sky")
                    .is_some(),
                demo.name == "ibl-grid-sky"
            );
            assert_eq!(stage.renderer.render_graph().environment_scale(), 1.0);
            for ((_, roughness, metallic), bindings) in
                ibl_grid_samples().iter().zip(&stage.grid_bindings)
            {
                assert_eq!(bindings.get("roughness"), Some(Value::F32(*roughness)));
                assert_eq!(bindings.get("metallic"), Some(Value::F32(*metallic)));
            }
            stage
                .render(demo, target.view(), 192, 144, 30.0, 0.0)
                .unwrap();
            stage.gpu.wait();
            let second = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
            assert_eq!(
                first, second,
                "time must change neither material values nor environment"
            );
            assert!(
                mean_luminance(&first) > 0.02,
                "{} must illuminate the grid",
                demo.name
            );
            assert_eq!(
                stage.renderer.pass_run_count("gallery IBL specular 5/7"),
                Some(1)
            );
            if demo.name == "ibl-grid-sky" {
                assert_eq!(stage.renderer.pass_run_count("gallery sky"), Some(1));
            }
            images.push(first);
        }
        assert_ne!(
            images[0], images[1],
            "changing the source must change the light"
        );
        assert_eq!(
            images[0], images[2],
            "returning to the HDR must restore the same light"
        );
    }
}
