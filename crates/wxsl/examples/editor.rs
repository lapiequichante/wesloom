//! The node editor, in a window.
//!
//! Everything editor-shaped is in `wxsl-editor`, which is
//! windowing-agnostic (ADR 0013). This file is the other half: a winit event
//! loop, a surface, and the translation from winit's events to
//! [`wxsl::render::ui::UiEvent`]. It is the only file in the workspace that
//! knows winit exists, which is what keeps the editor embeddable in an
//! application that already has an event loop of its own.
//!
//! ```text
//! cargo run --example editor                          # the shipped demo graph
//! cargo run --example editor -- --graph my.wxsl.json   # a graph of your own
//! cargo run --example editor -- --msdf gpu             # generate glyphs on the GPU
//! cargo run --example editor -- --font C:/Windows/Fonts/segoeui.ttf
//! cargo run --example editor -- --list-fonts           # what it would pick
//! cargo run --example editor -- --screenshot out.png   # one frame, no window
//! ```
//!
//! # Fonts
//!
//! `wxsl-render` ships no font, exactly as it ships no shaders (ADR 0014):
//! the application supplies them. This one looks in a short list of
//! well-known places for a proportional and a monospaced face, and
//! `--font`/`--mono` override it. If the search fails, the error says so
//! rather than opening a window with invisible labels.
//!
//! # Keys
//!
//! `F`/`D` render path · `M` preview mesh · `G` MSDF backend · `A` add a node
//! · `R` frame the graph · `Space` pause the spin · `Ctrl+S` save ·
//! `Delete` remove the selection · `Esc` quit.
//!
//! # Pointer
//!
//! Drag a node to move it · drag a port to link · drag a *connected* input to
//! pick its link up · right-click a link to cut it · right-click the canvas
//! to add a node there · middle-drag (or shift-right-drag) to pan · wheel to
//! zoom · drag the preview to turn it.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use glam::Vec2;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{Window, WindowId};
use wxsl::core::graph::Graph;
use wxsl::editor::{Editor, EditorConfig};
use wxsl::render::gpu::GpuContext;
use wxsl::render::ui::input::{Key, Modifiers, MouseButton, UiEvent};
use wxsl::render::ui::{MsdfBackend, UiTarget};

/// The graph opened when `--graph` is not given: the same demo the renderer's
/// example uses, so the two show the same material.
const DEFAULT_GRAPH: &str = include_str!("../assets/pbr_cube.wxsl.json");

const USAGE: &str = "\
editor — the wxsl visual node editor

USAGE:
    cargo run --example editor -- [OPTIONS]

OPTIONS:
    --graph <FILE>        Graph to open, in the node format (default: the shipped demo)
    --font <FILE>         Proportional font for the interface
    --mono <FILE>         Monospaced font for the code panels
    --msdf <cpu|gpu>      Which MSDF backend generates glyphs (default: gpu)
    --size <WIDTHxHEIGHT> Window size (default: 1600x900)
    --atlas <PIXELS>      Glyph atlas side length (default: 2048)
    --list-fonts          Print the fonts that would be used, and exit
    --screenshot <FILE>   Render one frame headless to a PNG and exit
    -h, --help            Print this help

KEYS:
    F / D        forward / deferred render path
    M            cycle the preview mesh
    G            switch the MSDF backend (cpu <-> gpu)
    A            add a node at the middle of the canvas
    R            frame the whole graph
    Space        pause the preview's spin
    Ctrl+S       save the graph
    Delete       delete the selected nodes
    Esc          quit

POINTER:
    drag a node             move it (the whole selection moves)
    drag a port             draw a link; drop it on a compatible port
    drag a connected input  pick that link up by its other end
    right-click a link      cut it
    right-click the canvas  add a node there
    middle-drag             pan (shift + right-drag also works)
    wheel                   zoom about the pointer
    drag the preview        turn the mesh
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

    let fonts = Fonts::find(options.ui_font.as_deref(), options.mono_font.as_deref())?;
    if options.list_fonts {
        println!("interface: {}", fonts.ui_path.display());
        println!("monospace: {}", fonts.mono_path.display());
        return Ok(());
    }

    let graph = load_graph(options.graph.as_deref())?;
    if let Some(path) = options.screenshot.clone() {
        return screenshot(&options, graph, fonts, &path);
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        options,
        graph: Some(graph),
        fonts: Some(fonts),
        state: None,
        started: Instant::now(),
        modifiers: Modifiers::NONE,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Headless
// ---------------------------------------------------------------------------

/// Render a few frames with no window and write the last one to a PNG.
///
/// Worth having beyond the obvious: the editor draws itself with the renderer
/// (ADR 0013), so a screenshot is a complete test of the UI shader, the glyph
/// atlas, the instance path and the offscreen preview — reviewable in a diff,
/// and runnable on a machine with no display.
fn screenshot(
    options: &Options,
    graph: Graph,
    fonts: Fonts,
    path: &Path,
) -> Result<(), Box<dyn Error>> {
    use wxsl::render::gpu::OffscreenTarget;

    let gpu = pollster::block_on(GpuContext::headless())?;
    let (width, height) = options.size;
    let target = OffscreenTarget::new(&gpu.device, width, height);

    let mut config = EditorConfig::new(
        wxsl::stdlib_library(),
        wxsl::stdlib::registry(),
        graph,
        fonts.ui,
        fonts.mono,
    );
    config.msdf_backend = options.msdf;
    config.atlas_size = options.atlas;
    let mut editor = Editor::new(&gpu.device, config)?;
    editor.handle_event(UiEvent::Resized {
        size: Vec2::new(width as f32, height as f32),
        scale: 1.0,
    });

    // Select the graph's output node, so the picture shows the inspector
    // doing something rather than saying "no node selected".
    let output = editor
        .graph()
        .surface_outputs(editor.registry())
        .first()
        .copied()
        .or_else(|| editor.graph().nodes().map(|(id, _)| id).next());
    if let Some(node) = output {
        editor.select([node]);
    }

    // Three frames: the first fills the glyph atlas and compiles the
    // material, and the preview needs one more to have something to spin.
    for frame in 0..3 {
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("editor screenshot"),
            });
        editor.frame(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            &UiTarget {
                view: target.view(),
                format: target.format(),
                width,
                height,
                clear: None,
            },
            frame as f64 * 0.016,
        )?;
        gpu.queue.submit([encoder.finish()]);
        gpu.wait();
    }

    let pixels = target.read_rgba8(&gpu.device, &gpu.queue);
    image::save_buffer(
        path,
        &pixels,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )?;
    println!(
        "wrote {} ({width}x{height}, msdf {}, {})",
        path.display(),
        options.msdf.name(),
        match editor.preview().status() {
            wxsl::editor::PreviewStatus::Ok => "the graph compiles".to_string(),
            other => format!("{other:?}"),
        }
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

struct Options {
    graph: Option<PathBuf>,
    ui_font: Option<PathBuf>,
    mono_font: Option<PathBuf>,
    msdf: MsdfBackend,
    size: (u32, u32),
    atlas: u32,
    list_fonts: bool,
    screenshot: Option<PathBuf>,
    help: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            graph: None,
            ui_font: None,
            mono_font: None,
            // Matches `EditorConfig::new`'s own default: the editor always
            // has a device, and the compute pass is noticeably faster once
            // a batch of glyphs is more than a handful.
            msdf: MsdfBackend::Gpu,
            size: (1600, 900),
            atlas: 2048,
            list_fonts: false,
            screenshot: None,
            help: false,
        }
    }
}

impl Options {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Options::default();
        let mut args = args.peekable();
        while let Some(argument) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| format!("{argument} needs a value"))
            };
            match argument.as_str() {
                "--graph" => options.graph = Some(PathBuf::from(value()?)),
                "--font" => options.ui_font = Some(PathBuf::from(value()?)),
                "--mono" => options.mono_font = Some(PathBuf::from(value()?)),
                "--msdf" => {
                    let text = value()?;
                    options.msdf = MsdfBackend::parse(&text)
                        .ok_or_else(|| format!("unknown MSDF backend `{text}`"))?;
                }
                "--size" => {
                    let text = value()?;
                    let (width, height) = text
                        .split_once(['x', 'X'])
                        .ok_or_else(|| format!("`{text}` is not WIDTHxHEIGHT"))?;
                    options.size = (
                        width.trim().parse().map_err(|_| "bad width".to_string())?,
                        height
                            .trim()
                            .parse()
                            .map_err(|_| "bad height".to_string())?,
                    );
                }
                "--atlas" => {
                    let text = value()?;
                    options.atlas = text.trim().parse().map_err(|_| "bad atlas size")?;
                }
                "--list-fonts" => options.list_fonts = true,
                "--screenshot" => options.screenshot = Some(PathBuf::from(value()?)),
                "-h" | "--help" => options.help = true,
                other => return Err(format!("unknown option `{other}`")),
            }
        }
        Ok(options)
    }
}

fn load_graph(path: Option<&Path>) -> Result<Graph, Box<dyn Error>> {
    match path {
        Some(path) => {
            let json = std::fs::read_to_string(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            Ok(serde_json::from_str(&json)?)
        }
        None => Ok(serde_json::from_str(DEFAULT_GRAPH)?),
    }
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

/// The two faces the editor needs, and where they came from.
struct Fonts {
    ui: Vec<u8>,
    mono: Vec<u8>,
    ui_path: PathBuf,
    mono_path: PathBuf,
}

impl Fonts {
    /// Load the fonts, from the paths given or from a platform default.
    fn find(ui: Option<&Path>, mono: Option<&Path>) -> Result<Self, Box<dyn Error>> {
        let ui_path = match ui {
            Some(path) => path.to_path_buf(),
            None => first_existing(UI_FONT_CANDIDATES).ok_or_else(|| {
                format!(
                    "no interface font found; pass --font <FILE>. Looked in:\n  {}",
                    UI_FONT_CANDIDATES.join("\n  ")
                )
            })?,
        };
        let mono_path = match mono {
            Some(path) => path.to_path_buf(),
            None => first_existing(MONO_FONT_CANDIDATES).ok_or_else(|| {
                format!(
                    "no monospaced font found; pass --mono <FILE>. Looked in:\n  {}",
                    MONO_FONT_CANDIDATES.join("\n  ")
                )
            })?,
        };
        Ok(Fonts {
            ui: read_font(&ui_path)?,
            mono: read_font(&mono_path)?,
            ui_path,
            mono_path,
        })
    }
}

fn read_font(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()).into())
}

fn first_existing(candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// Where a proportional UI font usually lives.
///
/// Deliberately dumb — a short list, checked in order. A real application
/// would ask the platform's font service; an example that has to work on
/// three operating systems with no dependencies checks paths.
#[cfg(target_os = "windows")]
const UI_FONT_CANDIDATES: &[&str] = &[
    "C:/Windows/Fonts/segoeui.ttf",
    "C:/Windows/Fonts/calibri.ttf",
    "C:/Windows/Fonts/arial.ttf",
    "C:/Windows/Fonts/tahoma.ttf",
];

/// Where a monospaced font usually lives.
#[cfg(target_os = "windows")]
const MONO_FONT_CANDIDATES: &[&str] = &[
    "C:/Windows/Fonts/consola.ttf",
    "C:/Windows/Fonts/CascadiaMono.ttf",
    "C:/Windows/Fonts/lucon.ttf",
    "C:/Windows/Fonts/cour.ttf",
];

#[cfg(target_os = "macos")]
const UI_FONT_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/SFNS.ttf",
    "/System/Library/Fonts/Helvetica.ttc",
    "/Library/Fonts/Arial.ttf",
];

#[cfg(target_os = "macos")]
const MONO_FONT_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/SFNSMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Monaco.ttf",
];

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const UI_FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
];

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const MONO_FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/liberation/LiberationMono-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
];

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// Everything that only exists once there is a window.
struct State {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    gpu: GpuContext,
    format: wgpu::TextureFormat,
    editor: Editor,
}

struct App {
    options: Options,
    /// Taken when the window is created.
    graph: Option<Graph>,
    /// Taken when the window is created.
    fonts: Option<Fonts>,
    state: Option<State>,
    started: Instant,
    modifiers: Modifiers,
    error: Option<Box<dyn Error>>,
}

impl App {
    fn create_state(&mut self, event_loop: &ActiveEventLoop) -> Result<State, Box<dyn Error>> {
        let (width, height) = self.options.size;
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("wxsl — node editor")
                    .with_inner_size(winit::dpi::LogicalSize::new(width, height)),
            )?,
        );

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(Arc::clone(&window))?;
        let gpu = pollster::block_on(GpuContext::new(instance, Some(&surface)))?;

        // A non-sRGB format: the UI pass writes colours through unchanged, and
        // the preview's shading function encodes sRGB itself so that the two
        // render paths agree exactly. With an `*Srgb` surface everything would
        // be encoded twice.
        let capabilities = surface.get_capabilities(&gpu.adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
            .unwrap_or_else(|| {
                eprintln!("warning: no linear surface format; colours will be washed out");
                capabilities.formats[0]
            });

        let fonts = self
            .fonts
            .take()
            .expect("fonts are loaded before the window");
        let graph = self
            .graph
            .take()
            .expect("the graph is loaded before the window");
        let scale = window.scale_factor() as f32;
        let mut config = EditorConfig::new(
            wxsl::stdlib_library(),
            wxsl::stdlib::registry(),
            graph,
            fonts.ui,
            fonts.mono,
        );
        config.msdf_backend = self.options.msdf;
        config.atlas_size = self.options.atlas;
        config.scale = scale;

        let mut editor = Editor::new(&gpu.device, config)?;
        let size = window.inner_size();
        // The editor learns the viewport from events, so it needs the first
        // one before it can lay anything out.
        editor.handle_event(UiEvent::Resized {
            size: Vec2::new(size.width as f32, size.height as f32),
            scale,
        });

        let mut state = State {
            window,
            surface,
            gpu,
            format,
            editor,
        };
        configure_surface(&mut state, size.width, size.height);
        Ok(state)
    }

    fn render(&mut self) {
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
        let mut encoder =
            state
                .gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("editor frame"),
                });
        let result = state.editor.frame(
            &state.gpu.device,
            &state.gpu.queue,
            &mut encoder,
            &UiTarget {
                view: &view,
                format: state.format,
                width: size.width,
                height: size.height,
                // The editor paints its own background over the whole window,
                // so there is nothing to clear to.
                clear: None,
            },
            self.started.elapsed().as_secs_f64(),
        );
        if let Err(error) = result {
            eprintln!("cannot draw the editor: {error}");
        }
        state.gpu.queue.submit([encoder.finish()]);
        state.gpu.queue.present(frame);
    }

    /// Save the graph back to where it came from.
    fn save(&mut self) {
        let path = self
            .options
            .graph
            .clone()
            .unwrap_or_else(|| PathBuf::from("edited.wxsl.json"));
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match serde_json::to_string_pretty(state.editor.graph()) {
            Ok(json) => match std::fs::write(&path, json) {
                Ok(()) => {
                    state.editor.mark_saved();
                    state
                        .editor
                        .set_message(format!("saved {}", path.display()));
                }
                Err(error) => state
                    .editor
                    .set_message(format!("cannot write {}: {error}", path.display())),
            },
            Err(error) => state
                .editor
                .set_message(format!("cannot serialize the graph: {error}")),
        }
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
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match self.create_state(event_loop) {
            Ok(state) => {
                println!("{USAGE}");
                self.state = Some(state);
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
        // Translation only: every decision about what an event *means* is the
        // editor's, which is what keeps it usable from another event loop.
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::Resized(size) => {
                if let Some(state) = self.state.as_mut() {
                    configure_surface(state, size.width, size.height);
                    let scale = state.window.scale_factor() as f32;
                    state.editor.handle_event(UiEvent::Resized {
                        size: Vec2::new(size.width as f32, size.height as f32),
                        scale,
                    });
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(state) = self.state.as_mut() {
                    let size = state.window.inner_size();
                    state.editor.handle_event(UiEvent::Resized {
                        size: Vec2::new(size.width as f32, size.height as f32),
                        scale: scale_factor as f32,
                    });
                }
            }
            WindowEvent::CursorMoved { position, .. } => self.send(UiEvent::PointerMoved(
                Vec2::new(position.x as f32, position.y as f32),
            )),
            WindowEvent::CursorLeft { .. } => self.send(UiEvent::PointerLeft),
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some(button) = translate_button(button) {
                    self.send(UiEvent::PointerButton {
                        button,
                        pressed: state == ElementState::Pressed,
                    });
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let delta = match delta {
                    // A line is worth about a row of text; the editor works
                    // in pixels of content.
                    MouseScrollDelta::LineDelta(x, y) => Vec2::new(x, y) * 32.0,
                    MouseScrollDelta::PixelDelta(position) => {
                        Vec2::new(position.x as f32, position.y as f32)
                    }
                };
                self.send(UiEvent::Scroll(delta));
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = translate_modifiers(modifiers.state());
                self.send(UiEvent::ModifiersChanged(self.modifiers));
            }
            WindowEvent::Focused(focused) => self.send(UiEvent::FocusChanged(focused)),
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if pressed {
                    if let WinitKey::Named(NamedKey::Escape) = event.logical_key {
                        event_loop.exit();
                        return;
                    }
                    // Ctrl+S is the application's, not the editor's: only the
                    // application knows where the document came from.
                    if self.modifiers.command()
                        && matches!(event.logical_key.to_text(), Some("s") | Some("S"))
                    {
                        self.save();
                        return;
                    }
                }
                if let Some(key) = translate_key(&event) {
                    self.send(UiEvent::Key {
                        key,
                        pressed,
                        repeat: event.repeat,
                    });
                }
                if pressed {
                    if let Some(text) = event.text.as_ref() {
                        for character in text.chars() {
                            self.send(UiEvent::Text(character));
                        }
                    }
                }
            }
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
    fn send(&mut self, event: UiEvent) {
        if let Some(state) = self.state.as_mut() {
            state.editor.handle_event(event);
        }
    }
}

fn translate_button(button: winit::event::MouseButton) -> Option<MouseButton> {
    match button {
        winit::event::MouseButton::Left => Some(MouseButton::Left),
        winit::event::MouseButton::Right => Some(MouseButton::Right),
        winit::event::MouseButton::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

fn translate_modifiers(state: ModifiersState) -> Modifiers {
    Modifiers {
        shift: state.shift_key(),
        ctrl: state.control_key(),
        alt: state.alt_key(),
        logo: state.super_key(),
    }
}

/// Winit's key to the editor's.
///
/// Printable keys become [`Key::Char`] with the *unmodified* lowercase
/// character, so a shortcut matches however the layout spells shift; typed
/// text arrives separately as [`UiEvent::Text`].
fn translate_key(event: &winit::event::KeyEvent) -> Option<Key> {
    use winit::keyboard::KeyCode;

    if let WinitKey::Named(named) = event.logical_key {
        let key = match named {
            NamedKey::Escape => Key::Escape,
            NamedKey::Enter => Key::Enter,
            NamedKey::Tab => Key::Tab,
            NamedKey::Backspace => Key::Backspace,
            NamedKey::Delete => Key::Delete,
            NamedKey::Insert => Key::Insert,
            NamedKey::Home => Key::Home,
            NamedKey::End => Key::End,
            NamedKey::PageUp => Key::PageUp,
            NamedKey::PageDown => Key::PageDown,
            NamedKey::ArrowLeft => Key::Left,
            NamedKey::ArrowRight => Key::Right,
            NamedKey::ArrowUp => Key::Up,
            NamedKey::ArrowDown => Key::Down,
            NamedKey::Space => Key::Char(' '),
            NamedKey::F1 => Key::Function(1),
            NamedKey::F2 => Key::Function(2),
            NamedKey::F3 => Key::Function(3),
            NamedKey::F4 => Key::Function(4),
            NamedKey::F5 => Key::Function(5),
            NamedKey::F6 => Key::Function(6),
            NamedKey::F7 => Key::Function(7),
            NamedKey::F8 => Key::Function(8),
            NamedKey::F9 => Key::Function(9),
            NamedKey::F10 => Key::Function(10),
            NamedKey::F11 => Key::Function(11),
            NamedKey::F12 => Key::Function(12),
            _ => return None,
        };
        return Some(key);
    }

    // The physical key, so that `F` and `D` are the same keys whatever the
    // layout does with shift, and a shortcut is not lost to a dead key.
    if let PhysicalKey::Code(code) = event.physical_key {
        let character = match code {
            KeyCode::KeyA => 'a',
            KeyCode::KeyB => 'b',
            KeyCode::KeyC => 'c',
            KeyCode::KeyD => 'd',
            KeyCode::KeyE => 'e',
            KeyCode::KeyF => 'f',
            KeyCode::KeyG => 'g',
            KeyCode::KeyH => 'h',
            KeyCode::KeyI => 'i',
            KeyCode::KeyJ => 'j',
            KeyCode::KeyK => 'k',
            KeyCode::KeyL => 'l',
            KeyCode::KeyM => 'm',
            KeyCode::KeyN => 'n',
            KeyCode::KeyO => 'o',
            KeyCode::KeyP => 'p',
            KeyCode::KeyQ => 'q',
            KeyCode::KeyR => 'r',
            KeyCode::KeyS => 's',
            KeyCode::KeyT => 't',
            KeyCode::KeyU => 'u',
            KeyCode::KeyV => 'v',
            KeyCode::KeyW => 'w',
            KeyCode::KeyX => 'x',
            KeyCode::KeyY => 'y',
            KeyCode::KeyZ => 'z',
            KeyCode::Digit0 => '0',
            KeyCode::Digit1 => '1',
            KeyCode::Digit2 => '2',
            KeyCode::Digit3 => '3',
            KeyCode::Digit4 => '4',
            KeyCode::Digit5 => '5',
            KeyCode::Digit6 => '6',
            KeyCode::Digit7 => '7',
            KeyCode::Digit8 => '8',
            KeyCode::Digit9 => '9',
            KeyCode::Minus => '-',
            KeyCode::Equal => '=',
            KeyCode::Space => ' ',
            _ => return None,
        };
        return Some(Key::Char(character));
    }
    None
}
