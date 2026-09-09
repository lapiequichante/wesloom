//! [`Editor`]: the whole thing, assembled.
//!
//! Owns the graph, the interface state, the canvas, the palette and the
//! preview, and lays them out:
//!
//! ```text
//!  ┌─────────────────────────────────────────────────────────────┐
//!  │ toolbar: name · path · mesh · MSDF backend · add · fit      │
//!  ├───────────┬──────────────────────────────┬──────────────────┤
//!  │ palette   │ node canvas                  │ preview          │
//!  │ (search,  │ (pan, zoom, link, unlink,    │ selected node    │
//!  │  category)│  move, delete)               │ macro variables  │
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
use wxsl_core::macros::MacroDef;
use wxsl_core::node::NodeRegistry;
use wxsl_render::ui::draw::{Color, Rect};
use wxsl_render::ui::input::{Key, UiEvent};
use wxsl_render::ui::text::{GlyphCache, TextOptions};
use wxsl_render::ui::{Atlas, Font, InputState, MsdfBackend, MsdfGenerator, UiRenderer, UiTarget};
use wxsl_render::{MeshKind, RenderError, RenderPath, ShaderLibrary};

use crate::canvas::{self, Canvas};
use crate::palette::NodePicker;
use crate::preview::Preview;
use crate::theme::Theme;
use crate::ui::{Align, Id, Ui, UiState};
use crate::widgets;

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
    /// Whatever is stopping it.
    Problems,
}

impl CodeTab {
    /// The tabs, in order.
    pub const ALL: &'static [CodeTab] = &[CodeTab::Wxsl, CodeTab::Wgsl, CodeTab::Problems];

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
}

impl Editor {
    /// Set the editor up.
    ///
    /// Fails if a font cannot be read or the shader library is missing the UI
    /// or ABI modules — both worth reporting now rather than as an empty
    /// window later.
    pub fn new(device: &wgpu::Device, config: EditorConfig) -> Result<Self, RenderError> {
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
        let preview = Preview::new(device, &mut renderer, library.clone())?;

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
        })
    }

    /// The graph being edited.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// The graph, for a caller that wants to change it — a loaded file, or an
    /// application's own command. Marks it for recompilation.
    pub fn graph_mut(&mut self) -> &mut Graph {
        self.dirty = true;
        self.modified = true;
        &mut self.graph
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

        if self.dirty {
            let macros = self.graph.macros().clone();
            self.preview
                .rebuild(device, &self.graph, &self.registry, &macros);
            self.dirty = false;
        }
        self.preview.render(device, queue, dt, time as f32)?;

        self.ui.draw.clear();
        let screen = Rect::from_min_size(Vec2::ZERO, self.input.size());
        self.build(device, queue, screen);

        let result = self
            .renderer
            .render(device, queue, encoder, target, &self.ui.draw);
        self.input.end_frame();
        result
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
        let mut requests = Requests::default();
        toolbar_panel(
            &mut ui,
            toolbar,
            &mut requests,
            &self.preview,
            self.backend,
            &mut self.graph,
        );

        let canvas_response =
            self.canvas
                .show(&mut ui, canvas_rect, &mut self.graph, &self.registry);
        if self.first_frame {
            self.canvas
                .fit_to_graph(&self.graph, &self.registry, canvas_rect, &theme);
            self.first_frame = false;
        }

        palette_panel(
            &mut ui,
            palette,
            &mut self.picker,
            &self.registry,
            &mut requests,
        );
        inspector_panel(
            &mut ui,
            inspector,
            &self.preview,
            &mut self.graph,
            &self.registry,
            self.canvas.selected_node(),
            &mut requests,
        );
        let tab = code_panel(&mut ui, code, self.tab, &self.preview);
        status_bar(
            &mut ui,
            status,
            &self.graph,
            &self.preview,
            self.backend,
            &self.message,
            self.modified,
        );

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
            if ui.input.key_pressed_plain(Key::Char('f')) {
                requests.path = Some(RenderPath::Forward);
            }
            if ui.input.key_pressed_plain(Key::Char('d')) {
                requests.path = Some(RenderPath::Deferred);
            }
            if ui.input.key_pressed_plain(Key::Char('m')) {
                requests.mesh = Some(self.preview.mesh_kind().next());
            }
            if ui.input.key_pressed_plain(Key::Char('g')) {
                requests.backend = Some(self.backend.toggled());
            }
            if ui.input.key_pressed_plain(Key::Char('a')) {
                requests.add_at =
                    Some(self.canvas.view.to_graph(canvas_rect, canvas_rect.center()));
            }
            if ui.input.key_pressed_plain(Key::Char('r')) {
                requests.fit = true;
            }
            if ui.input.key_pressed_plain(Key::Char(' ')) {
                requests.toggle_spin = true;
            }
        }
        let error = ui.error().map(|error| error.to_string());
        // Every widget has had its turn; safe to release a stale `active`
        // now without racing the click it was supposed to report.
        ui.end_frame();

        // -- apply -------------------------------------------------------
        self.tab = tab;
        if let Some(message) = canvas_response.message {
            self.message = message;
        }
        if canvas_response.changed {
            self.dirty = true;
            self.modified = true;
        }
        if requests.add_at_center {
            requests.add_at = Some(self.canvas.view.to_graph(canvas_rect, canvas_rect.center()));
        }
        if let Some(delta) = requests.spin_drag {
            self.preview.drag(delta);
        }
        if let Some(at) = canvas_response.add_node_at.or(requests.add_at) {
            self.picker.open_at(at);
            self.ui.focus_text(Id::new("palette.search"), "");
            self.message = "pick a node to add".to_string();
        }
        if let Some(definition) = requests.add_definition {
            let at = resolve_add_position(
                requests.drop_screen_point,
                canvas_rect,
                &self.canvas.view,
                self.picker.target,
            );
            let id = canvas::place_new_node(&mut self.graph, &definition, at);
            self.canvas.selection = vec![id];
            self.picker.close();
            self.ui.clear_focus();
            self.dirty = true;
            self.modified = true;
            self.message = format!("added {definition}");
        }
        if requests.changed_graph {
            self.dirty = true;
            self.modified = true;
        }
        if let Some(path) = requests.path {
            self.preview.set_path(device, path);
            self.message = format!("render path: {path}");
        }
        if let Some(mesh) = requests.mesh {
            self.preview.set_mesh(device, mesh);
            self.message = format!("preview mesh: {}", mesh.name());
        }
        if let Some(backend) = requests.backend {
            if let Err(error) = self.set_msdf_backend(device, backend) {
                self.message = error.to_string();
            }
        }
        if requests.toggle_spin {
            self.preview.spinning = !self.preview.spinning;
        }
        if requests.fit {
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

/// What one frame's widgets asked the editor to do.
///
/// Collected rather than applied in place, because a widget deep in a panel
/// holds a borrow of the interface and the graph, and half of these need the
/// device or the preview.
#[derive(Default)]
struct Requests {
    path: Option<RenderPath>,
    mesh: Option<MeshKind>,
    backend: Option<MsdfBackend>,
    /// Open the palette to add a node at this graph-space point.
    add_at: Option<Vec2>,
    /// Open the palette to add a node in the middle of the canvas, wherever
    /// that is — a widget outside the canvas does not know.
    add_at_center: bool,
    add_definition: Option<String>,
    /// Where `add_definition` was let go, in screen pixels — a palette row
    /// is a drag source, and this is where the drag ended up. `None` when
    /// the node was requested some other way (Enter in the search box, the
    /// toolbar button), in which case the existing `picker.target`/canvas
    /// centre fallback applies.
    drop_screen_point: Option<Vec2>,
    /// A palette row is being dragged this frame; its label, for the ghost
    /// that follows the pointer.
    dragging_definition: Option<String>,
    changed_graph: bool,
    toggle_spin: bool,
    /// Turn the preview by this many pixels of drag.
    spin_drag: Option<f32>,
    fit: bool,
    /// Show this in the status bar instead of building a message at the
    /// apply site — for a widget (the inspector's generic-type picker) whose
    /// outcome depends on what it did, not just that it ran.
    message: Option<String>,
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

/// The toolbar: what the graph is, and how it is being shown.
fn toolbar_panel(
    ui: &mut Ui<'_>,
    rect: Rect,
    requests: &mut Requests,
    preview: &Preview,
    backend: MsdfBackend,
    graph: &mut Graph,
) {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    ui.draw().rect(rect, theme.palette.panel);
    ui.separator(Rect::from_min_size(
        Vec2::new(rect.min.x, rect.max.y),
        Vec2::new(rect.width(), 1.0),
    ));

    let inner = rect.shrink(metrics.padding * 0.5);
    let row = Rect::from_min_size(
        Vec2::new(inner.min.x, inner.center().y - metrics.row_height * 0.5),
        Vec2::new(inner.width(), metrics.row_height),
    );
    let gap = metrics.row_gap;
    let mut cursor = row;

    // The graph's name, editable.
    let (name_rect, rest) = cursor.split_left(180.0 * theme.scale);
    let mut name = graph.name().to_string();
    if ui
        .text_field(Id::new("toolbar.name"), name_rect, &mut name)
        .changed
    {
        graph.set_name(name);
        requests.changed_graph = true;
    }
    cursor = Rect::from_min_max(rest.min + Vec2::new(gap, 0.0), rest.max);

    let button = |ui: &mut Ui<'_>, cursor: &mut Rect, id: &str, label: &str, on: bool| {
        let width = (ui.measure_ui(label).x + metrics.padding * 2.0).max(48.0);
        let (button_rect, rest) = cursor.split_left(width);
        *cursor = Rect::from_min_max(rest.min + Vec2::new(gap, 0.0), rest.max);
        let fill = if on { Some(theme.palette.accent) } else { None };
        ui.button_colored(Id::new(id), button_rect, label, fill)
            .clicked
    };

    for path in RenderPath::ALL {
        let label = path.name();
        let on = preview.path() == *path;
        if button(ui, &mut cursor, &format!("toolbar.path.{label}"), label, on) {
            requests.path = Some(*path);
        }
    }
    let mesh_label = format!("mesh: {}", preview.mesh_kind().name());
    if button(ui, &mut cursor, "toolbar.mesh", &mesh_label, false) {
        requests.mesh = Some(preview.mesh_kind().next());
    }
    let backend_label = format!("msdf: {}", backend.name());
    if button(ui, &mut cursor, "toolbar.backend", &backend_label, false) {
        requests.backend = Some(backend.toggled());
    }
    if button(ui, &mut cursor, "toolbar.add", "add node", false) {
        // Where "the middle of the canvas" is, the toolbar does not know.
        requests.add_at_center = true;
    }
    if button(ui, &mut cursor, "toolbar.fit", "fit", false) {
        requests.fit = true;
    }
    let spin_label = if preview.spinning {
        "spin: on"
    } else {
        "spin: off"
    };
    if button(ui, &mut cursor, "toolbar.spin", spin_label, false) {
        requests.toggle_spin = true;
    }
}

/// The palette: search the node library, and add one.
fn palette_panel(
    ui: &mut Ui<'_>,
    rect: Rect,
    picker: &mut NodePicker,
    registry: &NodeRegistry,
    requests: &mut Requests,
) {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let title = if picker.open {
        "nodes — pick one to add".to_string()
    } else {
        format!("nodes ({})", registry.len())
    };
    let inner = ui.panel(rect, Some(&title));
    if picker.open && ui.input.key_pressed(Key::Escape) {
        picker.close();
    }

    let (search_rect, rest) = inner.split_top(metrics.row_height);
    if ui
        .text_field(Id::new("palette.search"), search_rect, &mut picker.query)
        .changed
    {
        picker.highlighted = 0;
    }

    // Category filter: one row of buttons that wraps.
    let mut cursor = Vec2::new(rest.min.x, rest.min.y + metrics.row_gap);
    let categories = registry.categories();
    let button_height = metrics.row_height * 0.85;
    for (index, category) in std::iter::once("all").chain(categories).enumerate() {
        let width = ui.measure_ui(category).x + metrics.padding;
        if cursor.x + width > rest.max.x {
            cursor = Vec2::new(rest.min.x, cursor.y + button_height + metrics.row_gap);
        }
        let button_rect = Rect::from_min_size(cursor, Vec2::new(width, button_height));
        let selected = match picker.category.as_deref() {
            None => category == "all",
            Some(current) => current == category,
        };
        let fill = if selected {
            Some(theme.palette.accent)
        } else {
            None
        };
        if ui
            .button_colored(
                Id::new("palette.category").with(index as u64),
                button_rect,
                category,
                fill,
            )
            .clicked
        {
            picker.category = if category == "all" {
                None
            } else {
                Some(category.to_string())
            };
            picker.highlighted = 0;
        }
        cursor.x += width + metrics.row_gap;
    }

    let list_rect = Rect::from_min_max(
        Vec2::new(rest.min.x, cursor.y + button_height + metrics.row_gap),
        rest.max,
    );
    if list_rect.is_empty() {
        return;
    }

    let matches = picker.matches(registry, 200);
    // Only show a highlight once the keyboard is actually driving the list:
    // otherwise the first row looks selected in a palette nobody has touched.
    let searching = ui.state.focus() == Some(Id::new("palette.search"));
    let show_highlight = searching || !picker.query.trim().is_empty();
    // Arrow keys move the highlight while the search box has focus, so the
    // whole flow is type-then-Enter without touching the pointer.
    if searching {
        if ui.input.key_pressed(Key::Down) {
            picker.move_highlight(1, matches.len());
        }
        if ui.input.key_pressed(Key::Up) {
            picker.move_highlight(-1, matches.len());
        }
        if ui.input.key_pressed(Key::Enter) {
            if let Some(found) = picker.highlighted(&matches) {
                requests.add_definition = Some(found.id.clone());
            }
        }
    }

    let row_height = metrics.row_height * 1.5;
    let content = Vec2::new(list_rect.width(), matches.len() as f32 * row_height);
    let area = ui.scroll_area(Id::new("palette.list"), list_rect, content);
    let origin = area.origin();
    let first = ((area.offset.y / row_height).floor() as usize).saturating_sub(1);
    let visible = (list_rect.height() / row_height).ceil() as usize + 2;
    let last = (first + visible).min(matches.len());
    for (index, found) in matches.iter().enumerate().take(last).skip(first) {
        let row = Rect::from_min_size(
            Vec2::new(origin.x, origin.y + index as f32 * row_height),
            Vec2::new(
                list_rect.width() - metrics.scrollbar_width,
                row_height - 2.0,
            ),
        );
        let response = ui.interact(Id::new("palette.row").with(index as u64), row);
        let highlighted = show_highlight && index == picker.highlighted;
        if response.hovered || highlighted || response.dragging {
            ui.draw().round_rect(
                row,
                metrics.radius,
                if highlighted || response.dragging {
                    theme.palette.accent.with_alpha(0.30)
                } else {
                    theme.palette.control
                },
            );
        }
        // A row is a drag source: releasing anywhere adds the node, and
        // where it lands decides where. Releasing on the canvas places it
        // there (see the ghost this reports below, and where it is applied
        // in `Editor::build`); releasing anywhere else — including a plain
        // click with no drag at all, which is `drag_released` with a zero
        // delta — falls back to the palette's usual target/centre placement.
        if response.dragging {
            requests.dragging_definition = Some(found.label.clone());
        }
        if response.drag_released {
            requests.add_definition = Some(found.id.clone());
            requests.drop_screen_point = Some(ui.input.pointer_or_zero());
        }
        let (label_rect, id_rect) = row.shrink(3.0).split_top(row.height() * 0.55);
        ui.truncated_label(label_rect, &found.label, theme.palette.text, Align::Left);
        ui.small_label(id_rect, &found.id, theme.palette.text_dim, Align::Left);
    }
    area.end(ui);
}

/// The inspector: the preview, the selected node, and the macro variables.
///
/// Everything below the preview scrolls, because the tallest node in the
/// library has seven inputs and the macro list grows with the graph — and a
/// panel that silently runs out of room is a panel whose bottom half nobody
/// knows exists.
fn inspector_panel(
    ui: &mut Ui<'_>,
    rect: Rect,
    preview: &Preview,
    graph: &mut Graph,
    registry: &NodeRegistry,
    selected: Option<NodeId>,
    requests: &mut Requests,
) {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let inner = ui.panel(rect, Some("preview"));

    // The preview, square, as wide as the panel allows.
    let side = inner.width().min(inner.height() * 0.45);
    let image = Rect::from_min_size(
        Vec2::new(inner.center().x - side * 0.5, inner.min.y),
        Vec2::splat(side),
    );
    ui.draw()
        .round_rect(image, metrics.radius, theme.palette.canvas);
    ui.draw().image(
        image,
        preview.texture(),
        Vec2::ZERO,
        Vec2::ONE,
        Color::WHITE,
        metrics.radius,
    );
    ui.draw().round_rect_border(
        image,
        metrics.radius,
        metrics.outline_width,
        theme.palette.outline,
    );
    // Dragging the image turns the mesh, which is the gesture everyone tries
    // first on a 3D preview.
    let response = ui.interact(Id::new("preview.image"), image);
    if response.dragging && response.drag_delta.x != 0.0 {
        requests.spin_drag = Some(response.drag_delta.x);
    }
    if response.double_clicked {
        requests.toggle_spin = true;
    }

    // -- what is going to be drawn below, so it can scroll ---------------
    let selected_definition = selected
        .and_then(|id| graph.node(id))
        .and_then(|node| registry.get(&node.def).cloned());
    let (declared, _) = graph.declared_macros(registry);
    let mut macros: Vec<MacroDef> = abi::abi_macros();
    for definition in declared.values() {
        if !macros
            .iter()
            .any(|existing| existing.name == definition.name)
        {
            macros.push(definition.clone());
        }
    }

    let body = Rect::from_min_max(
        Vec2::new(inner.min.x, image.max.y + metrics.row_gap),
        inner.max,
    );
    if body.is_empty() {
        return;
    }
    // Every row is a known height, and the one paragraph is measurable, so
    // the content height is exact rather than a guess from last frame.
    let row = metrics.row_height + metrics.row_gap;
    let small_row = metrics.small_text_size * 1.4 + metrics.row_gap;
    let width = body.width() - metrics.scrollbar_width;
    let mut content = metrics.row_gap;
    let doc_height = match &selected_definition {
        Some(definition) if !definition.doc.is_empty() => {
            let font = ui.state.ui_font;
            let size = metrics.small_text_size;
            ui.layout(font, &definition.doc, TextOptions::new(size).wrapped(width))
                .size
                .y
        }
        _ => 0.0,
    };
    match &selected_definition {
        Some(definition) => {
            content += row * 3.0 + doc_height + metrics.row_gap;
            content += row * definition.generics.len() as f32;
            content += (small_row + row) * definition.inputs.len() as f32;
        }
        None => content += row,
    }
    if !macros.is_empty() {
        content += row * (1.0 + macros.len() as f32);
    }

    let area = ui.scroll_area(Id::new("inspector.scroll"), body, Vec2::new(width, content));
    let mut cursor = area.origin().y;
    let mut next = |height: f32| {
        let rect = Rect::from_min_size(Vec2::new(body.min.x, cursor), Vec2::new(width, height));
        cursor += height + metrics.row_gap;
        rect
    };

    // -- the selected node ---------------------------------------------
    match (selected, selected_definition) {
        (Some(id), Some(definition)) => {
            ui.label(
                next(metrics.row_height),
                &definition.label,
                theme.palette.text,
                Align::Left,
            );
            widgets::field_row(ui, next(metrics.row_height), "definition", &definition.id);
            widgets::field_row(
                ui,
                next(metrics.row_height),
                "category",
                &definition.category,
            );
            if doc_height > 0.0 {
                let font = ui.state.ui_font;
                let size = metrics.small_text_size;
                let layout =
                    ui.layout(font, &definition.doc, TextOptions::new(size).wrapped(width));
                let doc_rect = next(doc_height);
                ui.draw()
                    .text(&layout, doc_rect.min, theme.palette.text_dim);
            }

            // A generic node (one node kind serving every type it allows,
            // e.g. `math.add` instead of a separate `math.add.f32`/`.vec3f`/…
            // — see `wxsl_core::node::GenericParam`) needs its type picked
            // before its sockets mean anything. Connecting a wire already
            // resolves it automatically; this is for doing so by hand, and
            // for changing it later — which drops whatever wiring no longer
            // fits, exactly as switching from `math.add.f32` to
            // `math.add.vec3f` always would have.
            for param in &definition.generics {
                let param_name = param.name.as_str();
                let resolved = graph.generic_type(id, param_name);
                let row = next(metrics.row_height);
                let (label_rect, buttons_rect) = row.split_left(metrics.side_panel_width * 0.3);
                let label = match resolved {
                    Some(ty) => format!("{param_name}: {ty}"),
                    None => format!("{param_name}: ?"),
                };
                ui.truncated_label(
                    label_rect,
                    &label,
                    if resolved.is_some() {
                        theme.palette.text_dim
                    } else {
                        theme.palette.warning
                    },
                    Align::Left,
                );
                let gap = metrics.row_gap;
                // `GenericParam::new` requires at least one allowed type, so
                // `len() - 1` never underflows.
                let button_width = (buttons_rect.width() - gap * (param.allowed.len() - 1) as f32)
                    / param.allowed.len() as f32;
                for (index, &candidate) in param.allowed.iter().enumerate() {
                    let button_rect = Rect::from_min_size(
                        Vec2::new(
                            buttons_rect.min.x + (button_width + gap) * index as f32,
                            buttons_rect.min.y,
                        ),
                        Vec2::new(button_width, buttons_rect.height()),
                    );
                    let active = resolved == Some(candidate);
                    let fill = active.then_some(theme.palette.accent);
                    let button_id = Id::new("inspector.generic")
                        .with(u64::from(id.0))
                        .with(Id::new(param_name).0)
                        .with(index as u64);
                    if ui
                        .button_colored(button_id, button_rect, candidate.wxsl_type(), fill)
                        .clicked
                        && !active
                    {
                        match graph.set_generic(registry, id, param_name, candidate) {
                            Ok(disconnected) => {
                                requests.changed_graph = true;
                                if !disconnected.is_empty() {
                                    requests.message = Some(format!(
                                        "{param_name} is now {candidate}; disconnected {} \
                                         edge(s) that no longer fit",
                                        disconnected.len()
                                    ));
                                }
                            }
                            Err(error) => requests.message = Some(error.to_string()),
                        }
                    }
                }
            }

            // Unconnected inputs are editable; connected ones say what
            // drives them, because a value nobody reads is a lie.
            for socket in &definition.inputs {
                let name = socket.name.as_str();
                let reference = wxsl_core::graph::SocketRef::new(id, name);
                let connected = graph.edge_into(&reference).is_some();
                let value = graph
                    .node(id)
                    .and_then(|node| node.params.get(name).copied())
                    .or(socket.default);
                // `socket.ty` is only a placeholder on a generic socket (see
                // `wxsl_core::node::Socket::generic`); this instance's
                // resolution is what the label should actually say.
                let shown_ty = socket
                    .generic
                    .as_ref()
                    .and_then(|param| graph.generic_type(id, param.as_str()))
                    .unwrap_or(socket.ty);
                ui.small_label(
                    next(metrics.small_text_size * 1.4),
                    &format!(
                        "{name}  {}",
                        widgets::typed_summary(shown_ty, value.as_ref())
                    ),
                    theme.palette.text_dim,
                    Align::Left,
                );
                let editor_rect = next(metrics.row_height);
                if connected {
                    ui.draw().round_rect(
                        editor_rect,
                        metrics.radius,
                        theme.palette.control.with_alpha(0.5),
                    );
                    ui.small_label(
                        editor_rect.shrink(4.0),
                        "connected",
                        theme.palette.text_dim,
                        Align::Center,
                    );
                    continue;
                }
                let Some(mut value) = value else {
                    ui.small_label(
                        editor_rect,
                        "no default — connect something",
                        theme.palette.warning,
                        Align::Left,
                    );
                    continue;
                };
                let widget_id = Id::new("inspector.param")
                    .with(u64::from(id.0))
                    .with(Id::new(name).0);
                if widgets::value_editor(ui, widget_id, editor_rect, &mut value) {
                    graph.set_param(id, name, value);
                    requests.changed_graph = true;
                }
            }
        }
        _ => {
            ui.small_label(
                next(metrics.row_height),
                "no node selected",
                theme.palette.text_dim,
                Align::Left,
            );
        }
    }

    // -- macro variables -----------------------------------------------
    if !macros.is_empty() {
        let header = next(metrics.row_height);
        ui.separator(header);
        ui.label(header, "macro variables", theme.palette.text, Align::Left);
        for (index, declaration) in macros.iter().enumerate() {
            let name = declaration.name.as_str();
            let mut value = graph.macros().get(name).unwrap_or(declaration.default);
            if widgets::macro_editor(
                ui,
                Id::new("inspector.macro").with(index as u64),
                next(metrics.row_height),
                declaration,
                &mut value,
            ) {
                graph.set_macro(name.to_string(), value);
                requests.changed_graph = true;
            }
        }
    }
    area.end(ui);
}

/// The code panels: what the graph became, and what stopped it.
fn code_panel(ui: &mut Ui<'_>, rect: Rect, tab: CodeTab, preview: &Preview) -> CodeTab {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let inner = ui.panel(rect, None);
    let (tabs_rect, body) = inner.split_top(metrics.row_height);

    let problems = preview.status().errors().len();
    let problem_label = if problems == 0 {
        "problems".to_string()
    } else {
        format!("problems ({problems})")
    };
    let labels = ["WXSL", "WGSL", problem_label.as_str()];
    let (tabs_rect, meta_rect) = tabs_rect.split_left(280.0 * theme.scale);
    let chosen = ui.tabs(Id::new("code.tabs"), tabs_rect, &labels, tab.index());
    let tab = CodeTab::ALL[chosen.min(CodeTab::ALL.len() - 1)];

    let lines = match tab {
        CodeTab::Wxsl => preview.wxsl().lines().count(),
        CodeTab::Wgsl => preview.wgsl().lines().count(),
        CodeTab::Problems => problems,
    };
    let meta = match tab {
        CodeTab::Wxsl => format!("{lines} lines · generated from the graph"),
        CodeTab::Wgsl => format!("{lines} lines · {} path", preview.path()),
        CodeTab::Problems if problems == 0 => "the graph compiles".to_string(),
        CodeTab::Problems => format!("{problems} to fix"),
    };
    ui.small_label(meta_rect, &meta, theme.palette.text_dim, Align::Right);

    let body = Rect::from_min_max(body.min + Vec2::new(0.0, metrics.row_gap), body.max);
    match tab {
        CodeTab::Wxsl => ui.highlighted_code_view(
            Id::new("code.wxsl"),
            body,
            preview.wxsl(),
            preview.wxsl_highlight(),
        ),
        CodeTab::Wgsl => ui.highlighted_code_view(
            Id::new("code.wgsl"),
            body,
            preview.wgsl(),
            preview.wgsl_highlight(),
        ),
        CodeTab::Problems => {
            let errors = preview.status().errors();
            if errors.is_empty() {
                ui.label(
                    body,
                    "the graph compiles",
                    theme.palette.text_dim,
                    Align::Center,
                );
            } else {
                // One error per paragraph, wrapped: a shader diagnostic
                // carries the offending line and a caret, and truncating it
                // throws away the useful half.
                let text = errors.join("\n\n");
                ui.code_view(Id::new("code.problems"), body, &text);
            }
        }
    }
    tab
}

/// The status bar: the numbers worth watching while editing.
fn status_bar(
    ui: &mut Ui<'_>,
    rect: Rect,
    graph: &Graph,
    preview: &Preview,
    backend: MsdfBackend,
    message: &str,
    modified: bool,
) {
    let theme = *ui.theme();
    ui.draw().rect(rect, theme.palette.panel_header);
    let inner = rect.shrink(theme.metrics.padding * 0.5);

    let (variants, cache) = preview.variant_stats();
    let glyphs = ui.state.fonts.stats();
    let left = format!(
        "{} nodes · {} links · {variants} variants ({} hits, {} compiles) · {} glyphs · msdf {} · atlas {:.0}%",
        graph.node_count(),
        graph.edges().len(),
        cache.hits,
        cache.misses,
        glyphs.fields,
        backend.name(),
        ui.state.atlas.occupancy() * 100.0,
    );
    ui.small_label(inner, &left, theme.palette.text_dim, Align::Left);

    let color = if preview.status().is_ok() {
        theme.palette.text_dim
    } else {
        theme.palette.error
    };
    let right = if modified {
        format!("• {message}")
    } else {
        message.to_string()
    };
    ui.small_label(inner, &right, color, Align::Right);
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
