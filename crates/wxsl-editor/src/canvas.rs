//! The pan/zoom node canvas: layout, hit-testing, drawing, interaction.
//!
//! The canvas is the one screen that matters, so its geometry is separated
//! from its drawing on purpose: [`View`] is the transform, [`NodeLayout`] is
//! where a node and its ports ended up, and both are pure functions of the
//! graph and the theme. That is what lets the interaction rules — which port
//! is under the pointer, which link would this click cut, does this
//! connection type-check — be tested with no device and no window
//! (ADR 0013).
//!
//! Mutation goes straight to the [`Graph`]: [`Graph::connect`] type-checks
//! and refuses cycles, changing nothing on error, so the canvas can offer a
//! connection optimistically and report the refusal. The editor above it only
//! has to know *that* something changed, which is what
//! [`CanvasResponse::changed`] says.

use std::collections::BTreeMap;

use glam::Vec2;
use wxsl_core::graph::{Graph, Node, NodeId, SocketRef};
use wxsl_core::node::{NodeDefinition, NodeRegistry, Socket, ValueType};
use wxsl_render::ui::draw::{Color, Rect};
use wxsl_render::ui::input::{Key, MouseButton};

use crate::theme::Theme;
use crate::ui::{Align, Ui};

/// The transform between graph space and screen space.
///
/// Graph space is what a node's position is stored in (and what the node
/// format round-trips); screen space is physical pixels. Keeping the two
/// apart is what makes zoom a property of the view rather than something
/// baked into every stored coordinate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// Graph-space point at the canvas's top-left corner.
    pub pan: Vec2,
    /// Screen pixels per graph unit.
    pub zoom: f32,
}

impl Default for View {
    fn default() -> Self {
        View {
            pan: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

impl View {
    /// The tightest and loosest zoom the canvas allows.
    pub const ZOOM_RANGE: (f32, f32) = (0.2, 3.0);

    /// Where a graph-space point lands on screen, given the canvas's rect.
    pub fn to_screen(&self, canvas: Rect, point: Vec2) -> Vec2 {
        canvas.min + (point - self.pan) * self.zoom
    }

    /// Where a screen point falls in graph space.
    pub fn to_graph(&self, canvas: Rect, point: Vec2) -> Vec2 {
        self.pan + (point - canvas.min) / self.zoom
    }

    /// Zoom by `factor` about a screen point, keeping that point still.
    ///
    /// The behaviour a wheel has to have: zooming about the centre while the
    /// pointer is somewhere else feels like the canvas is fighting back.
    pub fn zoom_about(&mut self, canvas: Rect, screen: Vec2, factor: f32) {
        let before = self.to_graph(canvas, screen);
        self.zoom = (self.zoom * factor).clamp(View::ZOOM_RANGE.0, View::ZOOM_RANGE.1);
        let after = self.to_graph(canvas, screen);
        self.pan += before - after;
    }

    /// Pan by a screen-space delta.
    pub fn pan_by(&mut self, delta: Vec2) {
        self.pan -= delta / self.zoom;
    }

    /// Centre the view on `bounds`, zoomed to fit inside `canvas`.
    pub fn fit(&mut self, canvas: Rect, bounds: (Vec2, Vec2)) {
        let (min, max) = bounds;
        let size = (max - min).max(Vec2::splat(1.0));
        let margin = 40.0;
        let available = (canvas.size() - Vec2::splat(margin * 2.0)).max(Vec2::splat(1.0));
        let scale = (available / size).min_element();
        self.zoom = scale.clamp(View::ZOOM_RANGE.0, View::ZOOM_RANGE.1);
        let center = (min + max) * 0.5;
        self.pan = center - canvas.size() * 0.5 / self.zoom;
    }
}

/// One port, laid out.
#[derive(Clone, Debug, PartialEq)]
pub struct PortLayout {
    /// The socket this port is.
    pub socket: String,
    /// Its type, which is what decides its colour and what it may connect to.
    pub ty: ValueType,
    /// Centre of the port's circle, on screen.
    pub center: Vec2,
    /// The row the port labels, on screen.
    pub row: Rect,
    /// Whether an edge feeds it (inputs only).
    pub connected: bool,
}

/// One node, laid out.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeLayout {
    /// Which node.
    pub id: NodeId,
    /// The whole node on screen.
    pub rect: Rect,
    /// Its title strip.
    pub header: Rect,
    /// Output ports, top to bottom.
    pub outputs: Vec<PortLayout>,
    /// Input ports, below the outputs.
    pub inputs: Vec<PortLayout>,
}

impl NodeLayout {
    /// The port nearest `point` within `radius`, if any.
    pub fn port_at(&self, point: Vec2, radius: f32) -> Option<(PortKind, &PortLayout)> {
        let mut best: Option<(PortKind, &PortLayout, f32)> = None;
        for (kind, ports) in [
            (PortKind::Output, &self.outputs),
            (PortKind::Input, &self.inputs),
        ] {
            for port in ports {
                let distance = port.center.distance(point);
                if distance <= radius && best.is_none_or(|(_, _, best)| distance < best) {
                    best = Some((kind, port, distance));
                }
            }
        }
        best.map(|(kind, port, _)| (kind, port))
    }
}

/// Which side of a node a port is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortKind {
    /// A value the node consumes.
    Input,
    /// A value the node produces.
    Output,
}

/// The two per-socket questions [`layout_node`] cannot answer from the
/// definition alone, because they depend on a specific graph instance.
///
/// Grouped into one type rather than two more function parameters: `connect`
/// needs a live [`Graph`] and `display_ty` needs the node's id as well, and
/// bundling them is what keeps [`layout_node`] a pure function of "this
/// definition, at this position" instead of also taking the graph directly.
pub struct SocketQueries<'a> {
    /// Whether an edge feeds this input.
    pub connected: &'a dyn Fn(&str) -> bool,
    /// A socket's *effective* type — `socket.ty` is only a placeholder for a
    /// generic one (see [`wxsl_core::node::Socket::generic`]), and the
    /// resolved type is what decides its port colour.
    pub display_ty: &'a dyn Fn(&Socket) -> ValueType,
}

/// Lay one node out at `position` in graph space.
///
/// A pure function of the definition, `queries` and the theme, so the same
/// arithmetic serves drawing, hit-testing and the tests.
pub fn layout_node(
    id: NodeId,
    position: Vec2,
    definition: &NodeDefinition,
    queries: &SocketQueries<'_>,
    view: &View,
    canvas: Rect,
    theme: &Theme,
) -> NodeLayout {
    let zoom = view.zoom;
    let metrics = &theme.metrics;
    let width = metrics.node_width * zoom;
    let header_height = metrics.node_header_height * zoom;
    let row_height = metrics.row_height * zoom;
    let padding = metrics.padding * 0.5 * zoom;

    let rows = definition.outputs.len() + definition.inputs.len();
    let height = header_height + rows as f32 * row_height + padding * 2.0;
    let origin = view.to_screen(canvas, position);
    let rect = Rect::from_min_size(origin, Vec2::new(width, height));
    let (header, body) = rect.split_top(header_height);

    let mut y = body.min.y + padding;
    let mut outputs = Vec::with_capacity(definition.outputs.len());
    for socket in &definition.outputs {
        let row = Rect::from_min_size(Vec2::new(rect.min.x, y), Vec2::new(width, row_height));
        outputs.push(PortLayout {
            socket: socket.name.as_str().to_string(),
            ty: (queries.display_ty)(socket),
            // On the right edge, on the row's centre line.
            center: Vec2::new(rect.max.x, row.center().y),
            row,
            connected: false,
        });
        y += row_height;
    }
    let mut inputs = Vec::with_capacity(definition.inputs.len());
    for socket in &definition.inputs {
        let row = Rect::from_min_size(Vec2::new(rect.min.x, y), Vec2::new(width, row_height));
        inputs.push(PortLayout {
            socket: socket.name.as_str().to_string(),
            ty: (queries.display_ty)(socket),
            center: Vec2::new(rect.min.x, row.center().y),
            row,
            connected: (queries.connected)(socket.name.as_str()),
        });
        y += row_height;
    }

    NodeLayout {
        id,
        rect,
        header,
        outputs,
        inputs,
    }
}

/// The two control points of the curve a link is drawn as.
///
/// Horizontal tangents, scaled with the gap, so a link leaves an output to
/// the right and enters an input from the left however the two are arranged —
/// which is what makes a backwards link readable instead of a straight line
/// through three nodes.
pub fn link_controls(from: Vec2, to: Vec2) -> (Vec2, Vec2) {
    let reach = ((to.x - from.x).abs() * 0.5).clamp(30.0, 160.0);
    (from + Vec2::new(reach, 0.0), to - Vec2::new(reach, 0.0))
}

/// The distance from `point` to the link curve, by sampling it.
///
/// Used to decide which link a click would cut. Sampling rather than solving:
/// a cubic's true nearest point is a quintic, and twenty samples is well
/// inside the tolerance of "did the user click on this line".
pub fn distance_to_link(from: Vec2, to: Vec2, point: Vec2) -> f32 {
    let (control_from, control_to) = link_controls(from, to);
    let mut best = f32::INFINITY;
    let mut previous = from;
    for step in 1..=20 {
        let t = step as f32 / 20.0;
        let u = 1.0 - t;
        let sample = from * (u * u * u)
            + control_from * (3.0 * u * u * t)
            + control_to * (3.0 * u * t * t)
            + to * (t * t * t);
        best = best.min(distance_to_segment(previous, sample, point));
        previous = sample;
    }
    best
}

/// Distance from `point` to the segment `a`..`b`.
fn distance_to_segment(a: Vec2, b: Vec2, point: Vec2) -> f32 {
    let along = b - a;
    let length_squared = along.length_squared();
    if length_squared <= f32::EPSILON {
        return point.distance(a);
    }
    let t = ((point - a).dot(along) / length_squared).clamp(0.0, 1.0);
    point.distance(a + along * t)
}

/// What the canvas is in the middle of doing.
#[derive(Clone, Debug, PartialEq)]
pub enum Interaction {
    /// Nothing.
    Idle,
    /// The view is being dragged.
    Panning,
    /// A node is being moved. `grab` is where in the node the pointer took
    /// hold, in graph units, so it does not jump to the pointer.
    DraggingNode {
        /// The node being moved.
        node: NodeId,
        /// Offset from the node's origin to the grab point, in graph units.
        grab: Vec2,
    },
    /// A link is being dragged from a port that has no other end yet.
    DraggingLink {
        /// The port it started at.
        from: SocketRef,
        /// Which side of its node that port is.
        kind: PortKind,
    },
}

/// What one frame of canvas did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CanvasResponse {
    /// The graph changed, so the material needs recompiling.
    pub changed: bool,
    /// Something to show the user: a refused connection, usually.
    pub message: Option<String>,
    /// The user asked for the node palette at this graph-space point.
    pub add_node_at: Option<Vec2>,
}

/// The node canvas.
pub struct Canvas {
    /// The pan and zoom.
    pub view: View,
    /// Which nodes are selected.
    pub selection: Vec<NodeId>,
    interaction: Interaction,
    /// Node layouts from the last frame, for hit tests before drawing.
    layouts: Vec<NodeLayout>,
    /// The link the pointer is over, if any.
    hovered_link: Option<SocketRef>,
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas::new()
    }
}

impl Canvas {
    /// An empty canvas at the default view.
    pub fn new() -> Self {
        Canvas {
            view: View::default(),
            selection: Vec::new(),
            interaction: Interaction::Idle,
            layouts: Vec::new(),
            hovered_link: None,
        }
    }

    /// What the canvas is doing.
    pub fn interaction(&self) -> &Interaction {
        &self.interaction
    }

    /// The node layouts as of the last drawn frame.
    pub fn layouts(&self) -> &[NodeLayout] {
        &self.layouts
    }

    /// The single selected node, if exactly one is selected.
    pub fn selected_node(&self) -> Option<NodeId> {
        match self.selection.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }

    /// Frame the whole graph in `rect`.
    pub fn fit_to_graph(
        &mut self,
        graph: &Graph,
        registry: &NodeRegistry,
        rect: Rect,
        theme: &Theme,
    ) {
        if let Some(bounds) = graph_bounds(graph, registry, theme) {
            self.view.fit(rect, bounds);
        }
    }

    /// Lay out, draw and interact with the graph for one frame.
    ///
    /// One function because immediate mode: the layout a click is tested
    /// against has to be the layout that was drawn, and computing it twice is
    /// how the two drift apart.
    pub fn show(
        &mut self,
        ui: &mut Ui<'_>,
        rect: Rect,
        graph: &mut Graph,
        registry: &NodeRegistry,
    ) -> CanvasResponse {
        let mut response = CanvasResponse::default();
        let theme = *ui.theme();
        let pointer = ui.input.pointer();
        let over_canvas =
            pointer.is_some_and(|point| rect.contains(point)) && !ui.pointer_claimed();

        // -- view --------------------------------------------------------
        if over_canvas {
            let scroll = ui.input.scroll();
            if scroll.y != 0.0 {
                let factor = (scroll.y * 0.0015).exp();
                self.view
                    .zoom_about(rect, pointer.unwrap_or(rect.center()), factor);
            }
            if ui.input.pressed(MouseButton::Middle)
                || (ui.input.pressed(MouseButton::Right) && ui.input.modifiers().shift)
            {
                self.interaction = Interaction::Panning;
            }
        }
        if self.interaction == Interaction::Panning {
            if ui.input.is_down(MouseButton::Middle) || ui.input.is_down(MouseButton::Right) {
                self.view.pan_by(ui.input.pointer_delta());
            } else {
                self.interaction = Interaction::Idle;
            }
        }

        // -- layout ------------------------------------------------------
        self.layouts = layout_graph(graph, registry, &self.view, rect, &theme);

        // -- background --------------------------------------------------
        ui.draw().push_clip(rect);
        ui.draw().rect(rect, theme.palette.canvas);
        self.draw_grid(ui, rect);

        // -- links -------------------------------------------------------
        self.hovered_link = None;
        let link_hit_radius = (theme.metrics.link_width * 3.0).max(6.0);
        for edge in graph.edges() {
            let Some(from) = self.port_center(&edge.from, PortKind::Output) else {
                continue;
            };
            let Some(to) = self.port_center(&edge.to, PortKind::Input) else {
                continue;
            };
            let ty = self.port_type(&edge.from, PortKind::Output);
            let hovered = over_canvas
                && pointer
                    .is_some_and(|point| distance_to_link(from, to, point) <= link_hit_radius);
            if hovered {
                self.hovered_link = Some(edge.to.clone());
            }
            let color = if hovered {
                theme.palette.link_hover
            } else {
                ty.map(|ty| theme.type_color(ty).lerp(theme.palette.link, 0.35))
                    .unwrap_or(theme.palette.link)
            };
            self.draw_link(ui, from, to, color, hovered);
        }

        // -- nodes -------------------------------------------------------
        // Front to back for hit-testing, back to front for drawing: the last
        // node drawn is on top, so it is the first that should catch a click.
        let mut clicked_node = None;
        if over_canvas && ui.input.pressed(MouseButton::Left) {
            let point = pointer.unwrap_or(Vec2::ZERO);
            clicked_node = self
                .layouts
                .iter()
                .rev()
                .find(|layout| layout.rect.contains(point))
                .map(|layout| layout.id);
        }
        for index in 0..self.layouts.len() {
            self.draw_node(ui, index, graph, registry);
        }

        // -- interaction -------------------------------------------------
        response.changed |= self.handle_ports(ui, rect, graph, registry, &mut response);
        response.changed |= self.handle_nodes(ui, rect, graph, clicked_node);

        if over_canvas && ui.input.released(MouseButton::Right) && !ui.input.modifiers().shift {
            let point = pointer.unwrap_or(rect.center());
            if self.hovered_link.is_some() {
                // Right-clicking a link cuts it, which is the fastest
                // unlink gesture there is.
                if let Some(input) = self.hovered_link.clone() {
                    if graph.disconnect(registry, &input).is_some() {
                        response.changed = true;
                        response.message = Some(format!("disconnected {input}"));
                    }
                }
            } else if !self
                .layouts
                .iter()
                .any(|layout| layout.rect.contains(point))
            {
                response.add_node_at = Some(self.view.to_graph(rect, point));
            }
        }

        if over_canvas && !ui.state.is_editing() {
            let delete = ui.input.key_pressed_plain(Key::Delete)
                || ui.input.key_pressed_plain(Key::Backspace);
            if delete && !self.selection.is_empty() {
                for node in core::mem::take(&mut self.selection) {
                    graph.remove_node(registry, node);
                }
                response.changed = true;
                response.message = Some("deleted the selection".to_string());
            }
        }

        ui.draw().pop_clip();
        response
    }

    /// The grid, at a spacing that stays legible as the zoom changes.
    fn draw_grid(&self, ui: &mut Ui<'_>, rect: Rect) {
        let theme = *ui.theme();
        // Pick the power of two whose spacing on screen is closest to 24px,
        // so the grid neither disappears nor turns into a solid fill.
        let target = 24.0;
        let mut spacing = 16.0f32;
        while spacing * self.view.zoom < target {
            spacing *= 2.0;
        }
        while spacing * self.view.zoom > target * 2.0 {
            spacing *= 0.5;
        }
        let step = spacing * self.view.zoom;
        if step < 4.0 {
            return;
        }

        let first = (self.view.pan / spacing).floor() * spacing;
        let mut x = self.view.to_screen(rect, first).x;
        let mut column = (first.x / spacing).round() as i64;
        while x <= rect.max.x {
            if x >= rect.min.x {
                let color = if column % 10 == 0 {
                    theme.palette.grid_major
                } else {
                    theme.palette.grid
                };
                ui.draw().line(
                    Vec2::new(x, rect.min.y),
                    Vec2::new(x, rect.max.y),
                    1.0,
                    color,
                );
            }
            x += step;
            column += 1;
        }
        let mut y = self.view.to_screen(rect, first).y;
        let mut row = (first.y / spacing).round() as i64;
        while y <= rect.max.y {
            if y >= rect.min.y {
                let color = if row % 10 == 0 {
                    theme.palette.grid_major
                } else {
                    theme.palette.grid
                };
                ui.draw().line(
                    Vec2::new(rect.min.x, y),
                    Vec2::new(rect.max.x, y),
                    1.0,
                    color,
                );
            }
            y += step;
            row += 1;
        }
    }

    /// One link, as a curve.
    fn draw_link(&self, ui: &mut Ui<'_>, from: Vec2, to: Vec2, color: Color, emphasized: bool) {
        let theme = *ui.theme();
        let (control_from, control_to) = link_controls(from, to);
        let width =
            theme.metrics.link_width * self.view.zoom.max(0.5) * if emphasized { 1.8 } else { 1.0 };
        // Enough segments that a link reads as a curve at any zoom, few
        // enough that a graph of a hundred links is still a few thousand
        // instances.
        let segments = 24;
        ui.draw()
            .bezier(from, control_from, control_to, to, width, color, segments);
    }

    /// One node: body, title, ports, and each row's label and value.
    fn draw_node(&self, ui: &mut Ui<'_>, index: usize, graph: &Graph, registry: &NodeRegistry) {
        let theme = *ui.theme();
        let layout = self.layouts[index].clone();
        let Some(node) = graph.node(layout.id) else {
            return;
        };
        let Some(definition) = registry.get(&node.def) else {
            return;
        };
        let selected = self.selection.contains(&layout.id);
        let zoom = self.view.zoom;
        let radius = theme.metrics.radius * zoom;

        // A shadow, so a node reads as being above the grid.
        ui.draw().round_rect(
            layout.rect.translate(Vec2::splat(2.0 * zoom)),
            radius,
            Color::rgba(0.0, 0.0, 0.0, 0.35),
        );
        ui.draw()
            .round_rect(layout.rect, radius, theme.palette.node);
        // One colour for every kind of node unless this node says
        // otherwise — see `Theme::node_color`.
        let header_color = theme.node_color(node);
        ui.draw().round_rect(layout.header, radius, header_color);
        // Square the header's bottom corners against the body.
        let (_, header_bottom) = layout.header.split_bottom(radius);
        ui.draw().rect(header_bottom, header_color);
        ui.draw().round_rect_border(
            layout.rect,
            radius,
            theme.metrics.outline_width * if selected { 2.0 } else { 1.0 } * zoom.max(0.5),
            if selected {
                theme.palette.selection
            } else {
                theme.palette.outline
            },
        );

        // Below a certain zoom, text is unreadable and drawing it is just
        // noise and instances: the node's colour and shape carry it.
        let label_size = theme.metrics.small_text_size * zoom;
        if label_size < 5.0 {
            self.draw_ports(ui, &layout);
            return;
        }

        let title = node
            .label
            .clone()
            .unwrap_or_else(|| definition.label.clone());
        // The node's id, top-right. Every diagnostic names a node by it
        // ("node #7 references unknown definition ..."), so being able to
        // read it straight off the canvas is what makes those messages
        // actionable — and it is the one label a renamed node cannot hide.
        let id_text = layout.id.to_string();
        let inner = layout.header.shrink(theme.metrics.padding * 0.5 * zoom);
        let id_width = ui.measure_ui(&id_text).x + theme.metrics.padding * 0.5 * zoom;
        let (id_rect, title_rect) = inner.split_right(id_width);
        ui.truncated_label(title_rect, &title, theme.palette.text, Align::Left);
        ui.truncated_label(
            id_rect,
            &id_text,
            theme.palette.text.with_alpha(0.55),
            Align::Right,
        );

        let inset = theme.metrics.padding * zoom;
        for port in &layout.outputs {
            let row = Rect::from_min_max(
                Vec2::new(port.row.min.x + inset, port.row.min.y),
                Vec2::new(port.row.max.x - inset, port.row.max.y),
            );
            ui.small_label(row, &port.socket, theme.palette.text_dim, Align::Right);
        }
        for port in &layout.inputs {
            let row = Rect::from_min_max(
                Vec2::new(port.row.min.x + inset, port.row.min.y),
                Vec2::new(port.row.max.x - inset, port.row.max.y),
            );
            ui.small_label(row, &port.socket, theme.palette.text_dim, Align::Left);
            if port.connected {
                continue;
            }
            // An unconnected input shows what it will use: the value the
            // node pins, or the socket's default — which on a generic
            // socket is a scalar spread over whatever this instance
            // resolved to, so it needs the resolved type to be a value at
            // all (see `wxsl_core::node::Socket::default_for`).
            let value = node.params.get(&port.socket).copied().or_else(|| {
                let socket = definition.input(&port.socket)?;
                socket.default_for(graph.effective_type(layout.id, socket)?)
            });
            if let Some(value) = value {
                let text = crate::widgets::value_summary(&value);
                ui.small_label(row, &text, theme.palette.text_dim, Align::Right);
            }
        }
        self.draw_ports(ui, &layout);
    }

    /// A node's port circles.
    fn draw_ports(&self, ui: &mut Ui<'_>, layout: &NodeLayout) {
        let theme = *ui.theme();
        let radius = (theme.metrics.port_radius * self.view.zoom).max(2.0);
        let pointer = ui.input.pointer().unwrap_or(Vec2::splat(f32::MIN));
        for (kind, ports) in [
            (PortKind::Output, &layout.outputs),
            (PortKind::Input, &layout.inputs),
        ] {
            for port in ports {
                let hovered = port.center.distance(pointer) <= radius * 2.0;
                let color = theme.type_color(port.ty);
                ui.draw().circle(
                    port.center,
                    if hovered { radius * 1.35 } else { radius },
                    color,
                );
                // A filled port is connected (or, for an output, could be);
                // a hollow one is an input waiting for something.
                if kind == PortKind::Input && !port.connected {
                    ui.draw()
                        .circle(port.center, radius * 0.45, theme.palette.node);
                }
            }
        }
    }

    /// Start, continue and finish a link drag.
    fn handle_ports(
        &mut self,
        ui: &mut Ui<'_>,
        rect: Rect,
        graph: &mut Graph,
        registry: &NodeRegistry,
        response: &mut CanvasResponse,
    ) -> bool {
        let theme = *ui.theme();
        let radius = (theme.metrics.port_radius * self.view.zoom).max(2.0) * 2.0;
        let Some(pointer) = ui.input.pointer() else {
            return false;
        };
        let mut changed = false;

        let hit = self.layouts.iter().rev().find_map(|layout| {
            layout
                .port_at(pointer, radius)
                .map(|(kind, port)| (layout.id, kind, port.clone()))
        });

        if let Interaction::DraggingLink { from, kind } = self.interaction.clone() {
            // Draw the pending link to the pointer.
            let anchor = self.port_center(&from, kind).unwrap_or(pointer);
            let (start, end) = match kind {
                PortKind::Output => (anchor, pointer),
                PortKind::Input => (pointer, anchor),
            };
            self.draw_link(ui, start, end, theme.palette.link_pending, true);

            if ui.input.released(MouseButton::Left) {
                self.interaction = Interaction::Idle;
                if let Some((_, hit_kind, port)) = &hit {
                    if *hit_kind != kind {
                        let target = SocketRef::new(
                            self.layouts
                                .iter()
                                .find(|layout| {
                                    layout
                                        .inputs
                                        .iter()
                                        .chain(&layout.outputs)
                                        .any(|candidate| candidate.center == port.center)
                                })
                                .map(|layout| layout.id)
                                .unwrap_or(from.node),
                            port.socket.clone(),
                        );
                        let (output, input) = match kind {
                            PortKind::Output => (from.clone(), target),
                            PortKind::Input => (target, from.clone()),
                        };
                        // Re-plugging an input replaces whatever was there,
                        // which is what dropping a link on a busy input
                        // obviously means.
                        let previous = graph.disconnect(registry, &input);
                        match graph.connect(registry, output.clone(), input.clone()) {
                            Ok(()) => {
                                changed = true;
                                response.message = Some(format!("connected {output} → {input}"));
                            }
                            Err(error) => {
                                if let Some(edge) = previous {
                                    // The refusal changed nothing, so put
                                    // back what the optimistic unplug removed.
                                    let _ = graph.connect(registry, edge.from, edge.to);
                                }
                                response.message = Some(error.to_string());
                            }
                        }
                    }
                }
            }
            return changed;
        }

        if ui.input.pressed(MouseButton::Left) && !ui.pointer_claimed() {
            if let Some((node, kind, port)) = hit {
                let socket = SocketRef::new(node, port.socket.clone());
                match kind {
                    // Dragging from a connected input picks the existing link
                    // up by its far end, so a link can be moved rather than
                    // deleted and redrawn.
                    PortKind::Input if port.connected => {
                        if let Some(edge) = graph.disconnect(registry, &socket) {
                            changed = true;
                            self.interaction = Interaction::DraggingLink {
                                from: edge.from,
                                kind: PortKind::Output,
                            };
                        }
                    }
                    _ => {
                        self.interaction = Interaction::DraggingLink { from: socket, kind };
                    }
                }
                ui.claim_pointer();
                let _ = rect;
            }
        }
        changed
    }

    /// Selection and node dragging.
    fn handle_nodes(
        &mut self,
        ui: &mut Ui<'_>,
        rect: Rect,
        graph: &mut Graph,
        clicked: Option<NodeId>,
    ) -> bool {
        let mut changed = false;
        let pointer = ui.input.pointer();

        if matches!(self.interaction, Interaction::Idle) && !ui.pointer_claimed() {
            if let (Some(node), Some(point)) = (clicked, pointer) {
                let additive = ui.input.modifiers().shift;
                if additive {
                    if let Some(index) = self.selection.iter().position(|id| *id == node) {
                        self.selection.remove(index);
                    } else {
                        self.selection.push(node);
                    }
                } else if !self.selection.contains(&node) {
                    self.selection = vec![node];
                }
                let position = graph
                    .node(node)
                    .and_then(|node| node.position)
                    .map(Vec2::from_array)
                    .unwrap_or_default();
                self.interaction = Interaction::DraggingNode {
                    node,
                    grab: self.view.to_graph(rect, point) - position,
                };
                ui.claim_pointer();
            } else if clicked.is_none()
                && ui.input.pressed(MouseButton::Left)
                && pointer.is_some_and(|point| rect.contains(point))
                && self.hovered_link.is_none()
            {
                self.selection.clear();
            }
        }

        if let Interaction::DraggingNode { node, grab } = self.interaction {
            if ui.input.is_down(MouseButton::Left) {
                if let Some(point) = pointer {
                    let target = self.view.to_graph(rect, point) - grab;
                    let moved = graph
                        .node(node)
                        .and_then(|node| node.position)
                        .map(Vec2::from_array)
                        .unwrap_or_default();
                    let delta = target - moved;
                    if delta != Vec2::ZERO {
                        // Move every selected node, so a multiple selection
                        // moves as one.
                        let selection = if self.selection.contains(&node) {
                            self.selection.clone()
                        } else {
                            vec![node]
                        };
                        for id in selection {
                            if let Some(node) = graph.node_mut(id) {
                                let position =
                                    Vec2::from_array(node.position.unwrap_or_default()) + delta;
                                node.position = Some(position.to_array());
                            }
                        }
                        // A move changes the document but not the shader, so
                        // the caller should save but need not recompile.
                        changed = false;
                    }
                }
            } else {
                self.interaction = Interaction::Idle;
            }
        }
        changed
    }

    /// Where a socket's port is on screen, from the last layout.
    fn port_center(&self, socket: &SocketRef, kind: PortKind) -> Option<Vec2> {
        let layout = self
            .layouts
            .iter()
            .find(|layout| layout.id == socket.node)?;
        let ports = match kind {
            PortKind::Input => &layout.inputs,
            PortKind::Output => &layout.outputs,
        };
        ports
            .iter()
            .find(|port| port.socket == socket.socket)
            .map(|port| port.center)
    }

    /// A socket's type, from the last layout.
    fn port_type(&self, socket: &SocketRef, kind: PortKind) -> Option<ValueType> {
        let layout = self
            .layouts
            .iter()
            .find(|layout| layout.id == socket.node)?;
        let ports = match kind {
            PortKind::Input => &layout.inputs,
            PortKind::Output => &layout.outputs,
        };
        ports
            .iter()
            .find(|port| port.socket == socket.socket)
            .map(|port| port.ty)
    }
}

/// Lay every node of `graph` out.
pub fn layout_graph(
    graph: &Graph,
    registry: &NodeRegistry,
    view: &View,
    canvas: Rect,
    theme: &Theme,
) -> Vec<NodeLayout> {
    graph
        .nodes()
        .filter_map(|(id, node)| {
            let definition = registry.get(&node.def)?;
            let position = Vec2::from_array(node.position.unwrap_or_default());
            let connected = |socket: &str| graph.edge_into(&SocketRef::new(id, socket)).is_some();
            let display_ty =
                |socket: &Socket| graph.effective_type(id, socket).unwrap_or(socket.ty);
            let queries = SocketQueries {
                connected: &connected,
                display_ty: &display_ty,
            };
            Some(layout_node(
                id, position, definition, &queries, view, canvas, theme,
            ))
        })
        .collect()
}

/// The graph-space bounds of every node, including their bodies.
pub fn graph_bounds(graph: &Graph, registry: &NodeRegistry, theme: &Theme) -> Option<(Vec2, Vec2)> {
    let mut bounds: Option<(Vec2, Vec2)> = None;
    for (_, node) in graph.nodes() {
        let position = Vec2::from_array(node.position.unwrap_or_default());
        let rows = registry
            .get(&node.def)
            .map(|definition| definition.inputs.len() + definition.outputs.len())
            .unwrap_or(0);
        let size = Vec2::new(
            theme.metrics.node_width / theme.scale,
            (theme.metrics.node_header_height + rows as f32 * theme.metrics.row_height)
                / theme.scale,
        );
        let (min, max) = (position, position + size);
        bounds = Some(match bounds {
            None => (min, max),
            Some((low, high)) => (low.min(min), high.max(max)),
        });
    }
    bounds
}

/// Give every node without a position one, in dependency order.
///
/// A graph from the node format may have no positions at all (nothing but the
/// editor writes them), and a pile of nodes at the origin is unusable. Depth
/// from the output node becomes the column, so the graph reads right to left
/// the way it was authored, and siblings stack.
pub fn auto_layout(graph: &mut Graph, registry: &NodeRegistry) {
    if graph.nodes().all(|(_, node)| node.position.is_some()) {
        return;
    }
    let order = match graph.topological_order(None) {
        Ok(order) => order,
        // A cycle cannot be laid out in dependency order; the graph is
        // invalid anyway and validation will say so.
        Err(_) => graph.nodes().map(|(id, _)| id).collect(),
    };

    // Longest path from any source, which puts a node to the right of
    // everything it consumes.
    let mut depth: BTreeMap<NodeId, usize> = BTreeMap::new();
    for id in &order {
        let own = graph
            .edges_into(*id)
            .filter_map(|edge| depth.get(&edge.from.node).copied())
            .max()
            .map(|deepest| deepest + 1)
            .unwrap_or(0);
        depth.insert(*id, own);
    }

    // Graph units, not pixels: a stored position is scale-independent, and
    // the metrics at scale 1 *are* graph units.
    let metrics = crate::theme::Metrics::DEFAULT;
    let column_width = metrics.node_width + 70.0;
    let row_gap = 28.0;
    // How far down each column has been filled. Stacking by each node's own
    // height rather than by a fixed row is the difference between a readable
    // first impression and a pile of overlapping nodes: a surface output has
    // seven inputs and a constant has none.
    let mut filled: BTreeMap<usize, f32> = BTreeMap::new();
    for id in order {
        let column = depth.get(&id).copied().unwrap_or(0);
        let rows = graph
            .node(id)
            .and_then(|node| registry.get(&node.def))
            .map(|definition| definition.inputs.len() + definition.outputs.len())
            .unwrap_or(0);
        let height = metrics.node_header_height + rows as f32 * metrics.row_height;
        let top = filled.entry(column).or_insert(0.0);
        let position = Vec2::new(column as f32 * column_width, *top);
        *top += height + row_gap;
        if let Some(node) = graph.node_mut(id) {
            if node.position.is_none() {
                node.position = Some(position.to_array());
            }
        }
    }
}

/// Place a new node so that its top-left corner is where the user pointed.
///
/// [`Graph::add_resolved`] rather than `Graph::add`: a node dropped on the
/// canvas should be complete — its generic parameters at their default
/// types, its inputs holding a value of that type — rather than arrive
/// reporting an unresolved parameter and showing sockets with nothing to
/// edit. Connecting anything retypes it from there.
pub fn place_new_node(
    graph: &mut Graph,
    registry: &NodeRegistry,
    definition: &str,
    at: Vec2,
) -> NodeId {
    graph.add_resolved(registry, Node::new(definition).with_position(at.to_array()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxsl_core::node::{Socket, ValueType};

    fn definition() -> NodeDefinition {
        NodeDefinition::builder("math.add.f32", "Add")
            .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
            .input(Socket::new("b", ValueType::F32).with_splat_default(0.0))
            .output(Socket::new("out", ValueType::F32))
            .expr("{a} + {b}")
    }

    fn canvas_rect() -> Rect {
        Rect::new(0.0, 0.0, 800.0, 600.0)
    }

    #[test]
    fn screen_and_graph_coordinates_round_trip() {
        let rect = canvas_rect();
        for view in [
            View::default(),
            View {
                pan: Vec2::new(-120.0, 45.0),
                zoom: 2.5,
            },
            View {
                pan: Vec2::new(1000.0, -1000.0),
                zoom: 0.3,
            },
        ] {
            let point = Vec2::new(37.0, -12.0);
            let round_tripped = view.to_graph(rect, view.to_screen(rect, point));
            assert!(
                (round_tripped - point).length() < 1e-3,
                "{point:?} became {round_tripped:?}"
            );
        }
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer_still() {
        // The property that makes a wheel feel right, and the one that is
        // easy to get subtly wrong.
        let rect = canvas_rect();
        let mut view = View::default();
        let pointer = Vec2::new(500.0, 300.0);
        let before = view.to_graph(rect, pointer);
        view.zoom_about(rect, pointer, 1.6);
        let after = view.to_graph(rect, pointer);
        assert!((after - before).length() < 1e-3, "{before:?} vs {after:?}");
        assert!(view.zoom > 1.0);
    }

    #[test]
    fn zoom_stays_within_its_range() {
        let rect = canvas_rect();
        let mut view = View::default();
        for _ in 0..100 {
            view.zoom_about(rect, rect.center(), 2.0);
        }
        assert_eq!(view.zoom, View::ZOOM_RANGE.1);
        for _ in 0..200 {
            view.zoom_about(rect, rect.center(), 0.5);
        }
        assert_eq!(view.zoom, View::ZOOM_RANGE.0);
    }

    #[test]
    fn a_node_lays_its_ports_out_on_its_edges() {
        let theme = Theme::default();
        let definition = definition();
        let queries = SocketQueries {
            connected: &|_| false,
            display_ty: &|socket| socket.ty,
        };
        let layout = layout_node(
            NodeId(1),
            Vec2::new(10.0, 20.0),
            &definition,
            &queries,
            &View::default(),
            canvas_rect(),
            &theme,
        );
        assert_eq!(layout.outputs.len(), 1);
        assert_eq!(layout.inputs.len(), 2);
        // Outputs on the right edge, inputs on the left.
        assert_eq!(layout.outputs[0].center.x, layout.rect.max.x);
        for port in &layout.inputs {
            assert_eq!(port.center.x, layout.rect.min.x);
        }
        // Rows do not overlap, and are inside the body.
        assert!(layout.outputs[0].row.max.y <= layout.inputs[0].row.min.y + 0.01);
        assert!(layout.inputs[0].row.min.y >= layout.header.max.y);
        assert!(layout.inputs[1].row.max.y <= layout.rect.max.y);
        // The whole node is where it was put.
        assert_eq!(layout.rect.min, Vec2::new(10.0, 20.0));
    }

    #[test]
    fn a_generic_ports_colour_follows_its_resolved_type_not_the_placeholder() {
        // The bug this pins down: `socket.ty` on a generic socket is only a
        // placeholder (always `f32` for `math.add`), and using it directly
        // for a port's colour would show every generic node as if it were
        // still unresolved `f32`, however it was actually resolved.
        let mut registry = NodeRegistry::new();
        registry.register(
            NodeDefinition::builder("math.add", "Add")
                .generic_param(wxsl_core::node::GenericParam::new(
                    "T",
                    vec![ValueType::F32, ValueType::Vec3],
                ))
                .input(Socket::new("a", ValueType::F32).generic("T"))
                .input(Socket::new("b", ValueType::F32).generic("T"))
                .output(Socket::new("out", ValueType::F32).generic("T"))
                .expr("{a} + {b}"),
        );
        let mut graph = Graph::new("test");
        let add = graph.add_node("math.add");
        graph
            .set_generic(&registry, add, "T", ValueType::Vec3)
            .expect("vec3f is allowed");

        let layouts = layout_graph(
            &graph,
            &registry,
            &View::default(),
            canvas_rect(),
            &Theme::default(),
        );
        let layout = &layouts[0];
        assert_eq!(layout.outputs[0].ty, ValueType::Vec3, "{layout:?}");
        for port in &layout.inputs {
            assert_eq!(port.ty, ValueType::Vec3, "{port:?}");
        }
    }

    #[test]
    fn a_ports_hit_test_prefers_the_nearest_one() {
        let theme = Theme::default();
        let definition = definition();
        let queries = SocketQueries {
            connected: &|_| false,
            display_ty: &|socket| socket.ty,
        };
        let layout = layout_node(
            NodeId(1),
            Vec2::ZERO,
            &definition,
            &queries,
            &View::default(),
            canvas_rect(),
            &theme,
        );
        // Right on the first input.
        let first = layout.inputs[0].center;
        let (kind, port) = layout.port_at(first, 8.0).expect("a port is there");
        assert_eq!(kind, PortKind::Input);
        assert_eq!(port.socket, "a");

        // Between two inputs, but nearer the second.
        let between = (layout.inputs[0].center + layout.inputs[1].center * 3.0) / 4.0;
        let (_, nearer) = layout.port_at(between, 100.0).expect("within radius");
        assert_eq!(nearer.socket, "b");

        // Nowhere near anything.
        assert!(layout.port_at(Vec2::new(400.0, 400.0), 8.0).is_none());
    }

    #[test]
    fn a_links_curve_leaves_its_output_rightwards() {
        // What makes a backwards link readable instead of a straight line
        // through whatever is between the two nodes.
        let from = Vec2::new(100.0, 100.0);
        let to = Vec2::new(20.0, 200.0);
        let (control_from, control_to) = link_controls(from, to);
        assert!(control_from.x > from.x, "leaves the output to the right");
        assert!(control_to.x < to.x, "enters the input from the left");
    }

    #[test]
    fn the_distance_to_a_link_is_small_on_it_and_large_off_it() {
        let from = Vec2::new(0.0, 0.0);
        let to = Vec2::new(200.0, 0.0);
        // A straight horizontal link: the midpoint is on the curve.
        assert!(distance_to_link(from, to, Vec2::new(100.0, 0.0)) < 1.0);
        assert!(distance_to_link(from, to, Vec2::new(100.0, 60.0)) > 40.0);
        // And the endpoints are on it too.
        assert!(distance_to_link(from, to, from) < 1.0);
        assert!(distance_to_link(from, to, to) < 1.0);
    }

    #[test]
    fn auto_layout_puts_a_consumer_to_the_right_of_its_producer() {
        let mut registry = NodeRegistry::new();
        registry.register(definition());
        let mut graph = Graph::new("test");
        let first = graph.add_node("math.add.f32");
        let second = graph.add_node("math.add.f32");
        graph
            .wire(&registry, (first, "out"), (second, "a"))
            .expect("same type");

        auto_layout(&mut graph, &registry);
        let left = graph.node(first).and_then(|n| n.position).expect("placed");
        let right = graph.node(second).and_then(|n| n.position).expect("placed");
        assert!(right[0] > left[0], "{right:?} is not right of {left:?}");
    }

    #[test]
    fn auto_layout_stacks_by_height_so_nodes_do_not_overlap() {
        // Nodes in one column differ in height by a factor of several, so a
        // fixed row spacing overlaps the tall ones — which is what the first
        // screenshot of the editor showed.
        let mut registry = NodeRegistry::new();
        registry.register(definition());
        let tall = NodeDefinition::builder("math.tall", "Tall")
            .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
            .input(Socket::new("b", ValueType::F32).with_splat_default(0.0))
            .input(Socket::new("c", ValueType::F32).with_splat_default(0.0))
            .input(Socket::new("d", ValueType::F32).with_splat_default(0.0))
            .input(Socket::new("e", ValueType::F32).with_splat_default(0.0))
            .input(Socket::new("f", ValueType::F32).with_splat_default(0.0))
            .output(Socket::new("out", ValueType::F32))
            .expr("{a}");
        registry.register(tall);

        // Three roots, so all three land in the same column.
        let mut graph = Graph::new("test");
        let ids = [
            graph.add_node("math.tall"),
            graph.add_node("math.add.f32"),
            graph.add_node("math.tall"),
        ];
        auto_layout(&mut graph, &registry);

        let metrics = crate::theme::Metrics::DEFAULT;
        let mut boxes: Vec<(f32, f32)> = Vec::new();
        for id in ids {
            let node = graph.node(id).expect("node");
            let position = node.position.expect("placed");
            let rows = registry
                .get(&node.def)
                .map(|d| d.inputs.len() + d.outputs.len())
                .expect("registered");
            let height = metrics.node_header_height + rows as f32 * metrics.row_height;
            boxes.push((position[1], position[1] + height));
        }
        boxes.sort_by(|a, b| a.0.total_cmp(&b.0));
        for pair in boxes.windows(2) {
            assert!(
                pair[0].1 <= pair[1].0,
                "a node ending at {} overlaps one starting at {}",
                pair[0].1,
                pair[1].0
            );
        }
    }

    #[test]
    fn auto_layout_leaves_positions_the_document_already_had() {
        let mut registry = NodeRegistry::new();
        registry.register(definition());
        let mut graph = Graph::new("test");
        let placed = graph.add(Node::new("math.add.f32").with_position([12.0, 34.0]));
        let unplaced = graph.add_node("math.add.f32");
        graph
            .wire(&registry, (placed, "out"), (unplaced, "a"))
            .expect("same type");

        auto_layout(&mut graph, &registry);
        assert_eq!(
            graph.node(placed).and_then(|n| n.position),
            Some([12.0, 34.0]),
            "a stored position is the document's, not the editor's to move"
        );
        assert!(graph.node(unplaced).and_then(|n| n.position).is_some());
    }

    #[test]
    fn fitting_the_view_frames_every_node() {
        let mut registry = NodeRegistry::new();
        registry.register(definition());
        let mut graph = Graph::new("test");
        graph.add(Node::new("math.add.f32").with_position([0.0, 0.0]));
        graph.add(Node::new("math.add.f32").with_position([900.0, 700.0]));

        let theme = Theme::default();
        let rect = canvas_rect();
        let bounds = graph_bounds(&graph, &registry, &theme).expect("two nodes");
        let mut view = View::default();
        view.fit(rect, bounds);

        // Both corners of the bounds are on screen.
        for corner in [bounds.0, bounds.1] {
            let screen = view.to_screen(rect, corner);
            assert!(
                rect.expand(1.0).contains(screen),
                "{corner:?} landed at {screen:?}, outside {rect:?}"
            );
        }
    }

    #[test]
    fn an_empty_graph_has_no_bounds_to_fit() {
        let registry = NodeRegistry::new();
        let graph = Graph::new("empty");
        assert!(graph_bounds(&graph, &registry, &Theme::default()).is_none());
    }
}
