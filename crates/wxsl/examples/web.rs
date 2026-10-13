//! The demo cube in a browser (ADR 0075): WebGPU where the browser has it,
//! WebGL2 where it does not, the same renderer either way.
//!
//! Three tinted cubes, each reading its own transform and tint row, under a
//! shadow-casting sun, through the stock forward or deferred pipeline. On
//! WebGL2 the device profile is read off the adapter's limits, so the rows
//! come from data textures and the environment LUT is baked by its fragment
//! arm — nothing here asks for either.
//!
//! Build and serve (from the workspace root):
//!
//! ```sh
//! cargo build -p wxsl --example web --features webgl --target wasm32-unknown-unknown --release
//! wasm-bindgen --target web --out-dir target/web --no-typescript \
//!     target/wasm32-unknown-unknown/release/examples/web.wasm
//! cp crates/wxsl/examples/web.html target/web/index.html
//! python3 -m http.server -d target/web 8000
//! ```
//!
//! Then open `http://localhost:8000/`. `?webgl` forces the WebGL2 backend,
//! `?deferred` starts on the deferred pipeline; `F` and `D` switch. The
//! page's `#status` line says which backend and profile it got. Natively,
//! `cargo run -p wxsl --example web` opens the same scene in a window.

use std::sync::Arc;

use glam::{Mat4, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};
use wxsl::core::graph::Graph;
use wxsl::core::node::{Value, ValueType};
use wxsl::render::material::Material;
use wxsl::render::wgpu;
use wxsl::render::{
    Camera, DrawItem, DrawList, Environment, GpuContext, InstanceAttributes, Light,
    MaterialBindings, Mesh, RenderRequest, Renderer, StockPipeline, TargetConfig,
};

/// Everything that exists once the device does.
struct State {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    gpu: GpuContext,
    renderer: Renderer,
    format: wgpu::TextureFormat,
    cube: Mesh,
    ground: Mesh,
    material: Material,
    bindings: MaterialBindings,
    tints: Vec<InstanceAttributes>,
    pipeline: StockPipeline,
    frame: u32,
}

#[derive(Default)]
struct App {
    /// Filled asynchronously on the web, where a device cannot be waited
    /// for; at once natively.
    state: std::rc::Rc<std::cell::RefCell<Option<State>>>,
    started: bool,
}

/// What the page asked for: WebGL2 forced, and the starting pipeline.
fn query() -> (bool, StockPipeline) {
    #[cfg(target_arch = "wasm32")]
    let search = web_sys::window()
        .and_then(|window| window.location().search().ok())
        .unwrap_or_default();
    #[cfg(not(target_arch = "wasm32"))]
    let search = std::env::args().collect::<Vec<_>>().join(" ");
    let pipeline = if search.contains("deferred") {
        StockPipeline::Deferred
    } else {
        StockPipeline::Forward
    };
    (search.contains("webgl"), pipeline)
}

/// Say something where the page and a headless browser can read it.
fn status(text: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::console::log_1(&text.into());
        if let Some(element) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("status"))
        {
            element.set_text_content(Some(text));
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    println!("{text}");
}

async fn create_state(window: Arc<Window>) -> Result<State, Box<dyn std::error::Error>> {
    let (force_webgl, pipeline) = query();
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    if force_webgl {
        descriptor.backends = wgpu::Backends::GL;
    }
    // WebGPU if the browser can make an adapter, WebGL2 if not.
    let instance = wgpu::util::new_instance_with_webgpu_detection(descriptor).await;
    let surface = instance.create_surface(Arc::clone(&window))?;
    let gpu = GpuContext::new(instance, Some(&surface)).await?;
    let capabilities = surface.get_capabilities(&gpu.adapter);
    // Linear: the display transform encodes sRGB itself.
    let format = capabilities
        .formats
        .iter()
        .copied()
        .find(|format| !format.is_srgb())
        .unwrap_or(capabilities.formats[0]);
    let size = window.inner_size();
    let mut renderer = Renderer::new(
        &gpu.device,
        wxsl::stdlib_library(),
        TargetConfig::new(size.width.max(1), size.height.max(1), format),
    )?;
    renderer.set_pipeline(pipeline);

    let registry = wxsl::stdlib::registry();
    let graph: Graph = serde_json::from_str(include_str!("../assets/pbr_cube.wxsl.json"))?;
    let material = Material::from_graph(&graph, &registry)?;
    let bindings = white_bindings(&gpu, &mut renderer, &material)?;
    let tints = [
        [1.0, 0.45, 0.35],
        [0.4, 1.0, 0.5],
        [0.45, 0.6, 1.0],
        [1.0, 1.0, 1.0],
    ]
    .into_iter()
    .map(|tint| InstanceAttributes::new().with("instance_tint", Value::Vec3(tint)))
    .collect();
    status(&format!(
        "{:?} · {} · profile {} · {pipeline}",
        gpu.adapter.get_info().backend,
        gpu.adapter.get_info().name,
        renderer.profile(),
    ));
    let cube = Mesh::cube(&gpu.device, 0.9);
    let ground = Mesh::plane(&gpu.device, 8.0);
    let mut state = State {
        window,
        surface,
        gpu,
        renderer,
        format,
        cube,
        ground,
        material,
        bindings,
        tints,
        pipeline,
        frame: 0,
    };
    configure(&mut state);
    Ok(state)
}

/// The demo material's bind group with every texture a white texel: the
/// identity for what the graph multiplies it by.
fn white_bindings(
    gpu: &GpuContext,
    renderer: &mut Renderer,
    material: &Material,
) -> Result<MaterialBindings, Box<dyn std::error::Error>> {
    let mut bindings = renderer.material_bindings(&gpu.device, material);
    let extent = wgpu::Extent3d {
        width: 1,
        height: 1,
        depth_or_array_layers: 1,
    };
    let white = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("white"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        white.as_image_copy(),
        &[255u8; 4],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        extent,
    );
    let view = white.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = gpu
        .device
        .create_sampler(&wgpu::SamplerDescriptor::default());
    for resource in &material.interface().resources {
        let name = resource.name.as_str();
        if resource.ty == ValueType::Sampler {
            bindings.set_sampler(name, &sampler)?;
        } else {
            bindings.set_texture(name, &view)?;
        }
    }
    bindings.upload(&gpu.device, &gpu.queue)?;
    Ok(bindings)
}

fn configure(state: &mut State) {
    let size = state.window.inner_size();
    let (width, height) = (size.width.max(1), size.height.max(1));
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

fn render(state: &mut State) {
    let frame = match state.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(frame)
        | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
        wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
            configure(state);
            return;
        }
        _ => return,
    };
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    // Frames, not a clock: `Instant` does not exist on the web.
    let time = state.frame as f32 / 60.0;
    state.frame += 1;
    let size = state.window.inner_size();
    let environment = Environment {
        camera: Camera {
            eye: Vec3::new(3.0, 2.6, 4.2),
            target: Vec3::new(0.0, 0.3, 0.0),
            aspect: size.width as f32 / size.height.max(1) as f32,
            ..Camera::default()
        },
        lights: vec![
            Light::directional(Vec3::new(-0.5, 1.0, 0.4), Vec3::new(1.0, 0.95, 0.85), 2.2)
                .casting_shadow(5.0),
            Light::point(Vec3::new(1.8, 2.0, 1.6), Vec3::new(0.6, 0.7, 1.0), 14.0),
        ],
        ambient_sky: Vec3::new(0.2, 0.25, 0.35),
        ambient_ground: Vec3::new(0.05, 0.04, 0.03),
        time,
        previous_time: time,
        ..Environment::default()
    };
    let mut draws = DrawList::new();
    for (index, x) in [-1.3f32, 0.0, 1.3].into_iter().enumerate() {
        draws.push(
            DrawItem::new(&state.cube, &state.material)
                .with_transform(
                    Mat4::from_translation(Vec3::new(x, 0.45, 0.0))
                        * Mat4::from_rotation_y(time * 0.6 + index as f32),
                )
                .with_bindings(&state.bindings)
                .with_attributes(&state.tints[index]),
        );
    }
    draws.push(
        DrawItem::new(&state.ground, &state.material)
            .with_bindings(&state.bindings)
            .with_attributes(&state.tints[3]),
    );
    if let Err(error) = state.renderer.render(
        &state.gpu.device,
        &state.gpu.queue,
        &RenderRequest {
            view: &view,
            environment: &environment,
            draws: &draws,
        },
    ) {
        status(&format!("cannot render: {error}"));
    }
    state.gpu.queue.present(frame);
    if state.frame == 120 {
        // A milestone a headless browser can wait for.
        status(&format!(
            "rendered 120 frames · profile {} · {}",
            state.renderer.profile(),
            state.pipeline
        ));
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.started {
            return;
        }
        self.started = true;
        let attributes = Window::default_attributes()
            .with_title("wxsl — web")
            .with_inner_size(winit::dpi::LogicalSize::new(960, 540));
        #[cfg(target_arch = "wasm32")]
        let attributes = {
            use winit::platform::web::WindowAttributesExtWebSys;
            attributes.with_append(true)
        };
        let window = Arc::new(event_loop.create_window(attributes).expect("a window"));
        let slot = std::rc::Rc::clone(&self.state);
        let created = async move {
            match create_state(Arc::clone(&window)).await {
                Ok(state) => *slot.borrow_mut() = Some(state),
                Err(error) => status(&format!("cannot start: {error}")),
            }
            window.request_redraw();
        };
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(created);
        #[cfg(not(target_arch = "wasm32"))]
        pollster::block_on(created);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let mut slot = self.state.borrow_mut();
        let Some(state) = slot.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) => configure(state),
            WindowEvent::RedrawRequested => {
                render(state);
                state.window.request_redraw();
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code @ (KeyCode::KeyF | KeyCode::KeyD)),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                state.pipeline = if code == KeyCode::KeyF {
                    StockPipeline::Forward
                } else {
                    StockPipeline::Deferred
                };
                state.renderer.set_pipeline(state.pipeline);
                status(&format!(
                    "profile {} · {}",
                    state.renderer.profile(),
                    state.pipeline
                ));
            }
            _ => {}
        }
    }
}

fn main() {
    #[cfg(target_arch = "wasm32")]
    std::panic::set_hook(Box::new(|info| status(&format!("panic: {info}"))));
    let event_loop = EventLoop::new().expect("an event loop");
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        event_loop.spawn_app(App::default());
    }
    #[cfg(not(target_arch = "wasm32"))]
    event_loop
        .run_app(&mut App::default())
        .expect("the event loop runs");
}
