//! The window: input, the orbit camera, and switching demos.

use super::*;

/// Everything that only exists once there is a window.
pub(crate) struct State {
    pub(crate) window: Arc<Window>,
    pub(crate) surface: wgpu::Surface<'static>,
    pub(crate) stage: Stage,
    pub(crate) format: wgpu::TextureFormat,
}

/// Example-local arc-rotate camera, initialized from each demo's authored view.
#[derive(Clone, Copy)]
pub(crate) struct OrbitCamera {
    pub(crate) camera: Camera,
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
    pub(crate) radius: f32,
}

impl OrbitCamera {
    pub(crate) fn new(camera: Camera) -> Self {
        let offset = camera.eye - camera.target;
        let radius = offset.length().max(0.05);
        Self {
            camera,
            yaw: offset.x.atan2(offset.z),
            pitch: (offset.y / radius).clamp(-1.0, 1.0).asin(),
            radius,
        }
    }

    pub(crate) fn view(&self, aspect: f32) -> Camera {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Camera {
            eye: self.camera.target + Vec3::new(sy * cp, sp, cy * cp) * self.radius,
            aspect,
            ..self.camera
        }
    }

    pub(crate) fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * 0.005;
        self.pitch = (self.pitch + dy * 0.005).clamp(-FRAC_PI_2 + 0.01, FRAC_PI_2 - 0.01);
    }

    pub(crate) fn zoom(&mut self, lines: f32) {
        self.radius = (self.radius * (-lines * 0.12).exp()).clamp(0.05, 90.0);
    }

    pub(crate) fn pan(&mut self, dx: f32, dy: f32, height: f32) {
        let camera = self.view(1.0);
        let forward = (camera.target - camera.eye).normalize();
        let right = forward.cross(camera.up).normalize();
        let up = right.cross(forward);
        let scale = 2.0 * self.radius * (camera.fov_y * 0.5).tan() / height.max(1.0);
        self.camera.target += (-right * dx + up * dy) * scale;
    }
}

pub(crate) struct App {
    pub(crate) options: Options,
    pub(crate) demos: Vec<Demo>,
    pub(crate) current: usize,
    pub(crate) state: Option<State>,
    pub(crate) started: Instant,
    pub(crate) paused_at: Option<f32>,
    pub(crate) error: Option<Box<dyn Error>>,
    /// When the previous frame rendered, so a temporal demo's velocity
    /// answers for the *measured* frame time rather than an assumed one.
    pub(crate) last_frame: Option<Instant>,
    pub(crate) cameras: Vec<Option<OrbitCamera>>,
    pub(crate) previous_camera: Option<Camera>,
    pub(crate) cursor: Option<(f64, f64)>,
    pub(crate) drag: Option<MouseButton>,
    /// The web's device arrives asynchronously; it lands here first.
    #[cfg(target_arch = "wasm32")]
    pub(crate) pending: std::rc::Rc<std::cell::RefCell<Option<Result<State, Box<dyn Error>>>>>,
    #[cfg(target_arch = "wasm32")]
    pub(crate) launched: bool,
}

/// Say something where a terminal, or a page and a headless browser, can
/// read it.
pub(crate) fn say(text: &str) {
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

impl App {
    pub(crate) fn new(options: Options, demos: Vec<Demo>, start: usize) -> Self {
        App {
            cameras: vec![None; demos.len()],
            previous_camera: None,
            cursor: None,
            drag: None,
            options,
            demos,
            current: start,
            state: None,
            started: Instant::now(),
            paused_at: None,
            error: None,
            last_frame: None,
            #[cfg(target_arch = "wasm32")]
            pending: Default::default(),
            #[cfg(target_arch = "wasm32")]
            launched: false,
        }
    }

    pub(crate) fn elapsed(&self) -> f32 {
        self.paused_at
            .unwrap_or_else(|| self.started.elapsed().as_secs_f32())
    }

    pub(crate) fn announce(&self) {
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
        say(&format!(
            "[{}/{}] {} — {}  |  pipeline: {}  |  {stats}",
            self.current + 1,
            self.demos.len(),
            demo.name,
            demo.blurb,
            demo.pipeline.name(),
        ));
        if let Some(state) = self.state.as_ref() {
            state
                .window
                .set_title(&format!("wxsl gallery — {}", demo.name));
        }
    }

    /// Step to the demo at `index`, wrapping, and switch the renderer over.
    pub(crate) fn show(&mut self, index: usize) {
        let index = index % self.demos.len();
        if index == self.current {
            return;
        }
        self.current = index;
        self.previous_camera = None;
        self.drag = None;
        if let Some(state) = self.state.as_mut() {
            if let Err(error) = apply(&self.demos[index], &mut state.stage.renderer) {
                eprintln!("cannot switch to `{}`: {error}", self.demos[index].name);
            }
        }
        self.announce();
    }

    pub(crate) fn render(&mut self) {
        let time = self.elapsed();
        let step = self
            .last_frame
            .replace(Instant::now())
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
        let orbit = self.cameras[self.current]
            .get_or_insert_with(|| OrbitCamera::new(demo_environment(demo, 1.0, time).camera));
        let camera = orbit.view(size.width.max(1) as f32 / size.height.max(1) as f32);
        state.stage.camera_override = Some((camera, self.previous_camera));
        if let Err(error) = state.stage.render(
            demo,
            &view,
            size.width.max(1),
            size.height.max(1),
            time,
            step,
        ) {
            say(&format!("cannot render: {error}"));
        } else {
            self.previous_camera = Some(camera);
        }
        state.stage.gpu.queue.present(frame);
    }
}

pub(crate) fn configure_surface(state: &mut State, width: u32, height: u32) {
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
    #[cfg(target_arch = "wasm32")]
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.launched {
            return;
        }
        self.launched = true;
        let window = match self.create_window(event_loop) {
            Ok(window) => window,
            Err(error) => return say(&format!("cannot start: {error}")),
        };
        let slot = std::rc::Rc::clone(&self.pending);
        let ibl_source = self.options.ibl_source.clone();
        let quality = self.options.refraction_quality;
        wasm_bindgen_futures::spawn_local(async move {
            let state = build_state(Arc::clone(&window), &ibl_source, quality).await;
            *slot.borrow_mut() = Some(state);
            window.request_redraw();
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
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
        #[cfg(target_arch = "wasm32")]
        self.adopt_pending();
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(state) = self.state.as_mut() {
                    configure_surface(state, size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::MouseInput { state, button, .. }
                if matches!(
                    button,
                    MouseButton::Left | MouseButton::Right | MouseButton::Middle
                ) =>
            {
                if state == ElementState::Pressed {
                    self.drag = Some(button);
                } else if self.drag == Some(button) {
                    self.drag = None;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some((x, y)) = self.cursor {
                    if let Some(camera) = self.cameras[self.current].as_mut() {
                        let dx = (position.x - x) as f32;
                        let dy = (position.y - y) as f32;
                        match self.drag {
                            Some(MouseButton::Left) => camera.orbit(dx, dy),
                            Some(MouseButton::Right | MouseButton::Middle) => {
                                let height = self
                                    .state
                                    .as_ref()
                                    .map_or(1, |state| state.window.inner_size().height);
                                camera.pan(dx, dy, height as f32);
                            }
                            _ => {}
                        }
                    }
                }
                self.cursor = Some((position.x, position.y));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(camera) = self.cameras[self.current].as_mut() {
                    let lines = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(position) => position.y as f32 / 80.0,
                    };
                    camera.zoom(lines);
                }
            }
            WindowEvent::Focused(false) => {
                self.drag = None;
                self.cursor = None;
            }
            WindowEvent::CursorLeft { .. } => self.cursor = None,
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
    pub(crate) fn create_window(
        &self,
        event_loop: &ActiveEventLoop,
    ) -> Result<Arc<Window>, Box<dyn Error>> {
        let (width, height) = self.options.size.unwrap_or((1280, 720));
        let attributes = Window::default_attributes()
            .with_title("wxsl gallery")
            .with_inner_size(winit::dpi::LogicalSize::new(width, height));
        #[cfg(target_arch = "wasm32")]
        let attributes = {
            use winit::platform::web::WindowAttributesExtWebSys;
            attributes.with_append(true)
        };
        Ok(Arc::new(event_loop.create_window(attributes)?))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn create_state(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) -> Result<State, Box<dyn Error>> {
        let window = self.create_window(event_loop)?;
        let state = pollster::block_on(build_state(
            window,
            &self.options.ibl_source,
            self.options.refraction_quality,
        ))?;
        self.prepare(state)
    }

    /// Show the current demo on a freshly built state.
    pub(crate) fn prepare(&self, mut state: State) -> Result<State, Box<dyn Error>> {
        apply(&self.demos[self.current], &mut state.stage.renderer)?;
        let size = state.window.inner_size();
        configure_surface(&mut state, size.width, size.height);
        Ok(state)
    }

    /// Take the state the web's asynchronous start left behind, once.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn adopt_pending(&mut self) {
        let Some(result) = self.pending.borrow_mut().take() else {
            return;
        };
        match result.and_then(|state| self.prepare(state)) {
            Ok(state) => {
                self.state = Some(state);
                self.announce();
            }
            Err(error) => say(&format!("cannot start: {error}")),
        }
    }

    pub(crate) fn on_key(&mut self, code: KeyCode, event_loop: &ActiveEventLoop) {
        match code {
            KeyCode::Escape | KeyCode::KeyQ => event_loop.exit(),
            KeyCode::KeyR => {
                self.cameras[self.current] = None;
                self.previous_camera = None;
            }
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

/// The device, the surface and the stage, for a window that exists.
pub(crate) async fn build_state(
    window: Arc<Window>,
    ibl_source: &str,
    refraction_quality: RefractionQuality,
) -> Result<State, Box<dyn Error>> {
    let descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    #[cfg(not(target_arch = "wasm32"))]
    let instance = wgpu::Instance::new(descriptor);
    // WebGPU if the browser can make an adapter, WebGL2 if not.
    #[cfg(target_arch = "wasm32")]
    let instance = wgpu::util::new_instance_with_webgpu_detection(descriptor).await;
    let surface = instance.create_surface(Arc::clone(&window))?;
    let gpu = GpuContext::new(instance, Some(&surface)).await?;

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
        TargetConfig::new(size.width.max(1), size.height.max(1), format),
        ibl_source,
    )?;
    stage.refraction_quality = refraction_quality;
    Ok(State {
        window,
        surface,
        stage,
        format,
    })
}
