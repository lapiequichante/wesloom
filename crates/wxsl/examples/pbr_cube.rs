//! A PBR cube whose material is a node graph, rendered through either the
//! forward or the deferred path.
//!
//! The material is not written in this file: it is loaded from
//! `assets/pbr_cube.wxsl.json`, the node format, and compiled to WXSL at
//! startup. Nothing in the graph mentions forward or deferred — the same
//! graph drives both, because the render path is a property of the pipeline
//! and the difference is conditional translation inside one WXSL module
//! (ADR 0005).
//!
//! ```text
//! cargo run --example pbr_cube                       # windowed, forward
//! cargo run --example pbr_cube -- --path deferred    # windowed, deferred
//! cargo run --example pbr_cube -- --headless         # render both to PNG
//! cargo run --example pbr_cube -- --dump-wxsl        # what the graph became
//! cargo run --example pbr_cube -- --dump-wgsl --path deferred
//! cargo run --example pbr_cube -- --list-macros
//! cargo run --example pbr_cube -- --macro WXSL_FBM_OCTAVES=2
//! ```
//!
//! In the window: `F`/`D` switch path, `N` toggles the debug-normal view,
//! `T` the tonemap, `R` ridged noise, `Up`/`Down` change the noise octave
//! count, `Space` pauses the rotation, `Esc` quits. Every one of those but
//! `Space` changes which shader is compiled, and the status line shows the
//! variant cache absorbing it.

use std::borrow::Cow;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use glam::{Mat4, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};
use wxsl::core::abi;
use wxsl::core::codegen;
use wxsl::core::graph::Graph;
use wxsl::core::macros::{MacroSet, MacroValue};
use wxsl::core::node::NodeRegistry;
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::material::Material;
use wxsl::render::variants;
use wxsl::render::{
    Camera, DrawItem, DrawList, Environment, Light, Mesh, RenderPath, RenderRequest, Renderer,
    TargetConfig,
};

/// The graph used when `--graph` is not given.
const DEFAULT_GRAPH: &str = include_str!("../assets/pbr_cube.wxsl.json");

const USAGE: &str = "\
pbr_cube — a PBR cube whose material is a wxsl node graph

USAGE:
    cargo run --example pbr_cube -- [OPTIONS]

OPTIONS:
    --path <forward|deferred>  Render path to start in (default: forward)
    --graph <FILE>             Node-format graph to load (default: the shipped one)
    --macro <NAME=VALUE>       Override a macro variable; repeatable.
                               VALUE is true/false, an integer, or a decimal.
    --headless                 Render one frame per path to PNG and exit
    --out <DIR>                Where --headless writes (default: current directory)
    --size <WIDTHxHEIGHT>      Render size (default: 1280x720, or 800x600 headless)
    --instances <N>            Draw N copies of the cube in a row (default: 1)
    --dump-wxsl                Print the WXSL generated from the graph and exit
    --dump-wgsl                Print the WGSL the active path compiles to and exit
    --list-nodes               List the node library and exit
    --list-macros              List the macro variables in effect and exit
    -h, --help                 Print this help

KEYS (windowed):
    F / D          forward / deferred path
    N              toggle the debug-normal view
    T              toggle the tonemap
    R              toggle ridged noise
    Up / Down      noise octaves +/- 1
    Space          pause rotation
    Esc or Q       quit
";

fn main() -> Result<(), Box<dyn Error>> {
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if options.help {
        print!("{USAGE}");
        return Ok(());
    }

    let registry = wxsl::stdlib::registry();
    let graph = load_graph(options.graph.as_deref())?;
    graph.validate(&registry)?;

    if options.list_nodes {
        list_nodes(&registry);
        return Ok(());
    }

    let material = Material::from_graph_with_macros(&graph, &registry, &options.macros)?;

    if options.list_macros {
        list_macros(&graph, &registry, &material);
        return Ok(());
    }
    if options.dump_wxsl {
        print!("{}", material.wxsl());
        return Ok(());
    }
    if options.dump_wgsl {
        print!("{}", dump_wgsl(&material, options.path)?);
        return Ok(());
    }

    if options.headless {
        return run_headless(&options, &graph, &material);
    }
    run_windowed(options, graph, registry, material)
}

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

struct Options {
    path: RenderPath,
    graph: Option<PathBuf>,
    macros: MacroSet,
    headless: bool,
    out_dir: PathBuf,
    size: Option<(u32, u32)>,
    instances: u32,
    dump_wxsl: bool,
    dump_wgsl: bool,
    list_nodes: bool,
    list_macros: bool,
    help: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            path: RenderPath::Forward,
            graph: None,
            macros: MacroSet::new(),
            headless: false,
            out_dir: PathBuf::from("."),
            size: None,
            instances: 1,
            dump_wxsl: false,
            dump_wgsl: false,
            list_nodes: false,
            list_macros: false,
            help: false,
        }
    }
}

impl Options {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Options::default();
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("`{arg}` needs a value"));
            match arg.as_str() {
                "-h" | "--help" => options.help = true,
                "--headless" => options.headless = true,
                "--dump-wxsl" => options.dump_wxsl = true,
                "--dump-wgsl" => options.dump_wgsl = true,
                "--list-nodes" => options.list_nodes = true,
                "--list-macros" => options.list_macros = true,
                "--path" => {
                    let text = value()?;
                    options.path = RenderPath::parse(&text)
                        .ok_or_else(|| format!("unknown render path `{text}`"))?;
                }
                "--graph" => options.graph = Some(PathBuf::from(value()?)),
                "--instances" => {
                    let text = value()?;
                    options.instances = text
                        .trim()
                        .parse()
                        .map_err(|_| format!("`--instances` wants a number, got `{text}`"))?;
                }
                "--out" => options.out_dir = PathBuf::from(value()?),
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
                "--macro" => {
                    let text = value()?;
                    let (name, raw) = text
                        .split_once('=')
                        .ok_or_else(|| format!("`--macro` wants NAME=VALUE, got `{text}`"))?;
                    let parsed = MacroValue::parse(raw)
                        .ok_or_else(|| format!("cannot read `{raw}` as a macro value"))?;
                    options.macros.set(name.trim().to_string(), parsed);
                }
                other => return Err(format!("unexpected argument `{other}`")),
            }
        }
        Ok(options)
    }
}

fn load_graph(path: Option<&std::path::Path>) -> Result<Graph, Box<dyn Error>> {
    let json = match path {
        Some(path) => Cow::Owned(std::fs::read_to_string(path)?),
        None => Cow::Borrowed(DEFAULT_GRAPH),
    };
    Ok(serde_json::from_str(&json)?)
}

// ---------------------------------------------------------------------------
// Inspection
// ---------------------------------------------------------------------------

fn list_nodes(registry: &NodeRegistry) {
    println!("{} nodes in the library:\n", registry.len());
    for category in registry.categories() {
        println!("{category}:");
        for def in registry.iter().filter(|def| def.category == category) {
            let inputs = def
                .inputs
                .iter()
                .map(|socket| format!("{}: {}", socket.name, socket.ty))
                .collect::<Vec<_>>()
                .join(", ");
            let outputs = def
                .outputs
                .iter()
                .map(|socket| format!("{}: {}", socket.name, socket.ty))
                .collect::<Vec<_>>()
                .join(", ");
            println!("  {:<34} ({inputs}) -> ({outputs})", def.id);
        }
        println!();
    }
}

fn list_macros(graph: &Graph, registry: &NodeRegistry, material: &Material) {
    let (declared, _) = graph.declared_macros(registry);
    println!("Macro variables in effect for this material:\n");
    println!("  {:<26} {:<9} {:<8} DECLARED BY", "NAME", "VALUE", "KIND");
    for (name, value) in material.macros().iter() {
        let (kind, source) = match declared.get(name) {
            Some(decl) => (decl.kind(), "a node in the graph"),
            None if abi::abi_macros()
                .iter()
                .any(|decl| decl.name.as_str() == name) =>
            {
                (value.kind(), "the shader ABI")
            }
            None => (value.kind(), "the graph only"),
        };
        println!("  {name:<26} {:<9} {kind:<8} {source}", value.to_string());
    }
    println!(
        "\nSet one with --macro NAME=VALUE, or pin it in the graph's `macros` object.\n\
         Flags become @if conditions; numbers become const declarations."
    );
}

fn dump_wgsl(material: &Material, path: RenderPath) -> Result<String, Box<dyn Error>> {
    let library = wxsl::stdlib_library();
    let mut macros = material.macros().clone();
    path.apply_to(&mut macros);
    // The generated module declares its own macros now, so the material
    // source is the only thing to mount alongside the library (ADR 0011).
    let extra = [(
        codegen::MATERIAL_MODULE,
        Cow::Borrowed(material.shader.source.as_str()),
    )];
    Ok(variants::compile(
        &library,
        &extra,
        codegen::MATERIAL_MODULE,
        &macros,
    )?)
}

// ---------------------------------------------------------------------------
// The scene
// ---------------------------------------------------------------------------

fn demo_environment(aspect: f32, time: f32) -> Environment {
    Environment {
        camera: Camera {
            eye: Vec3::new(2.4, 1.9, 3.2),
            target: Vec3::ZERO,
            aspect,
            ..Camera::default()
        },
        lights: vec![
            // Key light: warm, close, so its inverse-square falloff is
            // visible across the cube's faces.
            Light::point(Vec3::new(2.6, 3.0, 2.2), Vec3::new(1.0, 0.86, 0.72), 42.0),
            // Fill: cool, from the other side, dimmer.
            Light::point(Vec3::new(-3.0, 1.2, -1.6), Vec3::new(0.5, 0.65, 1.0), 18.0),
            // Rim: directional, so it does not fall off with distance.
            Light::directional(Vec3::new(-0.4, 0.7, -1.0), Vec3::new(0.7, 0.75, 0.9), 1.1),
        ],
        ambient_sky: Vec3::new(0.14, 0.19, 0.28),
        ambient_ground: Vec3::new(0.05, 0.04, 0.035),
        exposure: 1.0,
        time,
    }
}

fn cube_transform(time: f32) -> Mat4 {
    Mat4::from_rotation_y(time * 0.45) * Mat4::from_rotation_x(time * 0.21)
}

/// The frame's draws: `count` copies of the cube, spread along x.
///
/// One by default, which is what every image in the README is. More than
/// one is the shortest demonstration that the renderer takes a draw *list*:
/// every copy is a row of the frame's instance storage buffer, and one
/// upload serves all of them (ADR 0021).
fn cube_draws<'a>(mesh: &'a Mesh, material: &'a Material, count: u32, time: f32) -> DrawList<'a> {
    let spin = cube_transform(time);
    (0..count.max(1))
        .map(|index| {
            let offset = index as f32 - (count.max(1) - 1) as f32 * 0.5;
            let place = Mat4::from_translation(Vec3::new(offset * 2.4, 0.0, 0.0));
            DrawItem::new(mesh, material).with_transform(place * spin)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Headless
// ---------------------------------------------------------------------------

fn run_headless(
    options: &Options,
    graph: &Graph,
    material: &Material,
) -> Result<(), Box<dyn Error>> {
    let (width, height) = options.size.unwrap_or((800, 600));
    let gpu = pollster::block_on(GpuContext::headless())?;
    println!("adapter: {}", gpu.adapter.get_info().name);
    println!(
        "graph:   {} ({} nodes)\nmacros:  {}",
        graph.name(),
        graph.node_count(),
        material.macros().signature()
    );

    let target = OffscreenTarget::new(&gpu.device, width, height);
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(width, height, target.format()),
    )?;
    let mesh = Mesh::cube(&gpu.device, 1.6);
    // A fixed time, so two runs produce identical images.
    let environment = demo_environment(width as f32 / height as f32, 1.0);
    let draws = cube_draws(&mesh, material, options.instances, 0.6);

    std::fs::create_dir_all(&options.out_dir)?;
    let mut images = Vec::new();
    for path in RenderPath::ALL {
        renderer.set_path(*path);
        renderer.render(
            &gpu.device,
            &gpu.queue,
            &RenderRequest {
                view: target.view(),
                environment: &environment,
                draws: &draws,
            },
        )?;
        gpu.wait();

        let pixels = target.read_rgba8(&gpu.device, &gpu.queue);
        let file = options.out_dir.join(format!("pbr_cube_{path}.png"));
        image::save_buffer(
            &file,
            &pixels,
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )?;
        println!(
            "{path:>8}: {} covered pixels, mean luminance {:.4} -> {}",
            covered_pixels(&pixels),
            mean_luminance(&pixels),
            file.display()
        );
        images.push(pixels);
    }

    // The two paths run the same material graph through the same shading
    // function, so they should agree to within rounding and the G-buffer's
    // 8-bit base colour.
    let difference = mean_difference(&images[0], &images[1]);
    println!(
        "\nmean |forward - deferred| = {difference:.4} (0 = identical, 1 = opposite)\n\
         shader variants compiled: {} ({:?})",
        renderer.variant_count(),
        renderer.cache_stats()
    );
    Ok(())
}

/// Pixels that are not the clear colour, i.e. where the cube is.
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

fn mean_difference(a: &[u8], b: &[u8]) -> f32 {
    let sum: f32 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (*x as f32 - *y as f32).abs() / 255.0)
        .sum();
    sum / a.len() as f32
}

// ---------------------------------------------------------------------------
// Windowed
// ---------------------------------------------------------------------------

fn run_windowed(
    options: Options,
    graph: Graph,
    registry: NodeRegistry,
    material: Material,
) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        options,
        graph,
        registry,
        material,
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

/// Everything that only exists once there is a window.
struct State {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    gpu: GpuContext,
    renderer: Renderer,
    mesh: Mesh,
    format: wgpu::TextureFormat,
}

struct App {
    options: Options,
    graph: Graph,
    registry: NodeRegistry,
    material: Material,
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

    /// Recompile the material after a macro change and report what happened.
    fn rebuild_material(&mut self) {
        match Material::from_graph_with_macros(&self.graph, &self.registry, &self.options.macros) {
            Ok(material) => self.material = material,
            Err(error) => eprintln!("cannot recompile the material: {error}"),
        }
        self.report();
    }

    fn report(&self) {
        let variants = self
            .state
            .as_ref()
            .map(|state| {
                let stats = state.renderer.cache_stats();
                format!(
                    "{} variants, {} hits, {} compiles",
                    state.renderer.variant_count(),
                    stats.hits,
                    stats.misses
                )
            })
            .unwrap_or_default();
        println!(
            "[{}] {}  |  {variants}",
            self.options.path,
            self.material.macros().signature()
        );
    }

    /// Flip a flag macro, or set it if the graph never mentioned it.
    fn toggle_flag(&mut self, name: &str) {
        let current = self
            .options
            .macros
            .get(name)
            .or_else(|| self.material.macros().get(name))
            .and_then(|value| value.as_flag())
            .unwrap_or(false);
        self.options
            .macros
            .set(name.to_string(), MacroValue::Flag(!current));
        self.rebuild_material();
    }

    fn adjust_octaves(&mut self, delta: i32) {
        let name = "WXSL_FBM_OCTAVES";
        let current = match self
            .options
            .macros
            .get(name)
            .or_else(|| self.material.macros().get(name))
        {
            Some(MacroValue::Int(value)) => value,
            _ => {
                eprintln!("this graph has no {name} macro");
                return;
            }
        };
        let next = (current + delta).clamp(1, 8);
        if next == current {
            return;
        }
        self.options
            .macros
            .set(name.to_string(), MacroValue::Int(next));
        self.rebuild_material();
    }

    fn render(&mut self) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let frame = match state.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            // Lost or outdated: reconfiguring is the normal response, and the
            // next frame succeeds.
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                let size = state.window.inner_size();
                configure_surface(state, size.width, size.height);
                return;
            }
            // Occluded or timed out: skip this frame and try again.
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
        let time = self
            .paused_at
            .unwrap_or_else(|| self.started.elapsed().as_secs_f32());
        let environment = demo_environment(size.width as f32 / size.height.max(1) as f32, time);
        let draws = cube_draws(&state.mesh, &self.material, self.options.instances, time);

        if let Err(error) = state.renderer.render(
            &state.gpu.device,
            &state.gpu.queue,
            &RenderRequest {
                view: &view,
                environment: &environment,
                draws: &draws,
            },
        ) {
            eprintln!("cannot render: {error}");
        }
        state.gpu.queue.present(frame);
    }
}

fn configure_surface(state: &mut State, width: u32, height: u32) {
    let width = width.max(1);
    let height = height.max(1);
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
                self.report();
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
                    .with_title("wxsl — PBR cube")
                    .with_inner_size(winit::dpi::LogicalSize::new(width, height)),
            )?,
        );

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(Arc::clone(&window))?;
        let gpu = pollster::block_on(GpuContext::new(instance, Some(&surface)))?;

        // A non-sRGB format: the shading function encodes sRGB itself so that
        // the forward path and the deferred lighting pass agree exactly. With
        // an `*Srgb` surface the GPU would encode a second time.
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
        let renderer = Renderer::new(
            &gpu.device,
            wxsl::stdlib_library(),
            TargetConfig::new(size.width, size.height, format),
        )?;
        let mesh = Mesh::cube(&gpu.device, 1.6);

        let mut state = State {
            window,
            surface,
            gpu,
            renderer,
            mesh,
            format,
        };
        state.renderer.set_path(self.options.path);
        configure_surface(&mut state, size.width, size.height);
        println!("adapter: {}", state.gpu.adapter.get_info().name);
        Ok(state)
    }

    fn on_key(&mut self, code: KeyCode, event_loop: &ActiveEventLoop) {
        match code {
            KeyCode::Escape | KeyCode::KeyQ => event_loop.exit(),
            KeyCode::KeyF | KeyCode::KeyD => {
                let path = if code == KeyCode::KeyF {
                    RenderPath::Forward
                } else {
                    RenderPath::Deferred
                };
                self.options.path = path;
                if let Some(state) = self.state.as_mut() {
                    state.renderer.set_path(path);
                }
                self.report();
            }
            KeyCode::KeyN => self.toggle_flag(abi::FEATURE_DEBUG_NORMALS),
            KeyCode::KeyT => self.toggle_flag(abi::FEATURE_TONEMAP),
            KeyCode::KeyR => self.toggle_flag("wxsl_fbm_ridged"),
            KeyCode::ArrowUp => self.adjust_octaves(1),
            KeyCode::ArrowDown => self.adjust_octaves(-1),
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
            _ => {}
        }
    }
}
