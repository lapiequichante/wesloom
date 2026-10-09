//! The new shader library, in one picture: a molten torus through the
//! full post chain.
//!
//! The material is built entirely from the batches plan4's S2/S3 landed:
//! Worley cracks glow through an HDR emissive, a simplex gradient
//! wrinkles the normal, `fbm3` displaces the vertices (so the shadow
//! passes see the same molten surface), and the pipeline document chains
//! the new screen effects over the lit frame — separable bloom, chromatic
//! aberration, vignette and film grain, all in linear radiance, then the
//! demo's own ACES display transform, a graph effect built here from
//! `color.tonemap_aces` and registered the way any application registers
//! one.
//!
//! ```text
//! cargo run -p wxsl --example cinema                        # the windowed demo
//! cargo run -p wxsl --example cinema -- --screenshot cinema.png
//! cargo run -p wxsl --example cinema -- --frames 12 --out spin/
//!     # one PNG per twelfth of a turn, for a GIF or a flipbook
//! ```
//!
//! Windowed by default; `--screenshot` renders headless instead — one
//! reviewable PNG per frame, the thing the screenshot tests run on.

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
use wxsl::core::graph::{Graph, Node, NodeId};
use wxsl::core::node::{Value, ValueType};
use wxsl::core::pipeline as doc;
use wxsl::render::draw::{DrawItem, DrawList};
use wxsl::render::effect::{Effect, EffectRegistry};
use wxsl::render::environment::{Camera, Environment, Light};
use wxsl::render::gpu::{GpuContext, OffscreenTarget};
use wxsl::render::mesh::Mesh;
use wxsl::render::pipeline::TargetConfig;
use wxsl::render::renderer::{RenderRequest, Renderer};

fn main() -> Result<(), Box<dyn Error>> {
    let mut screenshot = None;
    let mut out_dir = PathBuf::from(".");
    let mut frames = 1usize;
    let mut size = (1280, 720);
    let mut args = std::env::args().skip(1).peekable();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--screenshot" => screenshot = Some(args.next().ok_or("--screenshot needs a path")?),
            "--out" => out_dir = args.next().ok_or("--out needs a directory")?.into(),
            "--frames" => frames = args.next().ok_or("--frames needs a count")?.parse()?,
            "--size" => {
                let specified = args.next().ok_or("--size needs WIDTHxHEIGHT")?;
                let (w, h) = specified
                    .split_once('x')
                    .ok_or("--size wants WIDTHxHEIGHT")?;
                size = (w.parse()?, h.parse()?);
            }
            other => return Err(format!("unknown argument `{other}`").into()),
        }
    }

    match screenshot {
        Some(path) => run_headless(path, out_dir, frames, size),
        None => run_windowed(size),
    }
}

/// Everything a frame needs, shared by the headless and windowed paths:
/// the renderer with its registered effects and compiled pass list, the
/// torus, its material, and the material's (empty) bindings.
struct Stage {
    renderer: Renderer,
    mesh: Mesh,
    material: wxsl::render::Material,
    bindings: wxsl::render::MaterialBindings,
}

impl Stage {
    fn build(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Result<Self, Box<dyn Error>> {
        let nodes = wxsl::stdlib::registry();
        let graph = lava_graph();
        graph.validate(&nodes)?;
        let material = wxsl::render::Material::from_graph(&graph, &nodes)?;

        let mut renderer = Renderer::new(
            device,
            wxsl::stdlib_library(),
            TargetConfig::new(width, height, format),
        )?;
        // The stock documents name these; the application registers them.
        renderer.add_effect(wxsl::render::effect::BRDF_LUT);
        let shipped = wxsl::effects::registry(&nodes)?;
        for effect in shipped.iter() {
            renderer.add_effect(effect.clone());
        }
        // The demo's own display transform: ACES instead of the stock
        // filmic curve, authored as a screen graph over
        // `color.tonemap_aces` — the same move `wxsl::effects::tonemap`
        // makes, with a different curve.
        renderer.add_effect(aces_effect(&nodes)?);

        let document = cinema_document();
        let compiled = wxsl::render::compile_pipeline(
            &document,
            &wxsl::render::document_registry(renderer.effects()),
            renderer.effects(),
            &wxsl::render::PipelineConfig::new(TargetConfig::new(width, height, format)),
        )
        .map_err(|error| format!("the cinema document does not compile: {error}"))?;
        renderer
            .set_graph(compiled)
            .map_err(|error| format!("the cinema pass list does not run: {error}"))?;

        let mesh = Mesh::torus(device, 1.0, 0.42);
        let mut bindings = renderer.material_bindings(device, &material);
        bindings.upload(device, queue)?; // no declared resources; an empty upload
        Ok(Self {
            renderer,
            mesh,
            material,
            bindings,
        })
    }
}

/// The tilted, slowly turning torus. A free function over the stage's
/// fields, so a caller can hold the renderer `&mut` while the draws
/// borrow the rest.
fn cinema_draws<'a>(
    mesh: &'a Mesh,
    material: &'a wxsl::render::Material,
    bindings: &'a wxsl::render::MaterialBindings,
    time: f32,
) -> DrawList<'a> {
    let turn = Mat4::from_rotation_y(time * 0.5);
    let tilt = Mat4::from_rotation_x(-0.85);
    [DrawItem::new(mesh, material)
        .with_transform(tilt * turn)
        .with_bindings(bindings)]
    .into_iter()
    .collect()
}

fn run_headless(
    screenshot: String,
    out_dir: PathBuf,
    frames: usize,
    size: (u32, u32),
) -> Result<(), Box<dyn Error>> {
    let (width, height) = size;
    let gpu = pollster::block_on(GpuContext::headless())?;
    println!("adapter: {}", gpu.adapter.get_info().name);

    let target = OffscreenTarget::new(&gpu.device, width, height);
    let mut stage = Stage::build(&gpu.device, &gpu.queue, target.format(), width, height)?;

    std::fs::create_dir_all(&out_dir)?;
    for frame in 0..frames {
        // A fixed step per frame, so two runs produce identical images.
        let time = frame as f32 / frames as f32 * 6.0;
        let draws = cinema_draws(&stage.mesh, &stage.material, &stage.bindings, time);
        let environment = cinema_environment(width as f32 / height as f32, time);
        stage.renderer.render(
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
        let name = if frames == 1 {
            screenshot.clone()
        } else {
            format!("cinema_{frame:02}.png")
        };
        let file = out_dir.join(name);
        image::save_buffer(
            &file,
            &pixels,
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )?;
        println!("frame {frame}: -> {}", file.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// The usage line the window prints when it opens.
const USAGE: &str = "keys: Esc/Q quit · Space pause";

fn run_windowed(size: (u32, u32)) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        size,
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
struct WindowState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    gpu: GpuContext,
    stage: Stage,
    format: wgpu::TextureFormat,
}

struct App {
    size: (u32, u32),
    state: Option<WindowState>,
    started: Instant,
    paused_at: Option<f32>,
    error: Option<Box<dyn Error>>,
}

impl App {
    fn elapsed(&self) -> f32 {
        self.paused_at
            .unwrap_or_else(|| self.started.elapsed().as_secs_f32())
    }

    fn render(&mut self) {
        let time = self.elapsed();
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let frame = match state.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            // Lost or outdated: reconfiguring is the normal response, and
            // the next frame succeeds.
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
        let stage = &mut state.stage;
        let Stage {
            renderer,
            mesh,
            material,
            bindings,
        } = stage;
        let draws = cinema_draws(mesh, material, bindings, time);
        let environment = cinema_environment(size.width as f32 / size.height.max(1) as f32, time);
        if let Err(error) = renderer.render(
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

fn configure_surface(state: &mut WindowState, width: u32, height: u32) {
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
    state
        .stage
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
    fn create_state(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) -> Result<WindowState, Box<dyn Error>> {
        let (width, height) = self.size;
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("wxsl — cinema")
                    .with_inner_size(winit::dpi::LogicalSize::new(width, height)),
            )?,
        );

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(Arc::clone(&window))?;
        let gpu = pollster::block_on(GpuContext::new(instance, Some(&surface)))?;

        // A non-sRGB format: the chain's own encode is the only one, the
        // same honesty the headless path's target has. With an `*Srgb`
        // surface the GPU would encode a second time.
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
        let stage = Stage::build(&gpu.device, &gpu.queue, format, size.width, size.height)?;
        let mut state = WindowState {
            window,
            surface,
            gpu,
            stage,
            format,
        };
        configure_surface(&mut state, size.width, size.height);
        println!("adapter: {}", state.gpu.adapter.get_info().name);
        Ok(state)
    }

    fn on_key(&mut self, code: KeyCode, event_loop: &ActiveEventLoop) {
        match code {
            KeyCode::Escape | KeyCode::KeyQ => event_loop.exit(),
            KeyCode::Space => {
                // Pausing keeps the phase, so the torus does not jump.
                self.paused_at = match self.paused_at {
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

// ---------------------------------------------------------------------------
// The material: molten rock
// ---------------------------------------------------------------------------

/// The molten torus, as a graph.
///
/// Every term is a node from the S2/S3 batches: `worley3`'s cell borders
/// are the cracks (F2 − F1 is small near a wall), a `pulse` makes them
/// breathe, a `simplex2` gradient wrinkles the shading normal, and `fbm3`
/// displaces the vertices so the silhouette and the shadows are molten
/// too. The emissive is linear radiance well above one — that is what the
/// bloom thresholds on.
fn lava_graph() -> Graph {
    let nodes = wxsl::stdlib::registry();
    let mut graph = Graph::new("molten torus");
    let wire = |graph: &mut Graph, from: (NodeId, &str), to: (NodeId, &str)| {
        graph
            .wire(&nodes, from, to)
            .expect("a cinema material wire");
    };

    let uv = graph.add_node(abi::context_node_id("uv"));
    let time = graph.add_node(abi::context_node_id("time"));

    // The crack field, crawling slowly along the third axis: uv * 6
    // widened into a 3D point whose z is the clock.
    let scaled = graph.add(Node::new("math.multiply").with_param("b", Value::F32(10.0)));
    wire(&mut graph, (uv, "out"), (scaled, "a"));
    let crawl = graph.add(Node::new("math.multiply").with_param("b", Value::F32(0.15)));
    wire(&mut graph, (time, "out"), (crawl, "a"));
    let point = graph.add_node("convert.combine.vec3f");
    let x = graph.add_node("convert.split.vec2f");
    wire(&mut graph, (scaled, "out"), (x, "v"));
    wire(&mut graph, (x, "x"), (point, "x"));
    wire(&mut graph, (x, "y"), (point, "y"));
    wire(&mut graph, (crawl, "out"), (point, "z"));

    let cracks = graph.add_node("generative.worley3");
    wire(&mut graph, (point, "out"), (cracks, "p"));
    let pair = graph.add_node("convert.split.vec2f");
    wire(&mut graph, (cracks, "out"), (pair, "v"));
    // F2 − F1 is smallest at a cell border, so the vein is its inverse.
    let gap = graph.add(Node::new("math.subtract").with_param("b", Value::F32(0.0)));
    wire(&mut graph, (pair, "y"), (gap, "a"));
    wire(&mut graph, (pair, "x"), (gap, "b"));
    let rim = graph.add(
        Node::new("math.smoothstep")
            .with_param("edge0", Value::F32(0.0))
            .with_param("edge1", Value::F32(0.09)),
    );
    wire(&mut graph, (gap, "out"), (rim, "x"));
    let vein = graph.add(Node::new("math.subtract").with_param("a", Value::F32(1.0)));
    wire(&mut graph, (rim, "out"), (vein, "b"));
    let ember = graph.add(Node::new("math.multiply").with_param("b", Value::F32(1.8)));
    wire(&mut graph, (vein, "out"), (ember, "a"));
    // The pulse rides *on top* of the mask: heat that never quite dies.
    let flicker = graph.add(
        Node::new("animation.pulse")
            .with_param("frequency", Value::F32(0.7))
            .with_param("phase", Value::F32(0.25)),
    );
    wire(&mut graph, (time, "out"), (flicker, "time"));
    let breath = graph.add(Node::new("math.multiply").with_param("b", Value::F32(0.6)));
    wire(&mut graph, (flicker, "out"), (breath, "a"));
    let live = graph.add(Node::new("math.add").with_param("b", Value::F32(0.7)));
    wire(&mut graph, (breath, "out"), (live, "a"));
    let heat = graph.add(Node::new("math.multiply"));
    wire(&mut graph, (ember, "out"), (heat, "a"));
    wire(&mut graph, (live, "out"), (heat, "b"));

    // HDR radiance: the colour of hot metal, far past diffuse white —
    // what bloom's threshold and the ACES shoulder are both for.
    let channel = |graph: &mut Graph, scale: f32| {
        let scaled = graph.add(Node::new("math.multiply").with_param("b", Value::F32(scale)));
        wire(graph, (heat, "out"), (scaled, "a"));
        scaled
    };
    let red = channel(&mut graph, 2.6);
    let green = channel(&mut graph, 0.75);
    let blue = channel(&mut graph, 0.18);
    let emissive = graph.add_node("convert.combine.vec3f");
    wire(&mut graph, (red, "out"), (emissive, "x"));
    wire(&mut graph, (green, "out"), (emissive, "y"));
    wire(&mut graph, (blue, "out"), (emissive, "z"));

    // Charcoal rock, its roughness a simplex field — the melt is patchy.
    let charcoal =
        graph.add(Node::new("const.value").with_param("value", Value::Vec3([0.12, 0.10, 0.095])));
    graph
        .set_generic(&nodes, charcoal, "T", ValueType::Vec3)
        .expect("charcoal is a vec3f");
    let fine = graph.add(Node::new("math.multiply").with_param("b", Value::F32(4.0)));
    wire(&mut graph, (uv, "out"), (fine, "a"));
    let patch = graph.add_node("generative.simplex2");
    wire(&mut graph, (fine, "out"), (patch, "p"));
    let spread = graph.add(Node::new("math.multiply").with_param("b", Value::F32(0.25)));
    wire(&mut graph, (patch, "out"), (spread, "a"));
    let roughness = graph.add(Node::new("math.add").with_param("b", Value::F32(0.6)));
    wire(&mut graph, (spread, "out"), (roughness, "a"));

    // The normal: a simplex gradient by finite difference, three cheap
    // samples, flattened toward the cracks so the melt reads as liquid.
    let micro = graph.add(Node::new("math.multiply").with_param("b", Value::F32(24.0)));
    wire(&mut graph, (uv, "out"), (micro, "a"));
    let here = graph.add_node("generative.simplex2");
    wire(&mut graph, (micro, "out"), (here, "p"));
    let shifted = |graph: &mut Graph, dx: f32, dy: f32| {
        let offset = graph.add(Node::new("const.value").with_param("value", Value::Vec2([dx, dy])));
        graph
            .set_generic(&nodes, offset, "T", ValueType::Vec2)
            .expect("the shift is a vec2f");
        let at = graph.add(Node::new("math.add"));
        wire(graph, (micro, "out"), (at, "a"));
        wire(graph, (offset, "out"), (at, "b"));
        let sample = graph.add_node("generative.simplex2");
        wire(graph, (at, "out"), (sample, "p"));
        sample
    };
    let east = shifted(&mut graph, 0.02, 0.0);
    let north = shifted(&mut graph, 0.0, 0.02);
    let slope = |graph: &mut Graph, a: NodeId, b: NodeId| {
        let d = graph.add(Node::new("math.multiply").with_param("b", Value::F32(0.4)));
        let sub = graph.add(Node::new("math.subtract"));
        wire(graph, (a, "out"), (sub, "a"));
        wire(graph, (b, "out"), (sub, "b"));
        wire(graph, (sub, "out"), (d, "a"));
        d
    };
    let dx = slope(&mut graph, here, east);
    let dy = slope(&mut graph, here, north);
    let flat = graph.add(Node::new("convert.combine.vec3f"));
    wire(&mut graph, (dx, "out"), (flat, "x"));
    wire(&mut graph, (dy, "out"), (flat, "y"));
    let zero = graph.add(Node::new("const.value").with_param("value", Value::F32(0.0)));
    graph
        .set_generic(&nodes, zero, "T", ValueType::F32)
        .expect("zero is an f32");
    wire(&mut graph, (zero, "out"), (flat, "z"));
    let geometric = graph.add_node(abi::context_node_id("world_normal"));
    let bumped = graph.add(Node::new("math.add"));
    wire(&mut graph, (geometric, "out"), (bumped, "a"));
    wire(&mut graph, (flat, "out"), (bumped, "b"));
    let normal = graph.add_node("math.safe_normalize");
    wire(&mut graph, (bumped, "out"), (normal, "v"));

    // The vertex half: low-frequency fbm swells the tube, so the
    // silhouette heaves and the shadow passes see the same shape.
    let object_position = graph.add_node(abi::context_node_id("object_position"));
    let swell = graph.add(Node::new("math.multiply").with_param("b", Value::F32(2.2)));
    wire(&mut graph, (object_position, "out"), (swell, "a"));
    let relief = graph.add_node("generative.fbm3");
    wire(&mut graph, (swell, "out"), (relief, "p"));
    let centred = graph.add(Node::new("math.add").with_param("b", Value::F32(-0.5)));
    wire(&mut graph, (relief, "out"), (centred, "a"));
    let sized = graph.add(Node::new("math.multiply").with_param("b", Value::F32(0.12)));
    wire(&mut graph, (centred, "out"), (sized, "a"));
    let object_normal = graph.add_node(abi::context_node_id("object_normal"));
    let offset = graph.add(Node::new("math.multiply"));
    wire(&mut graph, (object_normal, "out"), (offset, "a"));
    wire(&mut graph, (sized, "out"), (offset, "b"));

    let surface = graph.add_node(abi::SURFACE_OUTPUT_ID);
    wire(&mut graph, (charcoal, "out"), (surface, "base_color"));
    wire(&mut graph, (roughness, "out"), (surface, "roughness"));
    wire(&mut graph, (normal, "out"), (surface, "normal"));
    wire(&mut graph, (emissive, "out"), (surface, "emissive"));
    let vertex = graph.add_node(abi::VERTEX_OUTPUT_ID);
    wire(
        &mut graph,
        (offset, "out"),
        (vertex, abi::SOCKET_POSITION_OFFSET),
    );
    graph
}

// ---------------------------------------------------------------------------
// The pipeline: lit frame, glow, lens, curve
// ---------------------------------------------------------------------------

/// The deferred preset's pass list with the whole S4 chain composed onto
/// it: separable bloom over the HDR lit frame, then chromatic aberration,
/// vignette and film grain — each writing an HDR intermediate the next
/// reads, because every one of them is light arithmetic and an 8-bit
/// intermediate would clamp it — and the ACES transform last.
fn cinema_document() -> Graph {
    let registry = wxsl::render::document_registry(&EffectRegistry::shipped());
    let mut graph = wxsl::core::pipeline::document("cinema");
    let wire = |graph: &mut Graph, from: (NodeId, &str), to: (NodeId, &str)| {
        graph.wire(&registry, from, to).expect("cinema wiring");
    };

    let scene = graph.add_node(doc::SOURCE_SCENE);
    let lights = graph.add_node(doc::SOURCE_LIGHTS);
    let shadows = graph.add_node(doc::PASS_SHADOW);
    let gbuffer = graph.add_node(doc::RESOURCE_GBUFFER);
    let material = graph.add(
        Node::new(doc::PASS_GEOMETRY)
            .with_label("molten surface")
            .with_setting(doc::SETTING_STAGE, "gbuffer"),
    );
    let scene_color = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("lit")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let lighting = graph.add(Node::new(doc::PASS_SCREEN).with_label("deferred lighting"));
    let present = graph.add_node(doc::PRESENT);

    wire(&mut graph, (scene, "draws"), (shadows, "draws"));
    wire(&mut graph, (lights, "shadows"), (shadows, "into"));
    wire(&mut graph, (scene, "draws"), (material, "draws"));
    wire(&mut graph, (gbuffer, "gbuffer"), (material, "gbuffer"));
    wire(&mut graph, (gbuffer, "gbuffer"), (lighting, "gbuffer"));
    wire(&mut graph, (scene_color, "color"), (lighting, "into"));

    // Separable bloom: threshold and horizontal Gaussian into `glow`,
    // then the vertical pass adding it back over the lit frame.
    let glow = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("glow")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    let bloom_x = graph.add(
        Node::new(doc::PASS_SCREEN)
            .with_label("bloom")
            .with_setting(doc::SETTING_EFFECT, "wxsl.bloom_x"),
    );
    let bloom_y = graph
        .add(Node::new(format!("{}wxsl.bloom_y", doc::PASS_SCREEN_PREFIX)).with_label("bloom-y"));
    // A resource is the interchange between every pair of passes here:
    // the writer takes it on `into`, the reader's `image` reads it.
    let bloomed = graph.add(
        Node::new(doc::RESOURCE_COLOR)
            .with_label("bloomed")
            .with_setting(doc::SETTING_PRECISION, "hdr"),
    );
    wire(&mut graph, (scene_color, "color"), (bloom_x, "image"));
    wire(&mut graph, (glow, "color"), (bloom_x, "into"));
    wire(&mut graph, (scene_color, "color"), (bloom_y, "color"));
    wire(&mut graph, (glow, "color"), (bloom_y, "glow"));
    wire(&mut graph, (bloomed, "color"), (bloom_y, "into"));

    // The lens and the film, each reading the previous stage's target.
    // The effect parameters ship as the nodes' defaults; the chain is
    // here to be tuned in the editor's pipeline canvas.
    let stage = |graph: &mut Graph, label: &'static str, effect: &str, from: NodeId| -> NodeId {
        let image = graph.add(
            Node::new(doc::RESOURCE_COLOR)
                .with_label(label)
                .with_setting(doc::SETTING_PRECISION, "hdr"),
        );
        let pass = graph.add(
            Node::new(doc::PASS_SCREEN)
                .with_label(label)
                .with_setting(doc::SETTING_EFFECT, effect),
        );
        wire(graph, (from, "color"), (pass, "image"));
        wire(graph, (image, "color"), (pass, "into"));
        image
    };
    let aberrated = stage(
        &mut graph,
        "aberration",
        "wxsl.chromatic_aberration",
        bloomed,
    );
    let vignetted = stage(&mut graph, "vignette", "wxsl.vignette", aberrated);
    let grained = stage(&mut graph, "grain", "wxsl.film_grain", vignetted);

    let aces = graph.add(
        Node::new(doc::PASS_SCREEN)
            .with_label("aces")
            .with_setting(doc::SETTING_EFFECT, "cinema.aces"),
    );
    wire(&mut graph, (grained, "color"), (aces, "image"));
    wire(&mut graph, (aces, "color"), (present, "surface"));
    graph
}

/// The demo's display transform, as a screen graph: load the image, run
/// the library's ACES fit, encode it — the stock tonemap graph with
/// `color.tonemap_aces` where the filmic curve sits.
fn aces_effect(
    nodes: &wxsl::core::node::NodeRegistry,
) -> Result<Effect, wxsl::core::error::CodegenError> {
    let mut graph = Graph::in_domain("aces", wxsl::core::node::GraphDomain::Screen);
    let wire = |graph: &mut Graph, from: (NodeId, &str), to: (NodeId, &str)| {
        graph.wire(nodes, from, to).expect("the aces graph wires");
    };
    let image = graph.add_node(abi::SCREEN_IMAGE_ID);
    let uv = graph.add_node(abi::context_node_id("uv"));
    let load = graph.add_node("sample.load_2d");
    let split = graph.add_node("convert.split.vec4f");
    let color = graph.add_node("convert.combine.vec3f");
    let curve = graph.add_node("color.tonemap_aces");
    let encode = graph.add_node("color.linear_to_srgb");
    let out = graph.add_node(abi::SCREEN_OUTPUT_ID);

    wire(&mut graph, (image, "out"), (load, "tex"));
    wire(&mut graph, (uv, "out"), (load, "uv"));
    wire(&mut graph, (load, "out"), (split, "v"));
    for channel in ["x", "y", "z"] {
        wire(&mut graph, (split, channel), (color, channel));
    }
    wire(&mut graph, (color, "out"), (curve, "color"));
    wire(&mut graph, (curve, "out"), (encode, "color"));
    wire(&mut graph, (encode, "out"), (out, abi::SOCKET_SCREEN_COLOR));
    wire(&mut graph, (split, "w"), (out, abi::SOCKET_SCREEN_ALPHA));
    Effect::from_graph(
        "cinema.aces",
        "ACES",
        "The ACES filmic curve, as a graph.",
        graph,
        nodes,
    )
}

// ---------------------------------------------------------------------------
// The scene
// ---------------------------------------------------------------------------

/// Two lights and a cold bounce: one warm key to catch on the metal, one
/// blue rim behind to cut the silhouette out of the dark. The ambient is
/// near nothing — this is a picture about the emissive.
fn cinema_environment(aspect: f32, time: f32) -> Environment {
    Environment {
        camera: Camera {
            eye: Vec3::new(0.0, 1.1, 3.6),
            target: Vec3::ZERO,
            aspect,
            ..Camera::default()
        },
        lights: vec![
            Light::point(Vec3::new(2.6, 3.0, 2.2), Vec3::new(1.0, 0.86, 0.72), 30.0),
            Light::point(Vec3::new(-3.0, 1.2, -2.0), Vec3::new(0.35, 0.5, 1.0), 22.0),
        ],
        ambient_sky: Vec3::new(0.02, 0.025, 0.04),
        ambient_ground: Vec3::new(0.01, 0.008, 0.008),
        exposure: 1.0,
        time,
        previous_time: time,
        previous_camera: None,
    }
}
