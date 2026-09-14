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
//! feature's channel in its G-buffer. The last, `ibl`, is ADR 0039's: no
//! lamps at all, so what is in the image is `ambient_environment` reading
//! the split-sum table the `brdf_lut` bake leaves in the frame group.
//! Nothing below touches
//! `wxsl-render`'s source — the bloom pipeline is the shipped deferred
//! preset's document with two nodes added and one rewired, compiled by
//! the same public `compile_pipeline` an application would call, and the
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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use glam::{Mat4, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};
use wxsl::core::graph::Graph;
use wxsl::core::graph::NodeId;
use wxsl::core::node::Value;
use wxsl::core::pipeline as doc;
use wxsl::render::effect::{BRDF_LUT, LUT_VIEW, RAMP_FILL, RAMP_VIEW};
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::{
    compile_pipeline, Attachment, Camera, DrawItem, DrawList, Environment, Extent,
    InstanceAttributes, Light, Mesh, PassDesc, PipelineConfig, Policy, Read, RenderGraph,
    RenderRequest, Renderer, ResourceDesc, StockPipeline, TargetConfig,
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
    --list                 List the demos and exit
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
        Some(dir) => run_screenshots(options.size, &demos, &dir),
        None => run_windowed(options, demos, start.unwrap_or(0)),
    }
}

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

struct Options {
    screenshot: Option<PathBuf>,
    size: Option<(u32, u32)>,
    start: Option<String>,
    list: bool,
    help: bool,
}

impl Options {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Options {
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

/// Where a demo's pass list comes from: a stock preset, a document built
/// here and compiled by the public pipeline compiler, or a hand-built
/// pass list for the proofs that name effects no document node can wire
/// yet (compute; ADR 0036's note on `pass.compute`). The hand-built
/// builders take the frame target's format, as the compiler does — a
/// window's surface is `Bgra8Unorm` where an offscreen target is
/// `Rgba8Unorm`, and the graph has to agree with whichever it is handed.
enum Pipeline {
    Stock(StockPipeline),
    Document(fn() -> Graph),
    Graph(fn(wgpu::TextureFormat) -> RenderGraph),
}

impl Pipeline {
    /// The pipeline's name, for titles and status lines.
    fn name(&self) -> String {
        match self {
            Pipeline::Stock(stock) => stock.name().to_string(),
            Pipeline::Document(build) => build().name().to_string(),
            Pipeline::Graph(_) => "hand-built pass list".to_string(),
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
    /// A sky to light the demo by instead of the lights: sky above,
    /// bounce below, with the lamps switched off. What is left is
    /// `ambient_environment` alone, and therefore the environment-BRDF
    /// table it reads (ADR 0039).
    sky: Option<(Vec3, Vec3)>,
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
            sky: None,
        },
        Demo {
            name: "deferred",
            blurb: "the stock deferred pipeline: G-buffer, then a lighting pass",
            pipeline: Pipeline::Stock(StockPipeline::Deferred),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            sky: None,
        },
        Demo {
            name: "single-pass",
            blurb: "the minimal forward document: one pass, its own depth, no shadows",
            pipeline: Pipeline::Document(minimal_forward_document),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            sky: None,
        },
        Demo {
            name: "deferred-bloom",
            blurb: "deferred lighting into a colour target, bloom over it — an effect \
                    chain as a document edit",
            pipeline: Pipeline::Document(deferred_bloom_document),
            instances: 1,
            // A highlight bright enough to cross bloom's threshold.
            key_intensity: 160.0,
            features: &[],
            sky: None,
        },
        Demo {
            name: "bloom-instances",
            blurb: "the same chain, six tinted copies from one draw list",
            pipeline: Pipeline::Document(deferred_bloom_document),
            instances: 6,
            key_intensity: 160.0,
            features: &[],
            sky: None,
        },
        Demo {
            name: "brdf-lut",
            blurb: "a compute effect bakes the split-sum BRDF LUT once (policy: once), \
                    and a per-frame view displays it — the execution-policy proof",
            pipeline: Pipeline::Graph(brdf_lut_graph),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            sky: None,
        },
        Demo {
            name: "buffer-ramp",
            blurb: "a compute effect fills a storage buffer and a screen effect reads \
                    it as storage — buffers are graph resources",
            pipeline: Pipeline::Graph(buffer_ramp_graph),
            instances: 1,
            key_intensity: 42.0,
            features: &[],
            sky: None,
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
            sky: None,
        },
        Demo {
            name: "ibl",
            blurb: "no lamps at all: a sky, a bounce, and the split-sum BRDF table                     the `brdf_lut` bake leaves in the frame group",
            pipeline: Pipeline::Stock(StockPipeline::Forward),
            instances: 1,
            key_intensity: 0.0,
            features: &[],
            // Bright enough to be the whole scene, and blue enough that the
            // specular response is visibly the *sky* rather than the
            // surface's own colour.
            sky: Some((Vec3::new(0.55, 0.72, 1.05), Vec3::new(0.18, 0.14, 0.1))),
        },
    ]
}

/// The execution-policy proof as a pass list (ADR 0035): a compute effect
/// bakes the split-sum environment-BRDF LUT once into a stable target, and
/// a screen effect displays that target every frame. Hand-built, because a
/// compute pass has no document node yet.
fn brdf_lut_graph(format: wgpu::TextureFormat) -> RenderGraph {
    let mut graph = RenderGraph::new(format);
    let lut = graph.resource(
        ResourceDesc::color("brdf lut", wgpu::TextureFormat::Rgba16Float)
            .with_extent(Extent::Fixed {
                width: 64,
                height: 64,
            })
            .with_usage(wgpu::TextureUsages::TEXTURE_BINDING)
            .persistent(0),
    );
    graph.pass(
        PassDesc::compute("lut bake", "brdf_lut")
            .with_write(lut)
            .with_policy(Policy::Once),
    );
    graph.pass(
        PassDesc::screen("lut view", "lut_view")
            .with_color(Attachment::clear(RenderGraph::TARGET, wgpu::Color::BLACK))
            .with_reads([Read::current(lut)]),
    );
    graph
}

/// The buffer proof as a pass list (ADR 0036): a compute effect fills a
/// 256-entry storage buffer, a screen effect reads it as storage and draws
/// it, one value per column.
fn buffer_ramp_graph(format: wgpu::TextureFormat) -> RenderGraph {
    let mut graph = RenderGraph::new(format);
    let ramp = graph.resource(ResourceDesc::buffer("ramp", 256 * 4));
    graph.pass(PassDesc::compute("fill ramp", "ramp_fill").with_write(ramp));
    graph.pass(
        PassDesc::screen("show ramp", "ramp_view")
            .with_color(Attachment::clear(RenderGraph::TARGET, wgpu::Color::BLACK))
            .with_reads([Read::current(ramp)]),
    );
    graph
}

/// The minimal pipeline there is: a scene, one lit material pass with a
/// depth target of its own, present. Three nodes and a resource — what a
/// pipeline document looks like with everything optional left out.
fn minimal_forward_document() -> Graph {
    let registry = wxsl::core::pipeline::registry();
    let mut graph = Graph::new("single pass");
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
    let registry = wxsl::core::pipeline::registry();
    let hdr = graph.add(
        wxsl::core::graph::Node::new(doc::RESOURCE_COLOR)
            .with_label("linear")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let tonemap = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("tonemap")
            .with_setting(doc::SETTING_EFFECT, "tonemap"),
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

/// The shipped deferred preset with a bloom chain composed onto it: the
/// lighting pass writes a `resource.color` instead of the frame's target,
/// a `pass.screen` node running `bloom` reads it, and its `into` stays
/// unconnected so *it* writes the frame's target. Two nodes and a rewire
/// — the composition that used to be hand-written Rust.
fn deferred_bloom_document() -> Graph {
    let registry = wxsl::core::pipeline::registry();
    let mut graph = Graph::new("deferred bloom");
    let scene = graph.add_node(doc::SOURCE_SCENE);
    let lights = graph.add_node(doc::SOURCE_LIGHTS);
    let shadows = graph.add_node(doc::PASS_SHADOW);
    let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
    let material = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_GEOMETRY)
            .with_label("deferred material")
            .with_setting(doc::SETTING_STAGE, "gbuffer"),
    );
    let scene_color =
        graph.add(wxsl::core::graph::Node::new(doc::RESOURCE_COLOR).with_label("scene"));
    let lighting =
        graph.add(wxsl::core::graph::Node::new(doc::PASS_SCREEN).with_label("deferred lighting"));
    let bloom = graph.add(
        wxsl::core::graph::Node::new(doc::PASS_SCREEN)
            .with_label("bloom")
            .with_setting(doc::SETTING_EFFECT, "bloom"),
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
    wire((scene_color, "color"), (bloom, "image"));
    wire((bloom, "color"), (present, "surface"));
    // Bloom thresholds *linear* radiance, so it belongs before the display
    // transform — which is the physically correct order, and the one the
    // move in ADR 0039 made expressible.
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
    match &demo.pipeline {
        Pipeline::Stock(stock) => {
            renderer.set_pipeline(*stock);
            Ok(())
        }
        Pipeline::Document(build) => {
            let document = build();
            let graph = compile_pipeline(
                &document,
                &wxsl::core::pipeline::registry(),
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
            Ok(())
        }
        Pipeline::Graph(build) => {
            renderer
                .set_graph(build(renderer.target().format))
                .map_err(|error| format!("the `{}` pass list does not run: {error}", demo.name))?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
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
        camera: Camera {
            eye: Vec3::new(2.4, 1.9, 3.2),
            target: Vec3::ZERO,
            aspect,
            ..Camera::default()
        },
        lights,
        ambient_sky,
        ambient_ground,
        exposure: 1.0,
        time,
        previous_time: time,
    }
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

fn cube_transform(time: f32) -> Mat4 {
    Mat4::from_rotation_y(time * 0.45) * Mat4::from_rotation_x(time * 0.21)
}

fn cube_draws<'a>(
    mesh: &'a Mesh,
    material: &'a wxsl::render::Material,
    bindings: &'a wxsl::render::MaterialBindings,
    tints: &'a [InstanceAttributes],
    count: u32,
    time: f32,
) -> DrawList<'a> {
    let spin = cube_transform(time);
    (0..count.max(1))
        .map(|index| {
            let offset = index as f32 - (count.max(1) - 1) as f32 * 0.5;
            let place = Mat4::from_translation(Vec3::new(offset * 2.4, 0.0, 0.0));
            let mut item = DrawItem::new(mesh, material)
                .with_transform(place * spin)
                .with_bindings(bindings);
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
}

impl Stage {
    fn new(gpu: GpuContext, target: TargetConfig) -> Result<Self, Box<dyn Error>> {
        let registry = wxsl::stdlib::registry();
        let scene_graph: Graph =
            serde_json::from_str(include_str!("../assets/pbr_cube.wxsl.json"))?;
        scene_graph.validate(&registry)?;
        let material = wxsl::render::Material::from_graph(&scene_graph, &registry)?;
        let mut renderer = Renderer::new(&gpu.device, wxsl::stdlib_library(), target)?;
        // The proof effects (ADRs 0035–0037) ship as descriptors; an
        // application registers them, which is the whole of "add a
        // compute pass" now.
        renderer.add_effect(BRDF_LUT);
        renderer.add_effect(LUT_VIEW);
        renderer.add_effect(RAMP_FILL);
        renderer.add_effect(RAMP_VIEW);
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
        Ok(Stage {
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
        })
    }

    /// Put the scene's material on `features`' plan, if the demo's
    /// differs from the current one (plan2 P12). A material is resolved
    /// against the plan of the pipeline it will draw under — that is the
    /// handshake the frame compile checks.
    fn ensure_material(&mut self, features: &'static [&'static str]) -> Result<(), Box<dyn Error>> {
        if self.material_features == features {
            return Ok(());
        }
        let registry = wxsl::stdlib::registry();
        let material = wxsl::render::Material::with_lighting(
            &self.scene_graph,
            &registry,
            &wxsl::render::material::MaterialConfig {
                features: wxsl::core::lighting::feature_requests(features)?,
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
        Ok(())
    }

    /// Render one frame of `demo` at `time`, into `view`.
    fn render(
        &mut self,
        demo: &Demo,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        time: f32,
    ) -> Result<(), Box<dyn Error>> {
        apply(demo, &mut self.renderer)?;
        self.ensure_material(demo.features)?;
        self.tints = instance_tints(demo.instances);
        let environment = demo_environment(demo, width as f32 / height.max(1) as f32, time);
        let draws = cube_draws(
            &self.mesh,
            &self.material,
            &self.bindings,
            &self.tints,
            demo.instances,
            time,
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
) -> Result<(), Box<dyn Error>> {
    let (width, height) = size.unwrap_or((800, 600));
    let gpu = pollster::block_on(GpuContext::headless())?;
    println!("adapter: {}", gpu.adapter.get_info().name);
    let target = OffscreenTarget::new(&gpu.device, width, height);
    let mut stage = Stage::new(gpu, TargetConfig::new(width, height, target.format()))?;

    std::fs::create_dir_all(dir)?;
    let mut shots: Vec<Shot> = Vec::new();
    for demo in demos {
        // A fixed time, so two runs produce identical images.
        stage.render(demo, target.view(), width, height, 0.6)?;
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
        if let Err(error) =
            state
                .stage
                .render(demo, &view, size.width.max(1), size.height.max(1), time)
        {
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
        let mut stage = Stage::new(gpu, TargetConfig::new(size.width, size.height, format))?;
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
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
