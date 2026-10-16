//! [`Editor`]: the whole thing, assembled.
//!
//! Owns the graph, the interface state, the canvas, the palette and the
//! preview, and lays them out:
//!
//! ```text
//!  ┌─────────────────────────────────────────────────────────────┐
//!  │ toolbar: name · path · mesh · fit · tidy · …                │
//!  ├───────────┬──────────────────────────────┬──────────────────┤
//!  │ palette   │ node canvas                  │ preview          │
//!  │ (search,  │ (pan, zoom, link, unlink,    │ selected node    │
//!  │  category)│  move, delete)               │ (name, colour,   │
//!  │           │                              │  types, values)  │
//!  │           │                              │ macro variables  │
//!  ├───────────┴──────────────────────────────┴──────────────────┤
//!  │ WXSL │ WGSL │ problems                                      │
//!  ├─────────────────────────────────────────────────────────────┤
//!  │ status: nodes · variants · atlas · glyphs · last message    │
//!  └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! Two calls per frame from the application: [`Editor::handle_event`] for
//! each input event it saw, then [`Editor::frame`] to record the preview pass
//! and the interface pass into an encoder. The editor never opens a window
//! and never asks winit for anything (ADR 0013) — see
//! `crates/wxsl/examples/editor.rs` for that half.

use glam::Vec2;
use wxsl_core::abi;
use wxsl_core::graph::{Graph, NodeId};
use wxsl_core::lighting;
use wxsl_core::macros::MacroDef;
use wxsl_core::node::{NodeRegistry, ValueType};
use wxsl_render::ui::draw::{Color, Rect};
use wxsl_render::ui::input::{Key, MouseButton, UiEvent};
use wxsl_render::ui::text::{GlyphCache, TextOptions};
use wxsl_render::ui::{Atlas, Font, InputState, MsdfBackend, MsdfGenerator, UiRenderer, UiTarget};
use wxsl_render::{MeshKind, RenderError, ShaderLibrary, StockPipeline};

use crate::canvas::{self, Canvas};
use crate::command::{
    Answer, Command, CommandError, Document, FramePass, FrameSummary, NodeSummary, Outcome,
    PassSummary, ProbeSummary, ProbedResource, Query, StepSummary,
};
use crate::function::{self, FunctionSource, Functions};
use crate::history::{History, Host};
use crate::palette::{Match, NodePicker};
use crate::pipeline::{self, CanvasMode, PipelineCanvas, PipelineTab};
use crate::preview::Preview;
use crate::reload::ShaderWatch;
use crate::scene::{SceneChanges, SceneDoc};
use crate::scene_mode::{self, SceneMode};
use crate::theme::Theme;
use crate::ui::{Align, Id, LineMark, Ui, UiState};
use crate::views::{DebugChoice, FrameView};
use crate::widgets;

mod code;
mod host;
mod inspector;
mod panels;

use code::*;
use inspector::*;
use panels::*;

/// What the editor needs to start.
pub struct EditorConfig {
    /// The WXSL modules imports are resolved against, including the shader
    /// ABI and the UI and MSDF passes. `wxsl::stdlib_library()` is the
    /// usual answer (ADR 0009).
    pub library: ShaderLibrary,
    /// The node definitions the graph is edited against.
    pub registry: NodeRegistry,
    /// The graph to open.
    pub graph: Graph,
    /// A proportional font, for the interface.
    pub ui_font: Vec<u8>,
    /// A monospaced font, for the code panels.
    pub mono_font: Vec<u8>,
    /// Which MSDF backend generates glyph fields (ADR 0014).
    pub msdf_backend: MsdfBackend,
    /// Side of the glyph and image atlas, in pixels.
    pub atlas_size: u32,
    /// Physical pixels per logical pixel.
    pub scale: f32,
}

impl EditorConfig {
    /// A configuration with the usual defaults, needing the four things that
    /// have no sensible default.
    ///
    /// `msdf_backend` defaults to [`MsdfBackend::Gpu`] here — not
    /// [`MsdfBackend::default()`], which stays [`MsdfBackend::Cpu`] for a
    /// general `wxsl-render` consumer that may have no compute-capable
    /// device to hand. The editor always has a device by the time it opens a
    /// glyph cache, and generating hundreds of glyphs a batch is exactly the
    /// case the compute pass exists for — noticeably faster than the CPU
    /// path in the one place in this workspace that fills a real atlas at
    /// interactive speed.
    pub fn new(
        library: ShaderLibrary,
        registry: NodeRegistry,
        graph: Graph,
        ui_font: Vec<u8>,
        mono_font: Vec<u8>,
    ) -> Self {
        EditorConfig {
            library,
            registry,
            graph,
            ui_font,
            mono_font,
            msdf_backend: MsdfBackend::Gpu,
            atlas_size: 2048,
            scale: 1.0,
        }
    }
}

/// Which code panel is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodeTab {
    /// The WXSL codegen produced from the graph.
    #[default]
    Wxsl,
    /// The WGSL the active render path compiled it to.
    Wgsl,
    /// The pipeline's material features and the G-buffer budget they
    /// spend (plan3 N9) — the engine panel, not a code view.
    Features,
    /// The preview's pass list, stepped one pass at a time (plan3 N9).
    Frame,
    /// Whatever is stopping it.
    Problems,
    /// A function being written, which saving turns into a node (plan3
    /// M11).
    Function,
}

impl CodeTab {
    /// The tabs, in order.
    pub const ALL: &'static [CodeTab] = &[
        CodeTab::Wxsl,
        CodeTab::Wgsl,
        CodeTab::Features,
        CodeTab::Frame,
        CodeTab::Problems,
        CodeTab::Function,
    ];

    fn index(self) -> usize {
        CodeTab::ALL
            .iter()
            .position(|tab| *tab == self)
            .unwrap_or(0)
    }
}

/// The node editor.
pub struct Editor {
    graph: Graph,
    registry: NodeRegistry,
    input: InputState,
    ui: UiState,
    renderer: UiRenderer,
    canvas: Canvas,
    picker: NodePicker,
    preview: Preview,
    tab: CodeTab,
    message: String,
    /// The graph changed and the material needs recompiling.
    dirty: bool,
    /// The document changed and is worth saving.
    modified: bool,
    first_frame: bool,
    time: f64,
    last_frame: f64,
    backend: MsdfBackend,
    library: ShaderLibrary,
    /// Where the node canvas landed on the last drawn frame. `Rect::NOTHING`
    /// until the first frame runs.
    last_canvas_rect: Rect,
    /// Which canvas the panels and shortcuts point at.
    mode: CanvasMode,
    /// The pipeline canvas, opened on the deferred preset the first time
    /// it is asked for. `None` until then, so a session that never
    /// switches never builds one.
    pipeline: Option<PipelineCanvas>,
    /// Whether the material canvas previews through the pipeline canvas's
    /// document instead of a stock pipeline — the toolbar's third
    /// pipeline choice.
    material_uses_document: bool,
    /// Which bottom panel is showing while the pipeline canvas is up.
    pipeline_tab: PipelineTab,
    /// The shader files hot reload watches, once
    /// [`Editor::watch_shaders`] has been called.
    watch: Option<ShaderWatch>,
    /// When the watch is next polled, on the frame clock.
    next_poll: f64,
    /// Space went down as a shortcut and the pointer has not moved since:
    /// releasing it toggles the spin. Moving pans the canvas instead.
    space_tap: bool,
    /// The function buffers of the code editor (plan3 M11).
    functions: Functions,
    /// Undo and redo (ADR 0081).
    history: History,
    /// The open scene and the scene mode's state (ADR 0082).
    scene: SceneMode,
    /// The agent attached through MCP, by name (ADR 0083).
    agent: Option<String>,
    /// A `set_pipeline` waiting for the device: a stock pipeline, or
    /// `None` for the pipeline canvas's document.
    pending_pipeline: Option<Option<StockPipeline>>,
    /// A probe for the next frame to take (plan6 E2), and the one it is
    /// taking, as fractions of the viewport.
    probe_request: Option<[f32; 2]>,
    probing: Option<[f32; 2]>,
    /// What the last probed frame read.
    probe: Option<ProbeSummary>,
}

impl Editor {
    /// Set the editor up.
    ///
    /// Fails if a font cannot be read or the shader library is missing the UI
    /// or ABI modules — both worth reporting now rather than as an empty
    /// window later.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: EditorConfig,
    ) -> Result<Self, RenderError> {
        let EditorConfig {
            library,
            registry,
            mut graph,
            ui_font,
            mono_font,
            msdf_backend,
            atlas_size,
            scale,
        } = config;

        let atlas = Atlas::new(device, atlas_size);
        let mut renderer = UiRenderer::new(device, &library, &atlas)?;
        let generator = MsdfGenerator::new(device, &library, msdf_backend)?;
        let mut fonts = GlyphCache::new(generator);
        let ui_font = fonts.add_font(Font::from_bytes(ui_font, 0)?);
        let mono_font = fonts.add_font(Font::from_bytes(mono_font, 0)?);
        let preview = Preview::new(device, queue, &mut renderer, library.clone())?;

        // A graph from the node format may carry no positions at all, and a
        // pile of nodes at the origin is unusable.
        canvas::auto_layout(&mut graph, &registry);

        Ok(Editor {
            graph,
            registry,
            input: InputState::new(),
            ui: UiState::new(atlas, fonts, ui_font, mono_font, Theme::new(scale)),
            renderer,
            canvas: Canvas::new(),
            picker: NodePicker::new(),
            preview,
            tab: CodeTab::default(),
            message: String::from("ready"),
            dirty: true,
            modified: false,
            first_frame: true,
            time: 0.0,
            last_frame: 0.0,
            backend: msdf_backend,
            library,
            last_canvas_rect: Rect::NOTHING,
            mode: CanvasMode::default(),
            pipeline: None,
            material_uses_document: false,
            pipeline_tab: PipelineTab::default(),
            watch: None,
            next_poll: 0.0,
            space_tap: false,
            functions: Functions::default(),
            history: History::default(),
            scene: SceneMode::default(),
            agent: None,
            pending_pipeline: None,
            probe_request: None,
            probing: None,
            probe: None,
        })
    }

    /// Hot reload (plan3 N9): watch the `.wxsl` files under `root` — a
    /// source tree laid out by module path, such as `wxsl-stdlib/shaders`
    /// — and apply an edit to the preview within half a second, or put
    /// its errors in the problems panel. Returns how many files are
    /// watched: only those whose text is the library's module verbatim.
    pub fn watch_shaders(&mut self, root: impl AsRef<std::path::Path>) -> usize {
        let watch = ShaderWatch::new(root.as_ref(), &self.library);
        let count = watch.len();
        self.watch = Some(watch);
        count
    }

    fn poll_shaders(&mut self, device: &wgpu::Device) {
        let Some(watch) = self.watch.as_mut() else {
            return;
        };
        let changes = watch.poll();
        let Some(first) = changes.first() else {
            return;
        };
        let name = first
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let count = changes.len();
        let landed = self.preview.reload(
            device,
            changes
                .into_iter()
                .map(|change| (change.module, change.source)),
        );
        let what = if count == 1 {
            name
        } else {
            format!("{count} shaders")
        };
        self.message = if landed {
            format!("reloaded {what}")
        } else {
            format!("{what} does not compile · see problems")
        };
    }

    /// The text of the function being written in the code editor (plan3
    /// M11).
    pub fn function_text(&self) -> &str {
        self.functions.current().buffer.text()
    }

    /// What the last save found wrong with that function, by line.
    pub fn function_problems(&self) -> &[LineMark] {
        &self.functions.current().marks
    }

    /// Whether a text field or the code editor has the keyboard — when an
    /// application should pass its own shortcuts (save, quit) through to
    /// the editor rather than act on them.
    pub fn is_typing(&self) -> bool {
        self.ui.is_editing()
    }

    /// Save the function buffer the code panel shows (plan3 M11): derive
    /// its node, mount its module in the preview's library, and register
    /// the node, so the palette offers it in this session. A graph already
    /// using the node recompiles against the new text. On failure the
    /// compiler's diagnostics land on their lines in the buffer and
    /// nothing changes. Returns whether it took.
    pub fn save_function(&mut self, device: &wgpu::Device) -> bool {
        let source = self.functions.current().buffer.text().to_string();
        let library = self.preview.library();
        let derived = function::derive(&source, library).and_then(|(path, definition)| {
            function::validate(device, &path, &source, library).map(|()| (path, definition))
        });
        let (path, definition) = match derived {
            Ok(derived) => derived,
            Err(marks) => {
                self.message = match marks.first() {
                    Some(mark) => format!("line {}: {}", mark.line + 1, mark.message),
                    None => "the function does not compile".to_string(),
                };
                self.functions.current_mut().record(Err(marks));
                return false;
            }
        };
        // Through hot reload's door: every shader the running pass list
        // asks of the material is checked against the library with the
        // module in it, so a graph already using the node cannot be
        // broken by the save.
        if !self.preview.reload(device, [(path.clone(), source)]) {
            let errors = self.preview.reload_errors().join("\n");
            self.message = "the function breaks the graph · see problems".to_string();
            self.functions
                .current_mut()
                .record(Err(vec![crate::ui::LineMark {
                    line: 0,
                    column: 0,
                    message: errors,
                }]));
            return false;
        }
        let id = definition.id.clone();
        let label = definition.label.clone();
        self.registry.register(definition);
        self.library
            .insert(path, self.functions.current().buffer.text().to_string());
        if self.graph.nodes().any(|(_, node)| node.def == id) {
            self.dirty = true;
        }
        self.message = format!(
            "saved {label} as {id} · in the palette under {}",
            function::PACKAGE
        );
        self.functions.current_mut().record(Ok(id));
        true
    }

    /// Save `source` as a function, the way the code panel's save does:
    /// into the buffer already holding a function of the same name, or a
    /// new one, so the code panel shows what an agent wrote. Returns the
    /// node id, or the diagnostics by line. Not an undo step: the node
    /// stays registered for the session.
    pub fn define_function(
        &mut self,
        device: &wgpu::Device,
        source: &str,
    ) -> Result<String, Vec<LineMark>> {
        let name = function::function_name(source);
        let existing = self.functions.buffers.iter().position(|buffer| {
            name.is_some() && function::function_name(buffer.buffer.text()) == name
        });
        let source = FunctionSource::new(source);
        match existing {
            Some(index) => {
                self.functions.buffers[index] = source;
                self.functions.current = index;
            }
            None => {
                self.functions.buffers.push(source);
                self.functions.current = self.functions.buffers.len() - 1;
            }
        }
        if self.save_function(device) {
            Ok(self.functions.current().node.clone().unwrap_or_default())
        } else {
            Err(self.functions.current().marks.clone())
        }
    }

    /// The graph being edited.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Apply a command: the only way a document changes (ADR 0081). A
    /// refused command changes nothing. Every applied command is one undo
    /// step; [`Command::Undo`] and [`Command::Redo`] walk them.
    pub fn apply(&mut self, command: Command) -> Result<Outcome, CommandError> {
        self.execute(&command, false)
    }

    /// [`Editor::apply`], for the agent [`Editor::set_agent`] names: the
    /// step is marked as the agent's in the history (ADR 0083).
    pub fn apply_as_agent(&mut self, command: Command) -> Result<Outcome, CommandError> {
        self.execute_as(&command, false, true)
    }

    /// Name the agent attached to the editor, or with `None` none. While
    /// one is, the status bar says so.
    pub fn set_agent(&mut self, agent: Option<String>) {
        self.agent = agent;
    }

    /// The agent attached to the editor.
    pub fn agent(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// Answer a read-only question about the editor's state.
    pub fn query(&self, query: &Query) -> Answer {
        match query {
            Query::Graph { document } => Answer::Graph {
                graph: Host::graph(self, *document).cloned(),
            },
            Query::Diagnostics => Answer::Diagnostics {
                material: self.preview.status().errors().to_vec(),
                pipeline: self.pipeline_status().to_vec(),
                reload: self.preview.reload_errors().to_vec(),
            },
            Query::Source => Answer::Source {
                wxsl: self.preview.wxsl().to_string(),
                wgsl: self.preview.wgsl().to_string(),
            },
            Query::Nodes { document, search } => {
                let needle = search.as_deref().map(str::to_lowercase);
                let wanted = |definition: &&std::sync::Arc<wxsl_core::node::NodeDefinition>| {
                    needle.as_ref().is_none_or(|needle| {
                        [&definition.id, &definition.label, &definition.category]
                            .iter()
                            .any(|text| text.to_lowercase().contains(needle))
                    })
                };
                let nodes = match document {
                    Document::Material => self
                        .registry
                        .in_domain(self.graph.domain())
                        .filter(wanted)
                        .map(|definition| NodeSummary::of(definition))
                        .collect(),
                    Document::Pipeline => {
                        let opened = self.pipeline.as_ref().map(|state| &state.registry);
                        let fallback;
                        let registry = match opened {
                            Some(registry) => registry,
                            None => {
                                fallback = wxsl_render::pipeline_doc::document_registry(
                                    self.preview.effects(),
                                );
                                &fallback
                            }
                        };
                        registry
                            .iter()
                            .filter(wanted)
                            .map(|definition| NodeSummary::of(definition))
                            .collect()
                    }
                };
                Answer::Nodes { nodes }
            }
            Query::Passes => Answer::Passes {
                passes: self
                    .preview
                    .render_graph()
                    .passes()
                    .iter()
                    .map(|pass| PassSummary {
                        label: pass.label.clone(),
                        policy: format!("{:?}", pass.policy),
                        parameters: pass
                            .parameters
                            .iter()
                            .map(|(name, default)| {
                                let value = self
                                    .preview
                                    .pass_param(&pass.label, name)
                                    .unwrap_or(*default);
                                (name.clone(), value)
                            })
                            .collect(),
                    })
                    .collect(),
            },
            Query::Stats => Answer::Stats {
                nodes: self.graph.node_count(),
                edges: self.graph.edges().len(),
                variants: self.preview.variant_stats().0,
                frame_ms: self.preview.frame_time(),
            },
            Query::History => Answer::History {
                undo: self.history.undo_depth(),
                redo: self.history.redo_depth(),
                steps: self.history.steps(),
            },
            Query::Scene => Answer::Scene {
                scene: self.scene.doc.as_ref().map(|doc| doc.document.clone()),
                selected: self.scene.selected,
                material: self.scene.slot,
            },
            Query::Probe => Answer::Probe {
                probe: self.probe.clone(),
            },
            Query::Frame => Answer::Frame {
                frame: self.frame_summary(),
            },
            Query::View => {
                let views = DebugChoice::all(&self.preview);
                let debug_view = DebugChoice::current(&self.preview).label();
                Answer::View {
                    mode: self.mode,
                    pipeline: if self.preview.runs_document() {
                        "document".to_string()
                    } else {
                        self.preview.pipeline().name().to_string()
                    },
                    debug_view: debug_view.to_string(),
                    debug_views: views
                        .iter()
                        .filter(|(_, available)| *available)
                        .map(|(view, _)| view.label().to_string())
                        .collect(),
                    stepping: self.preview.stepping().map(|view| StepSummary {
                        passes: view.passes,
                        count: view.count,
                        resources: view
                            .resources
                            .into_iter()
                            .filter(|(_, _, drawable)| *drawable)
                            .map(|(_, label, _)| label)
                            .collect(),
                        shown: view.shown,
                    }),
                }
            }
        }
    }

    /// Run a command through the history: `gesture` when the UI emitted
    /// it, so a drag merges into one step.
    fn execute(&mut self, command: &Command, gesture: bool) -> Result<Outcome, CommandError> {
        self.execute_as(command, gesture, false)
    }

    /// [`Editor::execute`], marking the step as an `agent`'s.
    fn execute_as(
        &mut self,
        command: &Command,
        gesture: bool,
        agent: bool,
    ) -> Result<Outcome, CommandError> {
        let mut history = std::mem::take(&mut self.history);
        let result = history.execute_as(self, command, gesture, agent);
        self.history = history;
        if result.is_ok()
            && matches!(
                command,
                Command::OpenScene { .. } | Command::ImportGltf { .. }
            )
        {
            self.scene.frame_next = true;
        }
        // An undone add or a removal leaves no selected ghost behind.
        let graph = &self.graph;
        self.canvas.selection.retain(|id| graph.node(*id).is_some());
        if let Some(state) = self.pipeline.as_mut() {
            let graph = &state.graph;
            state
                .canvas
                .selection
                .retain(|id| graph.node(*id).is_some());
        }
        result
    }

    /// The node library.
    pub fn registry(&self) -> &NodeRegistry {
        &self.registry
    }

    /// Which nodes are selected.
    pub fn selection(&self) -> &[NodeId] {
        &self.canvas.selection
    }

    /// Select `nodes`, replacing whatever was selected.
    ///
    /// For an application driving the editor from its own interface — an
    /// outliner, a search result, "show me the node this error is about" —
    /// and for the screenshot mode, which uses it to put something in the
    /// inspector.
    pub fn select(&mut self, nodes: impl IntoIterator<Item = NodeId>) {
        self.canvas.selection = nodes.into_iter().collect();
    }

    /// Frame the whole graph on the next drawn frame.
    ///
    /// Deferred rather than immediate because fitting needs the canvas
    /// rectangle, and only a frame knows that.
    pub fn fit_next_frame(&mut self) {
        self.first_frame = true;
    }

    /// Which canvas the panels and shortcuts are pointed at.
    pub fn canvas_mode(&self) -> CanvasMode {
        self.mode
    }

    /// Point the editor at the other canvas.
    ///
    /// Entering the pipeline mode opens it on the deferred preset the
    /// first time — the starting point a bloom drop edits — and compiles
    /// it onto the preview's renderer at the next frame. Leaving restores
    /// the stock pipeline the material canvas chose, so the two canvases
    /// share one renderer without surprising each other.
    pub fn set_canvas_mode(&mut self, mode: CanvasMode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        match mode {
            CanvasMode::Pipeline => {
                self.pipeline
                    .get_or_insert_with(|| PipelineCanvas::open_on_preset(StockPipeline::Deferred))
                    .dirty = true;
                self.message = "pipeline canvas — the document compiles on every edit".to_string();
            }
            CanvasMode::Material | CanvasMode::Scene if self.material_uses_document => {
                self.message = format!("{} — pipeline: the document", mode.name());
            }
            CanvasMode::Material | CanvasMode::Scene => {
                self.preview.restore_stock();
                self.message = format!(
                    "{} — pipeline: {}",
                    mode.name(),
                    self.preview.pipeline().name()
                );
            }
        }
    }

    /// Whether the material canvas previews through the pipeline
    /// canvas's document rather than a stock pipeline.
    pub fn previews_document(&self) -> bool {
        self.material_uses_document
    }

    /// The pipeline document, if the canvas has been opened.
    pub fn pipeline_graph(&self) -> Option<&Graph> {
        self.pipeline.as_ref().map(|state| &state.graph)
    }

    /// The pipeline document's last compile errors; empty means the
    /// document is running.
    pub fn pipeline_status(&self) -> &[String] {
        self.pipeline
            .as_ref()
            .map(|state| state.errors.as_slice())
            .unwrap_or(&[])
    }

    /// Where the node canvas was on the last drawn frame, in physical
    /// pixels.
    ///
    /// [`Rect::NOTHING`] before the first frame. For an application that
    /// wants to convert a screen position of its own — a drag-and-drop from
    /// outside the window, say — into graph space via [`Editor::canvas_view`].
    pub fn canvas_rect(&self) -> Rect {
        self.last_canvas_rect
    }

    /// The node canvas's current pan and zoom.
    pub fn canvas_view(&self) -> &crate::canvas::View {
        &self.canvas.view
    }

    /// The preview, for its compiled source and its status.
    pub fn preview(&self) -> &Preview {
        &self.preview
    }

    /// The active theme: its palette, metrics and scale.
    pub fn theme(&self) -> Theme {
        self.ui.theme
    }

    /// Whether the document has unsaved changes.
    pub fn is_modified(&self) -> bool {
        self.modified
    }

    /// Mark the document saved.
    pub fn mark_saved(&mut self) {
        self.modified = false;
        self.message = "saved".to_string();
    }

    /// Show a message in the status bar.
    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = message.into();
    }

    /// The current status message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Which MSDF backend is generating glyphs.
    pub fn msdf_backend(&self) -> MsdfBackend {
        self.backend
    }

    /// Switch MSDF backend, regenerating every cached glyph.
    ///
    /// Both backends produce the same fields, so this is for measuring the
    /// difference or working around a driver (ADR 0014).
    pub fn set_msdf_backend(
        &mut self,
        device: &wgpu::Device,
        backend: MsdfBackend,
    ) -> Result<(), RenderError> {
        if backend == self.backend {
            return Ok(());
        }
        let generator = MsdfGenerator::new(device, &self.library, backend)?;
        self.ui.fonts.set_generator(generator, &mut self.ui.atlas);
        self.backend = backend;
        self.message = format!("MSDF backend: {}", backend.name());
        Ok(())
    }

    /// Fold one input event in.
    pub fn handle_event(&mut self, event: UiEvent) {
        if let UiEvent::Resized { scale, .. } = &event {
            self.ui.theme.set_scale(*scale);
        }
        self.input.handle(event);
    }

    /// Build and record one frame.
    ///
    /// Records two passes into `encoder`: the material preview into its
    /// offscreen target, then the interface into `target`. `time` is the
    /// application's clock in seconds — it drives the caret's blink, the
    /// preview's spin, and any material that reads the ABI's `time` input.
    pub fn frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &UiTarget<'_>,
        time: f64,
    ) -> Result<(), RenderError> {
        let dt = (time - self.last_frame).clamp(0.0, 0.1) as f32;
        self.last_frame = time;
        self.time = time;
        // The time now, and the reset *after* the frame has consumed the
        // input: events arrived before this call, and clearing them here
        // would drop every click since the last frame.
        self.input.set_time(time);

        if time >= self.next_poll {
            self.next_poll = time + 0.5;
            self.poll_shaders(device);
        }
        self.sync(device, queue);
        if let Some(at) = self.probe_request.take() {
            self.preview.renderer_mut().probe(at);
            self.probing = Some(at);
        }
        let viewport = self.last_canvas_rect;
        match self.scene.resources() {
            Some(resources) if self.mode == CanvasMode::Scene && !viewport.is_empty() => {
                self.preview.set_size(
                    device,
                    &mut self.renderer,
                    viewport.width().round() as u32,
                    viewport.height().round() as u32,
                );
                let environment = self
                    .scene
                    .environment(viewport.width() / viewport.height(), time as f32);
                self.preview
                    .render_scene(device, queue, &resources.draw_list(), &environment)?;
            }
            _ => {
                self.preview
                    .set_size(device, &mut self.renderer, Preview::SIZE, Preview::SIZE);
                self.preview.render(device, queue, dt, time as f32)?;
            }
        }
        self.collect_probe(device, queue);

        self.ui.draw.clear();
        let screen = Rect::from_min_size(Vec2::ZERO, self.input.size());
        self.build(device, queue, screen);

        let result = self
            .renderer
            .render(device, queue, encoder, target, &self.ui.draw);
        self.input.end_frame();
        result
    }

    /// Keep what a probed frame read, once it has rendered: the textures,
    /// the target's pixel, and in a scene the instance a ray through the
    /// point hits.
    fn collect_probe(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let Some(at) = self.probing else {
            return;
        };
        let Some(texels) = self.preview.renderer_mut().take_probed() else {
            return;
        };
        self.probing = None;
        let (width, height) = self.preview.size();
        let pixel = wxsl_render::probe::texel_at(at, [width, height]);
        let rgba = self.preview.read_rgba8(device, queue);
        let start = ((pixel[1] * width + pixel[0]) * 4) as usize;
        let mut shown = [0; 4];
        if let Some(bytes) = rgba.get(start..start + 4) {
            shown.copy_from_slice(bytes);
        }
        let viewport = self.last_canvas_rect;
        let instance = (self.mode == CanvasMode::Scene && !viewport.is_empty())
            .then(|| {
                self.scene.pick(
                    viewport,
                    viewport.min + Vec2::new(at[0], at[1]) * viewport.size(),
                )
            })
            .flatten();
        let material = instance.and_then(|index| {
            let doc = self.scene.doc.as_ref()?;
            Some(doc.document.instances.get(index)?.material)
        });
        let resources = texels
            .into_iter()
            .map(|texel| ProbedResource {
                resource: texel.resource,
                format: match texel.format {
                    Some(format) => serde_json::to_value(format)
                        .ok()
                        .and_then(|name| name.as_str().map(str::to_string))
                        .unwrap_or_default(),
                    None => "buffer".to_string(),
                },
                texel: texel.texel,
                layer: texel.layer,
                value: texel.value.map(|value| match value {
                    wxsl_render::probe::TexelValue::Float(channels) => {
                        channels.into_iter().map(f64::from).collect()
                    }
                    wxsl_render::probe::TexelValue::Uint(channels) => {
                        channels.into_iter().map(f64::from).collect()
                    }
                    wxsl_render::probe::TexelValue::Sint(channels) => {
                        channels.into_iter().map(f64::from).collect()
                    }
                }),
            })
            .collect();
        self.message = format!("probe at ({}, {})", pixel[0], pixel[1]);
        self.probe = Some(ProbeSummary {
            at,
            pixel,
            shown,
            resources,
            instance,
            material,
        });
    }

    /// The last frame, pass by pass, and the frame's counters.
    fn frame_summary(&self) -> FrameSummary {
        frame_summary(&self.preview, &self.scene.warnings)
    }

    /// Bring the preview up to date with the documents without drawing:
    /// recompile an edited material or pipeline document, land a pending
    /// `set_pipeline`, and load the scene's changes. [`Editor::frame`]
    /// starts with it; a host that answers queries between frames, such as
    /// the MCP server, calls it so the diagnostics and the generated code
    /// describe the latest edit.
    pub fn sync(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if let Some(pipeline) = self.pending_pipeline.take() {
            self.choose_pipeline(device, pipeline);
        }
        if self.dirty {
            let macros = self.graph.macros().clone();
            self.preview
                .rebuild(device, &self.graph, &self.registry, &macros);
            self.dirty = false;
        }
        // The pipeline canvas's compile-on-edit loop: an edit last frame —
        // or the canvas having just been opened — compiles here, against
        // the config the renderer is actually running, and the frame
        // below renders the result. That ordering is what makes a
        // pipeline edit live by construction rather than by promise.
        if self.mode == CanvasMode::Pipeline || self.material_uses_document {
            if let Some(state) = self.pipeline.as_mut() {
                if state.dirty {
                    state.dirty = false;
                    state.registry =
                        wxsl_render::pipeline_doc::document_registry(self.preview.effects());
                    let (errors, graph) = pipeline::compile(
                        &state.graph,
                        &state.registry,
                        self.preview.effects(),
                        self.preview.pipeline_config(),
                    );
                    state.errors = errors;
                    if let Some(graph) = graph {
                        if let Err(error) = self.preview.install_document(graph) {
                            state.errors = vec![error.to_string()];
                        }
                    }
                }
            }
        }
        // The scene last: a scene naming a lighting model the renderer
        // lacks widens its set, and the material is rebuilt against it.
        if self
            .scene
            .sync(device, queue, &mut self.preview, &self.registry)
        {
            let macros = self.graph.macros().clone();
            self.preview
                .rebuild(device, &self.graph, &self.registry, &macros);
        }
    }

    /// Lay the interface out and interact with it.
    fn build(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, screen: Rect) {
        let theme = self.ui.theme;
        let metrics = theme.metrics;
        let mut ui = Ui::new(&mut self.ui, &self.input, device, queue);
        ui.draw().rect(screen, theme.palette.background);

        // -- regions -----------------------------------------------------
        // Every `split_*` returns (the strip asked for, what is left).
        let (toolbar, rest) = screen.split_top(metrics.row_height * 1.7);
        let (status, rest) = rest.split_bottom(metrics.status_height);
        let (code, middle) = rest.split_bottom(metrics.bottom_panel_height);
        let (palette, middle) = middle.split_left(metrics.side_panel_width);
        let (inspector, canvas_rect) = middle.split_right(metrics.side_panel_width);
        self.last_canvas_rect = canvas_rect;

        // -- panels ------------------------------------------------------
        // The pipeline canvas reuses every one of these panels; what
        // differs per mode is the document they are over and the two
        // panels whose content is mode's own (the palette's rows, the
        // bottom panel's tabs).
        let pipeline_mode = self.mode == CanvasMode::Pipeline;
        let scene_mode = self.mode == CanvasMode::Scene;
        let mut requests = Requests::default();

        let canvas_response = if scene_mode {
            scene_mode::viewport(
                &mut ui,
                canvas_rect,
                &mut self.scene,
                &self.preview,
                &mut requests,
            );
            canvas::CanvasResponse::default()
        } else if pipeline_mode {
            let state = self
                .pipeline
                .as_mut()
                .expect("pipeline mode opened the canvas");
            let response = state.canvas.show(
                &mut ui,
                canvas_rect,
                &state.graph,
                &state.registry,
                Document::Pipeline,
            );
            if state.fit_pending {
                state
                    .canvas
                    .fit_to_graph(&state.graph, &state.registry, canvas_rect, &theme);
                state.fit_pending = false;
            }
            response
        } else {
            let response = self.canvas.show(
                &mut ui,
                canvas_rect,
                &self.graph,
                &self.registry,
                Document::Material,
            );
            if self.first_frame {
                self.canvas
                    .fit_to_graph(&self.graph, &self.registry, canvas_rect, &theme);
                self.first_frame = false;
            }
            response
        };

        if pipeline_mode {
            toolbar_panel(
                &mut ui,
                toolbar,
                &mut requests,
                &self.preview,
                (self.mode, (false, "")),
                &self
                    .pipeline
                    .as_ref()
                    .expect("pipeline mode opened the canvas")
                    .graph,
            );
        } else {
            toolbar_panel(
                &mut ui,
                toolbar,
                &mut requests,
                &self.preview,
                (
                    self.mode,
                    (
                        self.material_uses_document,
                        self.pipeline
                            .as_ref()
                            .map_or("deferred", |state| state.graph.name()),
                    ),
                ),
                &self.graph,
            );
        }

        // The palette's rows and categories, computed per mode: the
        // material registry filtered by the graph's domain, or the
        // document registry plus one row per screen effect.
        let (matches, categories, node_count) = if pipeline_mode {
            let state = self
                .pipeline
                .as_ref()
                .expect("pipeline mode opened the canvas");
            (
                pipeline::palette_matches(&state.picker, &state.registry, self.preview.effects()),
                pipeline::palette_categories(&state.registry, self.preview.effects()),
                state.registry.len(),
            )
        } else {
            (
                self.picker
                    .matches(&self.registry, self.graph.domain(), 200),
                self.registry
                    .categories_in(self.graph.domain())
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                self.registry.len(),
            )
        };
        if scene_mode {
            scene_mode::outliner_panel(&mut ui, palette, &mut self.scene, &mut requests);
        } else if pipeline_mode {
            let state = self
                .pipeline
                .as_mut()
                .expect("pipeline mode opened the canvas");
            palette_panel(
                &mut ui,
                palette,
                &mut state.picker,
                matches,
                &categories,
                node_count,
                &mut requests,
            );
        } else {
            palette_panel(
                &mut ui,
                palette,
                &mut self.picker,
                matches,
                &categories,
                node_count,
                &mut requests,
            );
        }

        if scene_mode {
            scene_mode::inspector_panel(&mut ui, inspector, &self.scene, &mut requests);
        } else if pipeline_mode {
            let state = self
                .pipeline
                .as_mut()
                .expect("pipeline mode opened the canvas");
            pipeline::inspector_panel(
                &mut ui,
                inspector,
                state,
                &mut self.preview,
                state.canvas.selected_node(),
                &mut requests,
            );
        } else {
            inspector_panel(
                &mut ui,
                inspector,
                &self.preview,
                &self.graph,
                &self.registry,
                self.canvas.selected_node(),
                &mut requests,
            );
        }

        if pipeline_mode {
            // Field-level, not `self.pipeline_status()`: the interface
            // holds a borrow of `self.ui`, and a `&self` method would
            // overlap it.
            let errors = self
                .pipeline
                .as_ref()
                .map(|state| state.errors.as_slice())
                .unwrap_or(&[]);
            self.pipeline_tab = pipeline::passes_panel(
                &mut ui,
                code,
                self.pipeline_tab,
                &self.preview,
                errors,
                &mut requests,
            );
        } else {
            let frame = (self.tab == CodeTab::Frame)
                .then(|| frame_summary(&self.preview, &self.scene.warnings));
            self.tab = code_panel(
                &mut ui,
                code,
                self.tab,
                &self.preview,
                FrameTab {
                    frame: frame.as_ref(),
                    probe: self.probe.as_ref(),
                },
                &mut self.functions,
                &mut requests,
            );
        }
        if pipeline_mode {
            let state = self
                .pipeline
                .as_ref()
                .expect("pipeline mode opened the canvas");
            let document = StatusBarDocument {
                text: format!(
                    "{} nodes · {} links · {} passes",
                    state.graph.node_count(),
                    state.graph.edges().len(),
                    self.preview.render_graph().passes().len(),
                ),
                healthy: state.errors.is_empty(),
            };
            status_bar(
                &mut ui,
                status,
                &document,
                &self.preview,
                self.backend,
                (&self.message, self.agent.as_deref()),
                self.modified,
            );
        } else {
            let document = match (scene_mode, self.scene.doc.as_ref()) {
                (true, Some(doc)) => StatusBarDocument {
                    text: format!(
                        "{} instances · {} materials · {} meshes",
                        doc.document.instances.len(),
                        doc.document.materials.len(),
                        doc.document.meshes.len(),
                    ),
                    healthy: self.scene.error.is_none(),
                },
                _ => StatusBarDocument {
                    text: format!(
                        "{} nodes · {} links",
                        self.graph.node_count(),
                        self.graph.edges().len(),
                    ),
                    healthy: self.preview.status().is_ok(),
                },
            };
            status_bar(
                &mut ui,
                status,
                &document,
                &self.preview,
                self.backend,
                (&self.message, self.agent.as_deref()),
                self.modified,
            );
        }

        // -- open dropdown -------------------------------------------------
        // Over every panel; the drag ghost below still wins, being the
        // pointer's own.
        ui.show_popup(screen);

        // -- drag ghost ---------------------------------------------------
        // Drawn last (and so on top, and unclipped by any panel's own
        // scroll region) so a node dragged from the palette visibly follows
        // the pointer over the canvas, tinted to show whether letting go
        // here would actually place it.
        if let Some(label) = requests.dragging_definition.clone() {
            let point = ui.input.pointer_or_zero();
            let over_canvas = canvas_rect.contains(point);
            let size = ui.measure_ui(&label) + Vec2::splat(metrics.padding);
            let ghost = Rect::from_min_size(point + Vec2::splat(14.0), size);
            let fill = if over_canvas {
                theme.palette.accent
            } else {
                theme.palette.control_active
            };
            ui.draw()
                .round_rect(ghost, metrics.radius, fill.with_alpha(0.92));
            ui.draw().round_rect_border(
                ghost,
                metrics.radius,
                metrics.outline_width,
                theme.palette.outline,
            );
            ui.label(
                ghost.shrink(metrics.padding * 0.5),
                &label,
                if over_canvas {
                    theme.palette.text_on_accent
                } else {
                    theme.palette.text
                },
                Align::Center,
            );
        }

        // -- shortcuts ---------------------------------------------------
        if !ui.state.is_editing() {
            // The code editor's buffers keep their own undo while typing.
            for press in ui.input.keys() {
                let command = press.modifiers.command() && !press.modifiers.alt;
                match press.key {
                    Key::Char('z') if command && press.modifiers.shift => {
                        requests.commands.push(Command::Redo)
                    }
                    Key::Char('z') if command => requests.commands.push(Command::Undo),
                    Key::Char('y') if command => requests.commands.push(Command::Redo),
                    _ => {}
                }
            }
            // The stock pipelines are the material canvas's knobs; the
            // pipeline canvas's document *is* the pipeline, so the keys
            // would lie there.
            if !pipeline_mode {
                if ui.input.key_pressed_plain(Key::Char('f')) {
                    requests.pipeline = Some(StockPipeline::Forward);
                }
                if ui.input.key_pressed_plain(Key::Char('d')) {
                    requests.pipeline = Some(StockPipeline::Deferred);
                }
            }
            if ui.input.key_pressed_plain(Key::Char('p')) {
                requests.mode = Some(self.mode.other());
            }
            if ui.input.key_pressed_plain(Key::Char('m')) {
                requests.mesh = Some(self.preview.mesh_kind().next());
            }
            if ui.input.key_pressed_plain(Key::Char('a')) && !scene_mode {
                let at = canvas_rect.center();
                requests.add_at = Some(if pipeline_mode {
                    self.pipeline
                        .as_ref()
                        .expect("pipeline mode opened the canvas")
                        .canvas
                        .view
                        .to_graph(canvas_rect, at)
                } else {
                    self.canvas.view.to_graph(canvas_rect, at)
                });
            }
            if ui.input.key_pressed_plain(Key::Char('r')) {
                requests.fit = true;
            }
            let space = Key::Char(' ');
            // Not on a repeat: that would re-arm the tap after a pan.
            if ui
                .input
                .keys()
                .iter()
                .any(|press| press.key == space && !press.repeat && press.modifiers.is_none())
            {
                self.space_tap = true;
            }
            if ui.input.key_down(space) && ui.input.pointer_delta() != Vec2::ZERO {
                self.space_tap = false;
            }
            if ui.input.key_released(space) && std::mem::take(&mut self.space_tap) {
                requests.toggle_spin = true;
            }
        }
        let error = ui.error().map(|error| error.to_string());
        // Every widget has had its turn; safe to release a stale `active`
        // now without racing the click it was supposed to report.
        ui.end_frame();

        // -- apply -------------------------------------------------------
        if let Some(message) = canvas_response.message {
            self.message = message;
        }
        let document = if pipeline_mode {
            Document::Pipeline
        } else {
            Document::Material
        };
        if let Some(delta) = requests.spin_drag {
            self.preview.drag(delta);
        }
        if let Some(at) = canvas_response.add_node_at.or(requests.add_at) {
            if pipeline_mode {
                self.pipeline
                    .as_mut()
                    .expect("pipeline mode opened the canvas")
                    .picker
                    .open_at(at);
            } else {
                self.picker.open_at(at);
            }
            self.ui.focus_text(Id::new("palette.search"), "");
            self.message = "pick a node to add".to_string();
        }
        if let Some(add) = requests.add.take() {
            let (view, picker_target) = if pipeline_mode {
                let state = self
                    .pipeline
                    .as_ref()
                    .expect("pipeline mode opened the canvas");
                (&state.canvas.view, state.picker.target)
            } else {
                (&self.canvas.view, self.picker.target)
            };
            let at =
                resolve_add_position(requests.drop_screen_point, canvas_rect, view, picker_target);
            let (definition, preset) = (add.definition, add.preset);
            let mut node =
                wxsl_core::graph::Node::new(definition.as_str()).with_position(at.to_array());
            if let Some((name, value)) = preset {
                node = node.with_setting(name, value);
            }
            match self.execute(&Command::AddNode { document, node }, false) {
                Ok(outcome) => {
                    let canvas = if pipeline_mode {
                        &mut self
                            .pipeline
                            .as_mut()
                            .expect("pipeline mode opened the canvas")
                            .canvas
                    } else {
                        &mut self.canvas
                    };
                    canvas.selection = outcome.added;
                    self.message = format!("added {definition}");
                }
                Err(error) => self.message = error.to_string(),
            }
            if pipeline_mode {
                self.pipeline
                    .as_mut()
                    .expect("pipeline mode opened the canvas")
                    .picker
                    .close();
            } else {
                self.picker.close();
            }
            self.ui.clear_focus();
        }
        if requests.tidy {
            requests.commands.push(Command::TidyLayout { document });
        }
        for command in canvas_response
            .commands
            .into_iter()
            .chain(std::mem::take(&mut requests.commands))
        {
            match self.execute(&command, true) {
                Ok(outcome) => {
                    if let Command::SetGeneric { param, ty, .. } = &command {
                        if !outcome.disconnected.is_empty() {
                            self.message = format!(
                                "{param} is now {ty}; disconnected {} edge(s) that no longer fit",
                                outcome.disconnected.len()
                            );
                        }
                    }
                    match command {
                        Command::Undo => self.message = "undone".to_string(),
                        Command::Redo => self.message = "redone".to_string(),
                        Command::TidyLayout { .. } => {
                            let count = Host::graph(self, document).map_or(0, Graph::node_count);
                            self.message = format!("tidied {count} nodes");
                            requests.fit = true;
                        }
                        _ => {}
                    }
                }
                Err(error) => self.message = error.to_string(),
            }
        }
        // A drag, a slider or a text field is one undo step until the
        // pointer is up and nothing has the keyboard.
        if !self.input.is_down(MouseButton::Left) && !self.ui.is_editing() {
            self.history.seal();
        }
        if let Some(mode) = requests.mode {
            self.set_canvas_mode(mode);
        }
        if requests.fit {
            if scene_mode {
                self.scene.frame();
            } else if pipeline_mode {
                let state = self
                    .pipeline
                    .as_mut()
                    .expect("pipeline mode opened the canvas");
                state
                    .canvas
                    .fit_to_graph(&state.graph, &state.registry, canvas_rect, &theme);
            } else {
                self.canvas
                    .fit_to_graph(&self.graph, &self.registry, canvas_rect, &theme);
            }
        }
        if requests.use_document {
            self.choose_pipeline(device, None);
        }
        if let Some(pipeline) = requests.pipeline {
            self.choose_pipeline(device, Some(pipeline));
        }
        if let Some(step) = requests.step {
            let _ = self.step(step);
        }
        if let Some(compare) = requests.compare {
            match self.preview.set_compare(device, compare) {
                Ok(()) => {
                    self.message = match compare {
                        Some(pipeline) => format!("A/B: B is {pipeline}"),
                        None => "A/B: off".to_string(),
                    };
                }
                Err(error) => self.message = error.to_string(),
            }
        }
        if let Some(split) = requests.split {
            self.preview.split = split.clamp(0.0, 1.0);
        }
        if let Some(view) = requests.debug_view {
            let _ = self.show_debug_view(view);
        }
        if let Some(mesh) = requests.mesh {
            self.preview.set_mesh(device, mesh);
            self.message = format!("preview mesh: {}", mesh.name());
        }
        if requests.toggle_spin {
            self.preview.spinning = !self.preview.spinning;
        }
        if requests.save_function {
            self.save_function(device);
        }
        if let Some(names) = requests.features.take() {
            // The material is regenerated right after: the plan the
            // checkboxes widened is the plan it must resolve against, and
            // the handshake checks both sides by name.
            match self.preview.set_features(&names) {
                Ok(()) => {
                    self.dirty = true;
                    // A document's pass list was compiled against the old
                    // plan: its G-buffer lacks (or keeps) the channel. So
                    // were the scene's materials.
                    if let Some(state) = self.pipeline.as_mut() {
                        state.dirty = true;
                    }
                    self.scene.pending.reload = true;
                    self.message = if names.is_empty() {
                        "features: none".to_string()
                    } else {
                        format!("features: {}", names.join(", "))
                    };
                }
                Err(error) => self.message = error.to_string(),
            }
        }
        if let Some(label) = requests.mark_pass.take() {
            self.preview.mark_pass(&label);
            self.message = format!("run requested: {label}");
        }
        if requests.reset_runs {
            self.preview.reset_runs();
            self.message = "run counts reset".to_string();
        }
        if requests.toggle_theme {
            self.ui.theme.toggle_mode();
        }
        if requests.fit && !scene_mode {
            self.canvas
                .fit_to_graph(&self.graph, &self.registry, canvas_rect, &theme);
        }
        if let Some(message) = requests.message {
            self.message = message;
        }
        if let Some(error) = error {
            self.message = error;
        }
    }
}

impl Editor {
    /// Preview through a stock pipeline, or with `None` through the
    /// pipeline canvas's document.
    fn choose_pipeline(&mut self, device: &wgpu::Device, pipeline: Option<StockPipeline>) {
        let Some(pipeline) = pipeline else {
            self.material_uses_document = true;
            self.pipeline
                .get_or_insert_with(|| PipelineCanvas::open_on_preset(StockPipeline::Deferred))
                .dirty = true;
            self.message = "pipeline: the pipeline canvas's document".to_string();
            return;
        };
        self.material_uses_document = false;
        // A surface view the new pipeline has no pass to show shades
        // again: forward has no lighting pass, deferred no forward one.
        let stage = lighting::debug_views()
            .into_iter()
            .find(|view| view.id == self.preview.debug_view())
            .map(|view| view.stage);
        let unshown = match pipeline {
            StockPipeline::Forward => lighting::DebugStage::Deferred,
            _ => lighting::DebugStage::Forward,
        };
        if stage == Some(unshown) {
            let _ = self.show_debug_view(DebugChoice::Shaded);
        }
        self.preview.set_pipeline(device, pipeline);
        self.message = match self.preview.swap_progress() {
            Some(progress) => format!("pipeline: {pipeline} ({progress})"),
            None => format!("pipeline: {pipeline}"),
        };
    }

    /// Show a debug view, or shade again.
    fn show_debug_view(&mut self, view: DebugChoice) -> Result<(), String> {
        // A surface view is a material macro the lighting pass inherits:
        // the material is regenerated, and the lighting pass with it, and
        // so is every scene material.
        // Overdraw is a surface view, a constant, that its frame view
        // counts.
        let surface = match view {
            DebugChoice::Surface(id) => id,
            DebugChoice::Frame(FrameView::Overdraw) => lighting::debug_views()
                .into_iter()
                .find(|view| view.label == lighting::OVERDRAW_VIEW)
                .map_or(0, |view| view.id),
            _ => 0,
        };
        if surface != self.preview.debug_view() {
            self.preview.set_debug_view(surface);
            self.dirty = true;
            self.scene.retint_materials();
        }
        let frame = match view {
            DebugChoice::Frame(view) => Some(view),
            _ => None,
        };
        let result = if frame != self.preview.frame_view() {
            self.preview.set_frame_view(frame)
        } else {
            Ok(())
        };
        self.message = match &result {
            Ok(()) => format!("view: {}", view.label()),
            Err(error) => error.clone(),
        };
        result
    }

    /// Start, move or stop pass stepping, saying where it landed.
    fn step(&mut self, step: StepRequest) -> Result<(), RenderError> {
        let result = match step {
            StepRequest::Start => self.preview.start_stepping(),
            StepRequest::To(count, show) => self.preview.step_to(count, show),
            StepRequest::Stop => {
                self.preview.stop_stepping();
                Ok(())
            }
        };
        self.message = match &result {
            Ok(()) => match self.preview.stepping() {
                Some(view) => format!("frame: pass {} of {}", view.count, view.passes.len()),
                None => "frame: live".to_string(),
            },
            Err(error) => error.to_string(),
        };
        result
    }
}

/// What the frame panel asks of the preview's pass stepping.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum StepRequest {
    /// Freeze the frame and step.
    Start,
    /// Run this many passes, showing this resource (or the last pass's
    /// output).
    To(usize, Option<wxsl_render::pass::ResourceId>),
    /// Back to the live pass list.
    Stop,
}

/// What one frame's widgets asked the editor to do.
///
/// Collected rather than applied in place, because a widget deep in a panel
/// holds a borrow of the interface and the graph, and half of these need the
/// device or the preview. Crate-visible because the pipeline canvas's
/// panels (its inspector above all) ask through the same channel.
#[derive(Default)]
pub(crate) struct Requests {
    pub(crate) pipeline: Option<StockPipeline>,
    /// Preview the material through the pipeline canvas's document.
    pub(crate) use_document: bool,
    /// Show this debug view.
    pub(crate) debug_view: Option<DebugChoice>,
    /// Put the A/B split up with this B-side pipeline, or take it down.
    pub(crate) compare: Option<Option<StockPipeline>>,
    /// Move the A/B split's edge to this fraction of the preview's width.
    pub(crate) split: Option<f32>,
    /// Start, move or stop pass stepping.
    pub(crate) step: Option<StepRequest>,
    pub(crate) mesh: Option<MeshKind>,
    /// Switch which canvas the panels point at.
    pub(crate) mode: Option<CanvasMode>,
    /// Open the palette to add a node at this graph-space point.
    pub(crate) add_at: Option<Vec2>,
    /// A node a palette row asked for.
    pub(crate) add: Option<AddNode>,
    /// Where the row was let go, in screen pixels — a palette row is a
    /// drag source, and this is where the drag ended up. `None` when the
    /// node was requested some other way (Enter in the search box, the
    /// toolbar button), in which case the existing `picker.target`/canvas
    /// centre fallback applies.
    pub(crate) drop_screen_point: Option<Vec2>,
    /// A palette row is being dragged this frame; its label, for the ghost
    /// that follows the pointer.
    pub(crate) dragging_definition: Option<String>,
    /// The edits the panels asked for, applied in order after the frame
    /// (ADR 0081).
    pub(crate) commands: Vec<Command>,
    pub(crate) toggle_spin: bool,
    pub(crate) toggle_theme: bool,
    /// Turn the preview by this many pixels of drag.
    pub(crate) spin_drag: Option<f32>,
    pub(crate) fit: bool,
    /// Save the function buffer showing, making it a node (plan3 M11).
    pub(crate) save_function: bool,
    /// Lay the canvas's nodes out again, then fit.
    pub(crate) tidy: bool,
    /// The full set of material features the pipeline should carry, as the
    /// features panel's checkboxes left it (plan3 N9): applied as one
    /// `set_features`, so a toggle is a pass-list rebuild rather than a
    /// per-box increment.
    pub(crate) features: Option<Vec<&'static str>>,
    /// Run the pass labelled this on the next frame — the policy
    /// inspector's "run now".
    pub(crate) mark_pass: Option<String>,
    /// Make every `once` and `on resize` pass due again — the policy
    /// inspector's reset.
    pub(crate) reset_runs: bool,
    /// Show this in the status bar instead of building a message at the
    /// apply site — for a widget (the inspector's generic-type picker) whose
    /// outcome depends on what it did, not just that it ran.
    pub(crate) message: Option<String>,
}

/// A node a palette row asked for: the definition to place, and — for the
/// pipeline palette's effect rows — a setting it should arrive with, so
/// dropping "bloom" is a `pass.screen` that already names its effect.
#[derive(Clone)]
pub(crate) struct AddNode {
    pub(crate) definition: String,
    pub(crate) preset: Option<(String, String)>,
}

/// Where to add a node requested by `requests.add_definition`.
///
/// A palette row dropped on the canvas wins over anything else: it is the
/// most specific placement a user just gave, by dragging it there. Failing
/// that (a plain click on a row, which is a drag released with zero delta
/// and so still inside the palette; or a drag let go somewhere that is
/// neither the palette nor the canvas), fall back to wherever the palette
/// was opened for — a right-click on the canvas, or the `A` shortcut both
/// set `picker_target` — and only then to the canvas centre.
///
/// A free function over plain data (no `Editor`, no `Ui`) so the placement
/// rule is exercised directly, with no device and no frame to build.
fn resolve_add_position(
    drop_screen_point: Option<Vec2>,
    canvas_rect: Rect,
    view: &canvas::View,
    picker_target: Option<Vec2>,
) -> Vec2 {
    drop_screen_point
        .filter(|point| canvas_rect.contains(*point))
        .map(|point| view.to_graph(canvas_rect, point))
        .or(picker_target)
        .unwrap_or_else(|| view.to_graph(canvas_rect, canvas_rect.center()))
}

/// What the frame tab shows while the frame runs live.
#[derive(Clone, Copy)]
struct FrameTab<'a> {
    /// The last frame, when the tab is showing.
    frame: Option<&'a FrameSummary>,
    /// The last probe.
    probe: Option<&'a ProbeSummary>,
}

/// What the status bar says about the active document, and whether its
/// own compile is healthy — the material's when the material canvas is
/// up, the pipeline document's when it is not, so a broken document
/// colours the bar even while the last good pass list is still rendering.
struct StatusBarDocument {
    /// The document half of the left side: nodes, links, and — for a
    /// pipeline — the passes it compiled to.
    text: String,
    healthy: bool,
}

/// The widget id an input's value editor takes, so the pass that *reserves*
/// its height and the pass that draws it agree — an open colour picker makes
/// the row taller, and the height comes from the same id the editor reads.
fn param_widget_id(node: NodeId, socket: &wxsl_core::node::Socket) -> Id {
    Id::new("inspector.param")
        .with(u64::from(node.0))
        .with(Id::new(socket.name.as_str()).0)
}

/// The id of a node's own colour swatch, whose picker is opened the same way
/// a socket value's is.
fn node_color_id(node: NodeId) -> Id {
    Id::new("inspector.node_color").with(u64::from(node.0))
}

/// How many columns and rows a type-picker's buttons take at `width`.
///
/// A generic parameter can allow anything from one type to every operand
/// type WGSL has, and `mat3x3f` needs several times the width of `f32`, so
/// the buttons wrap onto as many rows as they need instead of being squeezed
/// into one unreadable strip.
fn picker_grid(count: usize, width: f32, gap: f32, button: f32) -> (usize, usize) {
    let fits = ((width + gap) / (button + gap)).floor().max(1.0) as usize;
    let columns = fits.min(count.max(1));
    (columns, count.div_ceil(columns))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_tabs_round_trip_through_their_index() {
        for tab in CodeTab::ALL {
            assert_eq!(CodeTab::ALL[tab.index()], *tab);
        }
        assert_eq!(CodeTab::default(), CodeTab::Wxsl);
    }

    #[test]
    fn a_drop_on_the_canvas_places_the_node_at_the_drop_point() {
        let canvas_rect = Rect::new(300.0, 0.0, 800.0, 600.0);
        let view = canvas::View::default();
        let screen_point = Vec2::new(500.0, 200.0);

        let at = resolve_add_position(Some(screen_point), canvas_rect, &view, None);
        assert_eq!(at, view.to_graph(canvas_rect, screen_point));
    }

    #[test]
    fn a_plain_click_still_inside_the_palette_falls_back_to_the_picker_target() {
        let canvas_rect = Rect::new(300.0, 0.0, 800.0, 600.0);
        let view = canvas::View::default();
        // A click that never left the palette row: the release point is
        // well outside the canvas.
        let inside_palette = Vec2::new(50.0, 200.0);
        let picker_target = Vec2::new(11.0, 22.0);

        let at = resolve_add_position(
            Some(inside_palette),
            canvas_rect,
            &view,
            Some(picker_target),
        );
        assert_eq!(at, picker_target);
    }

    #[test]
    fn with_no_drop_and_no_target_the_node_lands_at_the_canvas_centre() {
        let canvas_rect = Rect::new(300.0, 0.0, 800.0, 600.0);
        let view = canvas::View::default();

        let at = resolve_add_position(None, canvas_rect, &view, None);
        assert_eq!(at, view.to_graph(canvas_rect, canvas_rect.center()));
    }

    #[test]
    fn a_drop_wins_over_a_picker_target_even_when_both_are_set() {
        // The palette drag is the more specific placement a user just gave,
        // so it must win even if a right-click earlier also set a target.
        let canvas_rect = Rect::new(300.0, 0.0, 800.0, 600.0);
        let view = canvas::View::default();
        let drop = Vec2::new(600.0, 300.0);
        let stale_target = Vec2::new(999.0, 999.0);

        let at = resolve_add_position(Some(drop), canvas_rect, &view, Some(stale_target));
        assert_eq!(at, view.to_graph(canvas_rect, drop));
    }

    #[test]
    fn a_default_config_needs_only_what_has_no_default() {
        let config = EditorConfig::new(
            ShaderLibrary::new(),
            NodeRegistry::new(),
            Graph::new("test"),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(config.atlas_size, 2048);
        assert_eq!(config.scale, 1.0);
        // Gpu, not `MsdfBackend::default()` (which stays Cpu for a general
        // `wxsl-render` consumer): the editor always has a device, and a
        // batch of glyphs is exactly the case the compute pass is faster at.
        assert_eq!(config.msdf_backend, MsdfBackend::Gpu);
    }
}
