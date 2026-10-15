//! The inspector: what is drawn below the preview for the selection,
//! the resolved material and the macro variables.

use super::*;

/// The inspector: the preview, the selected node, and the macro variables.
///
/// Everything below the preview scrolls, because the tallest node in the
/// library has seven inputs and the macro list grows with the graph — and a
/// panel that silently runs out of room is a panel whose bottom half nobody
/// knows exists.
///
/// The scroll area needs its content height *before* anything is drawn, so
/// every row's height is computed twice: once to reserve it and once as it
/// is drawn. Two of them are not a fixed row — a type picker wraps onto as
/// many rows as its labels need, and a value editor grows for a matrix's
/// grid or an open colour picker — so both passes go through the same
/// helpers ([`picker_grid`], [`widgets::value_editor_height`]) and the same
/// widget ids ([`param_widget_id`], [`node_color_id`]). Reserving one height
/// and drawing another is how a panel ends up with rows that overlap.
pub(super) fn inspector_panel(
    ui: &mut Ui<'_>,
    rect: Rect,
    preview: &Preview,
    graph: &Graph,
    registry: &NodeRegistry,
    selected: Option<NodeId>,
    requests: &mut Requests,
) {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let inner = ui.panel(rect, Some("preview"));

    // The A/B split's B side, in the header's right end: off, or a stock
    // pipeline drawn beside whatever A runs (plan3 N9).
    {
        let labels: Vec<String> = core::iter::once("A/B: off".to_string())
            .chain(
                StockPipeline::ALL
                    .iter()
                    .map(|pipeline| format!("B: {}", pipeline.name())),
            )
            .collect();
        let widest = labels
            .iter()
            .map(|label| ui.measure_ui(label).x)
            .fold(0.0_f32, f32::max);
        let width = widest + metrics.padding * 2.0 + metrics.row_height;
        let header = Rect::from_min_max(
            Vec2::new(rect.max.x - width - metrics.padding * 0.25, rect.min.y),
            Vec2::new(
                rect.max.x - metrics.padding * 0.25,
                rect.min.y + metrics.row_height,
            ),
        )
        .shrink(1.0);
        let options: Vec<(&str, bool)> = labels.iter().map(|l| (l.as_str(), true)).collect();
        let mut selected = preview
            .compare()
            .and_then(|b| StockPipeline::ALL.iter().position(|p| *p == b))
            .map_or(0, |index| index + 1);
        if ui
            .dropdown(Id::new("preview.compare"), header, &options, &mut selected)
            .changed
        {
            requests.compare = Some(match selected {
                0 => None,
                index => Some(StockPipeline::ALL[index - 1]),
            });
        }
    }

    // The preview, square, as wide as the panel allows.
    let side = inner.width().min(inner.height() * 0.45);
    let image = Rect::from_min_size(
        Vec2::new(inner.center().x - side * 0.5, inner.min.y),
        Vec2::splat(side),
    );
    ui.draw()
        .round_rect(image, metrics.radius, theme.palette.canvas);
    // With the split up, A left of the edge and B right of it — the same
    // frame from two pipelines, cut where the author drags.
    let edge = image.min.x + image.width() * preview.split;
    let mut handle = None;
    match preview.compare() {
        Some(b) => {
            let (left, right) = (
                Rect::from_min_max(image.min, Vec2::new(edge, image.max.y)),
                Rect::from_min_max(Vec2::new(edge, image.min.y), image.max),
            );
            let s = preview.split;
            ui.draw().image(
                left,
                preview.texture(),
                Vec2::ZERO,
                Vec2::new(s, 1.0),
                Color::WHITE,
                0.0,
            );
            ui.draw().image(
                right,
                preview.compare_texture(),
                Vec2::new(s, 0.0),
                Vec2::ONE,
                Color::WHITE,
                0.0,
            );
            let line = Rect::from_min_max(
                Vec2::new(edge - 1.0, image.min.y),
                Vec2::new(edge + 1.0, image.max.y),
            );
            ui.draw().rect(line, theme.palette.accent);
            let pad = metrics.padding * 0.5;
            let label_height = metrics.small_text_size * 1.4;
            let a_name = if preview.runs_document() {
                "document".to_string()
            } else {
                preview.pipeline().name().to_string()
            };
            ui.small_label(
                Rect::from_min_size(
                    image.min + Vec2::splat(pad),
                    Vec2::new(image.width() * 0.5, label_height),
                ),
                &format!("A: {a_name}"),
                theme.palette.text,
                Align::Left,
            );
            ui.small_label(
                Rect::from_min_size(
                    Vec2::new(image.center().x, image.min.y + pad),
                    Vec2::new(image.width() * 0.5 - pad, label_height),
                ),
                &format!("B: {}", b.name()),
                theme.palette.text,
                Align::Right,
            );
            // A strip either side of the edge drags it; the rest of the
            // image still turns the mesh.
            let grip = metrics.padding.max(6.0);
            handle = Some(Rect::from_min_max(
                Vec2::new(edge - grip, image.min.y),
                Vec2::new(edge + grip, image.max.y),
            ));
        }
        None => ui.draw().image(
            image,
            preview.texture(),
            Vec2::ZERO,
            Vec2::ONE,
            Color::WHITE,
            metrics.radius,
        ),
    }
    ui.draw().round_rect_border(
        image,
        metrics.radius,
        metrics.outline_width,
        theme.palette.outline,
    );
    if let Some(handle) = handle {
        let response = ui.interact(Id::new("preview.split"), handle);
        if response.dragging && response.drag_delta.x != 0.0 {
            requests.split = Some(preview.split + response.drag_delta.x / image.width());
        }
    }
    // Dragging the image turns the mesh, which is the gesture everyone tries
    // first on a 3D preview.
    let response = ui.interact(Id::new("preview.image"), image);
    if response.dragging && response.drag_delta.x != 0.0 {
        requests.spin_drag = Some(response.drag_delta.x);
    }
    // Alt+click probes the pixel (plan6 E2): the A side, under a split.
    if response.clicked && ui.input.modifiers().alt {
        let at = (ui.input.pointer_or_zero() - image.min) / image.size();
        requests
            .commands
            .push(Command::ProbePixel { x: at.x, y: at.y });
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
    // The widest type name a picker can offer, so the grid below wraps at a
    // width that actually fits its labels rather than a guessed column count.
    let button_width = ui.measure_ui(ValueType::Mat3.wxsl_type()).x + metrics.padding * 2.0;
    match &selected_definition {
        Some(definition) => {
            // name, definition, category, then the colour row.
            content += row * 4.0 + doc_height + metrics.row_gap;
            if let Some(id) = selected {
                if ui.color_picker_open(node_color_id(id)) {
                    content += metrics.row_gap + widgets::color_picker_height(&theme, width, false);
                }
            }
            content += (small_row + row) * definition.settings.len() as f32;
            for param in &definition.generics {
                let (_, rows) =
                    picker_grid(param.allowed.len(), width, metrics.row_gap, button_width);
                content += small_row + row * rows as f32;
            }
            for socket in &definition.inputs {
                // How tall a value's editor is depends on what the socket
                // resolved to — a matrix is a grid of fields — and on
                // whether its colour picker is open.
                let editor = match selected.and_then(|id| graph.effective_type(id, socket)) {
                    Some(ty) => widgets::value_editor_height(
                        ui,
                        param_widget_id(selected.expect("resolved implies selected"), socket),
                        ty,
                        width,
                    ),
                    None => metrics.row_height,
                };
                content += small_row + editor + metrics.row_gap;
            }
        }
        None => content += row,
    }
    if !macros.is_empty() {
        content += row * (1.0 + macros.len() as f32);
    }
    // The resolved material: its config, then one editor per parameter.
    let material_params = preview.material_params();
    let param_id = |name: &str| Id::new("inspector.material.param").with(Id::new(name).0);
    content += row
        * if preview.material().is_some() {
            4.0
        } else {
            2.0
        };
    if material_params.is_empty() {
        content += row;
    }
    for (name, value) in &material_params {
        content += small_row
            + widgets::value_editor_height(ui, param_id(name), value.ty(), width)
            + metrics.row_gap;
    }

    let area = ui.scroll_area(Id::new("inspector.scroll"), body, Vec2::new(width, content));
    let mut cursor = area.origin().y;
    // The row the type-picker grid is currently filling; a new one is taken
    // whenever a row is full.
    let mut picker_row = Rect::NOTHING;
    let mut next = |height: f32| {
        let rect = Rect::from_min_size(Vec2::new(body.min.x, cursor), Vec2::new(width, height));
        cursor += height + metrics.row_gap;
        rect
    };

    // -- the selected node ---------------------------------------------
    match (selected, selected_definition) {
        (Some(id), Some(definition)) => {
            // The node's own name, editable: one generic node kind stands
            // in for what used to be four, so "Add" on its own says less
            // than it did, and what a node is *for* ("noise scale",
            // "metal patches") is the label a reader of the graph wants.
            // Clearing it back to the definition's own label drops the
            // override.
            let name_row = next(metrics.row_height);
            let (name_label, name_field) = name_row.split_left(width * 0.3);
            ui.small_label(name_label, "name", theme.palette.text_dim, Align::Left);
            let mut name = graph
                .node(id)
                .and_then(|node| node.label.clone())
                .unwrap_or_else(|| definition.label.clone());
            if ui
                .text_field(
                    Id::new("inspector.name").with(u64::from(id.0)),
                    name_field,
                    &mut name,
                )
                .changed
            {
                let trimmed = name.trim();
                requests.commands.push(Command::SetLabel {
                    document: Document::Material,
                    node: id,
                    label: (!trimmed.is_empty() && trimmed != definition.label)
                        .then(|| trimmed.to_string()),
                });
            }
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

            // The node's own colour. A property of this node rather than of
            // its kind (see `wxsl_core::graph::Node::color`), so it can mark
            // what a part of the graph is *for* — the default is the same
            // for every node precisely so that a colour means something when
            // an author sets one.
            let color_row = next(metrics.row_height);
            let (color_label, color_rest) = color_row.split_left(width * 0.3);
            ui.small_label(color_label, "colour", theme.palette.text_dim, Align::Left);
            let picker_id = node_color_id(id);
            let current = graph.node(id).and_then(|node| node.color);
            let reset_width = ui.measure_ui("default").x + metrics.padding * 2.0;
            let (reset_rect, swatch_rect) = color_rest.split_right(reset_width);
            let swatch_rect = Rect::from_min_max(
                swatch_rect.min,
                swatch_rect.max - Vec2::new(metrics.row_gap, 0.0),
            );

            let shown = theme.node_color(graph.node(id).expect("selected node exists"));
            ui.draw().round_rect(swatch_rect, metrics.radius, shown);
            let swatch = ui.interact(picker_id.with(1), swatch_rect);
            if swatch.clicked {
                ui.toggle_color_picker(picker_id);
            }
            ui.draw().round_rect_border(
                swatch_rect,
                metrics.radius,
                metrics.outline_width * if swatch.hovered { 2.0 } else { 1.0 },
                if swatch.hovered {
                    theme.palette.selection
                } else {
                    theme.palette.outline
                },
            );
            if ui
                .button_colored(picker_id.with(2), reset_rect, "default", None)
                .clicked
                && current.is_some()
            {
                requests.commands.push(Command::SetColor {
                    document: Document::Material,
                    node: id,
                    color: None,
                });
            }
            if ui.color_picker_open(picker_id) {
                let rect = next(widgets::color_picker_height(&theme, width, false));
                // Starting from whatever is on screen, so opening the picker
                // on a node that never had a colour picks up where the
                // default left off instead of jumping to black.
                let mut components = current.unwrap_or([shown.r, shown.g, shown.b]).to_vec();
                if widgets::color_picker(ui, picker_id.with(3), rect, &mut components) {
                    requests.commands.push(Command::SetColor {
                        document: Document::Material,
                        node: id,
                        color: Some([components[0], components[1], components[2]]),
                    });
                }
            }

            // A node's *settings*: string-valued properties that change
            // what it compiles to rather than how it looks. A
            // `param.value`'s `name` is the uniform's identity — rename it
            // and the host writes a different field — which is why this is
            // not the node label above (ADR 0019, ADR 0023).
            for setting in &definition.settings {
                let setting_name = setting.name.as_str();
                ui.small_label(
                    next(metrics.small_text_size * 1.4),
                    &format!("{}  {}", setting.label, setting.doc),
                    theme.palette.text_dim,
                    Align::Left,
                );
                let current = graph
                    .setting(registry, id, setting_name)
                    .unwrap_or_default()
                    .to_string();
                let field_id = Id::new("inspector.setting")
                    .with(u64::from(id.0))
                    .with(Id::new(setting_name).0);
                if let Some(value) = widgets::setting_editor(
                    ui,
                    field_id,
                    next(metrics.row_height),
                    &current,
                    &setting.choices,
                ) {
                    // A declaration, not decoration: renaming a parameter
                    // changes the generated struct, so the material is
                    // recompiled rather than merely redrawn.
                    requests.commands.push(Command::SetSetting {
                        document: Document::Material,
                        node: id,
                        name: setting_name.to_string(),
                        value,
                    });
                }
            }

            // A generic node (one node kind serving every type it allows,
            // e.g. `math.add` instead of a separate `math.add.f32`/`.vec3f`/…
            // — see `wxsl_core::node::GenericParam`) needs its type picked
            // before its sockets mean anything. Connecting a wire already
            // resolves it automatically; this is for doing so by hand, and
            // for changing it later — which drops whatever wiring no longer
            // fits, exactly as switching from `math.add.f32` to
            // `math.add.vec3f` always would have.
            //
            // A node with two parameters (`math.multiply`'s `A` and `B`,
            // whose operands WGSL lets differ) gets one picker each.
            for param in &definition.generics {
                let param_name = param.name.as_str();
                let resolved = graph.generic_type(id, param_name);
                let label = match resolved {
                    Some(ty) => format!("{param_name}: {ty}"),
                    None => format!("{param_name}: ?"),
                };
                ui.small_label(
                    next(metrics.small_text_size * 1.4),
                    &label,
                    if resolved.is_some() {
                        theme.palette.text_dim
                    } else {
                        theme.palette.warning
                    },
                    Align::Left,
                );
                let gap = metrics.row_gap;
                let (columns, _) = picker_grid(param.allowed.len(), width, gap, button_width);
                for (index, &candidate) in param.allowed.iter().enumerate() {
                    let column = index % columns;
                    if column == 0 {
                        picker_row = next(metrics.row_height);
                    }
                    let cell_width =
                        (picker_row.width() - gap * (columns - 1) as f32) / columns as f32;
                    let button_rect = Rect::from_min_size(
                        Vec2::new(
                            picker_row.min.x + (cell_width + gap) * column as f32,
                            picker_row.min.y,
                        ),
                        Vec2::new(cell_width, picker_row.height()),
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
                        requests.commands.push(Command::SetGeneric {
                            document: Document::Material,
                            node: id,
                            param: param_name.to_string(),
                            ty: candidate,
                        });
                    }
                }
            }

            // Unconnected inputs are editable; connected ones say what
            // drives them, because a value nobody reads is a lie.
            for socket in &definition.inputs {
                let name = socket.name.as_str();
                let reference = wxsl_core::graph::SocketRef::new(id, name);
                let connected = graph.edge_into(&reference).is_some();
                // `socket.ty` is only a placeholder on a generic or
                // combined socket (see `wxsl_core::node::Socket::generic`/
                // `Socket::combine`); this instance's resolution is what the
                // label should actually say, and what a splat default has to
                // be spread over before it is a value at all.
                let resolved_ty = graph.effective_type(id, socket);
                let shown_ty = resolved_ty.unwrap_or(socket.ty);
                let value = graph
                    .node(id)
                    .and_then(|node| node.params.get(name).copied())
                    .or_else(|| resolved_ty.and_then(|ty| socket.default_for(ty)));
                ui.small_label(
                    next(metrics.small_text_size * 1.4),
                    &format!(
                        "{name}  {}",
                        widgets::typed_summary(shown_ty, value.as_ref())
                    ),
                    theme.palette.text_dim,
                    Align::Left,
                );
                let widget_id = param_widget_id(id, socket);
                let editor_rect = next(match resolved_ty {
                    Some(ty) => widgets::value_editor_height(ui, widget_id, ty, width),
                    None => metrics.row_height,
                });
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
                // A generic socket has no static default, so before this
                // instance resolved its type there is nothing to edit; once
                // it has, an editor starting from that type's zero is what
                // makes the value typeable at all. Nothing is pinned until
                // the user actually edits it, so validation still reports
                // the input as unfed in the meantime.
                let Some(mut value) = value.or_else(|| resolved_ty.and_then(|ty| ty.zero())) else {
                    ui.small_label(
                        editor_rect,
                        "pick a type above, or connect something",
                        theme.palette.warning,
                        Align::Left,
                    );
                    continue;
                };
                if widgets::value_editor(ui, widget_id, editor_rect, &mut value) {
                    requests.commands.push(Command::SetParam {
                        document: Document::Material,
                        node: id,
                        socket: name.to_string(),
                        value,
                    });
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

    // -- the resolved material (plan3 N9) --------------------------------
    // What `MaterialConfig::resolve` made of the graph under the running
    // plan, and the parameters as the buffer holds them: a drag here is a
    // buffer write, not a recompile.
    let header = next(metrics.row_height);
    ui.separator(header);
    ui.label(header, "material", theme.palette.text, Align::Left);
    match preview.material() {
        Some(material) => {
            let lighting = &material.config().lighting;
            let model = lighting
                .set
                .get(lighting.model)
                .map_or("?", |model| model.name);
            let set: Vec<&str> = lighting.set.models().iter().map(|m| m.name).collect();
            let features = preview.features();
            widgets::field_row(ui, next(metrics.row_height), "model", model);
            widgets::field_row(ui, next(metrics.row_height), "set", &set.join(", "));
            widgets::field_row(
                ui,
                next(metrics.row_height),
                "channels",
                &if features.is_empty() {
                    "none".to_string()
                } else {
                    features.join(", ")
                },
            );
        }
        None => {
            ui.small_label(
                next(metrics.row_height),
                "no material compiled",
                theme.palette.text_dim,
                Align::Left,
            );
        }
    }
    if material_params.is_empty() {
        ui.small_label(
            next(metrics.row_height),
            "no parameters — add a param node",
            theme.palette.text_dim,
            Align::Left,
        );
    }
    for (name, mut value) in material_params {
        ui.small_label(
            next(metrics.small_text_size * 1.4),
            &format!(
                "{name}  {}",
                widgets::typed_summary(value.ty(), Some(&value))
            ),
            theme.palette.text_dim,
            Align::Left,
        );
        let id = param_id(&name);
        let editor = next(widgets::value_editor_height(ui, id, value.ty(), width));
        if widgets::value_editor(ui, id, editor, &mut value) {
            requests
                .commands
                .push(Command::SetMaterialParam { name, value });
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
                requests.commands.push(Command::SetMacro {
                    document: Document::Material,
                    name: name.to_string(),
                    value: Some(value),
                });
            }
        }
    }

    area.end(ui);
}
