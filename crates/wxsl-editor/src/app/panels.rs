//! The toolbar, the palette and the status bar.

use super::*;

/// The toolbar: what the graph is, and how it is being shown.
///
/// The canvas toggle comes first — the two documents are the editor's
/// biggest fact — then the active document's name, then the knobs that
/// still apply. The stock-pipeline buttons are the material canvas's:
/// when the pipeline canvas is up, the document *is* the pipeline, and a
/// "forward" button over it would lie.
pub(super) fn toolbar_panel(
    ui: &mut Ui<'_>,
    rect: Rect,
    requests: &mut Requests,
    preview: &Preview,
    // Which canvas is up, whether the material previews through the
    // pipeline document, and that document's name.
    (mode, document): (CanvasMode, (bool, &str)),
    graph: &Graph,
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

    // The theme toggle, pinned to the top right: reserved before the
    // left-flowing buttons below claim any of `row`, so it stays in the same
    // place regardless of how many of them there are.
    let theme_label = format!("theme: {}", theme.mode.name());
    let theme_width = (ui.measure_ui(&theme_label).x + metrics.padding * 2.0).max(48.0);
    let (right, row) = row.split_right(theme_width + gap);
    let (_, theme_rect) = right.split_left(gap);
    if ui
        .button(Id::new("toolbar.theme"), theme_rect, &theme_label)
        .clicked
    {
        requests.toggle_theme = true;
    }

    let mut cursor = row;

    // Which canvas is up: two buttons, the active one accented.
    for candidate in CanvasMode::ALL {
        let label = candidate.name();
        let width = (ui.measure_ui(label).x + metrics.padding * 2.0).max(48.0);
        let (mode_rect, rest) = cursor.split_left(width);
        cursor = Rect::from_min_max(rest.min + Vec2::new(gap, 0.0), rest.max);
        let fill = (*candidate == mode).then_some(theme.palette.accent);
        if ui
            .button_colored(
                Id::new("toolbar.mode").with(*candidate as u64),
                mode_rect,
                label,
                fill,
            )
            .clicked
            && *candidate != mode
        {
            requests.mode = Some(*candidate);
        }
    }
    cursor = Rect::from_min_max(cursor.min + Vec2::new(gap, 0.0), cursor.max);

    // The graph's name, editable.
    let (name_rect, rest) = cursor.split_left(180.0 * theme.scale);
    let mut name = graph.name().to_string();
    if ui
        .text_field(Id::new("toolbar.name"), name_rect, &mut name)
        .changed
    {
        requests.commands.push(Command::SetName {
            document: if mode == CanvasMode::Pipeline {
                Document::Pipeline
            } else {
                Document::Material
            },
            name,
        });
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

    // A dropdown sized for its widest option, so it does not jump as the
    // choice changes.
    let dropdown_rect = |ui: &mut Ui<'_>, cursor: &mut Rect, labels: &[String]| {
        let widest = labels
            .iter()
            .map(|label| ui.measure_ui(label).x)
            .fold(0.0_f32, f32::max);
        let width = widest + metrics.padding * 2.0 + metrics.row_height;
        let (rect, rest) = cursor.split_left(width);
        *cursor = Rect::from_min_max(rest.min + Vec2::new(gap, 0.0), rest.max);
        rect
    };

    if mode != CanvasMode::Pipeline {
        // The stock pipelines, then whatever the pipeline canvas holds:
        // the material previews through that document, live.
        let (uses_document, document_name) = document;
        let mut labels: Vec<String> = StockPipeline::ALL
            .iter()
            .map(|pipeline| pipeline.name().to_string())
            .collect();
        labels.push(format!("document: {document_name}"));
        let mut selected = if uses_document {
            StockPipeline::ALL.len()
        } else {
            StockPipeline::ALL
                .iter()
                .position(|pipeline| *pipeline == preview.pipeline())
                .unwrap_or(0)
        };
        let rect = dropdown_rect(ui, &mut cursor, &labels);
        let options: Vec<(&str, bool)> = labels.iter().map(|l| (l.as_str(), true)).collect();
        if ui
            .dropdown(Id::new("toolbar.pipeline"), rect, &options, &mut selected)
            .changed
        {
            match StockPipeline::ALL.get(selected) {
                Some(pipeline) => requests.pipeline = Some(*pipeline),
                None => requests.use_document = true,
            }
        }

        // The debug views: what a shading pass returns instead of shading,
        // and the frame views over the pass list. One the running pass
        // list cannot show is listed, greyed.
        let views = DebugChoice::all(preview);
        let labels: Vec<String> = views
            .iter()
            .map(|(view, _)| format!("view: {}", view.label()))
            .collect();
        let options: Vec<(&str, bool)> = views
            .iter()
            .zip(&labels)
            .map(|((_, available), label)| (label.as_str(), *available))
            .collect();
        let current = DebugChoice::current(preview);
        let mut selected = views
            .iter()
            .position(|(view, _)| *view == current)
            .unwrap_or(0);
        let rect = dropdown_rect(ui, &mut cursor, &labels);
        if ui
            .dropdown(Id::new("toolbar.view"), rect, &options, &mut selected)
            .changed
        {
            requests.debug_view = Some(views[selected].0);
        }
    }
    let labels: Vec<String> = MeshKind::ALL
        .iter()
        .map(|kind| format!("mesh: {}", kind.name()))
        .collect();
    let mut selected = MeshKind::ALL
        .iter()
        .position(|kind| *kind == preview.mesh_kind())
        .unwrap_or(0);
    let rect = dropdown_rect(ui, &mut cursor, &labels);
    let options: Vec<(&str, bool)> = labels.iter().map(|l| (l.as_str(), true)).collect();
    if ui
        .dropdown(Id::new("toolbar.mesh"), rect, &options, &mut selected)
        .changed
    {
        requests.mesh = Some(MeshKind::ALL[selected]);
    }
    if button(ui, &mut cursor, "toolbar.fit", "fit", false) {
        requests.fit = true;
    }
    if button(ui, &mut cursor, "toolbar.tidy", "tidy", false) {
        requests.tidy = true;
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
///
/// The rows and categories arrive precomputed, because the two canvases
/// search different sources: the material registry filtered by the graph's
/// domain, and the pipeline registry plus its effect rows. The panel
/// itself — search box, category filter, list, drag source — is one piece
/// of widget vocabulary serving both.
pub(super) fn palette_panel(
    ui: &mut Ui<'_>,
    rect: Rect,
    picker: &mut NodePicker,
    matches: Vec<Match>,
    categories: &[String],
    node_count: usize,
    requests: &mut Requests,
) {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let title = if picker.open {
        "nodes — pick one to add".to_string()
    } else {
        format!("nodes ({node_count})")
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
    // The categories the caller decided this canvas can hold, not of the
    // whole registry: an empty category filter is worse than no filter
    // (ADR 0040).
    let button_height = metrics.row_height * 0.85;
    for (index, category) in std::iter::once("all")
        .chain(categories.iter().map(String::as_str))
        .enumerate()
    {
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
                requests.add = Some(AddNode {
                    definition: found.id.clone(),
                    preset: found.preset.clone(),
                });
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
            requests.add = Some(AddNode {
                definition: found.id.clone(),
                preset: found.preset.clone(),
            });
            requests.drop_screen_point = Some(ui.input.pointer_or_zero());
        }
        let (label_rect, id_rect) = row.shrink(3.0).split_top(row.height() * 0.55);
        ui.truncated_label(label_rect, &found.label, theme.palette.text, Align::Left);
        ui.small_label(id_rect, &found.id, theme.palette.text_dim, Align::Left);
    }
    area.end(ui);
}

/// The status bar: the numbers worth watching while editing.
pub(super) fn status_bar(
    ui: &mut Ui<'_>,
    rect: Rect,
    document: &StatusBarDocument,
    preview: &Preview,
    backend: MsdfBackend,
    (message, agent): (&str, Option<&str>),
    modified: bool,
) {
    let theme = *ui.theme();
    ui.draw().rect(rect, theme.palette.panel_header);
    let inner = rect.shrink(theme.metrics.padding * 0.5);

    let (variants, cache) = preview.variant_stats();
    let glyphs = ui.state.fonts.stats();
    let left = format!(
        "{} · {variants} variants ({} hits, {} compiles) · {} glyphs · msdf {} · atlas {:.0}%",
        document.text,
        cache.hits,
        cache.misses,
        glyphs.fields,
        backend.name(),
        ui.state.atlas.occupancy() * 100.0,
    );
    ui.small_label(inner, &left, theme.palette.text_dim, Align::Left);

    let color = if document.healthy {
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
    // An attached agent is always visible (ADR 0083).
    if let Some(agent) = agent {
        ui.small_label(
            inner,
            &format!("● agent: {agent}"),
            theme.palette.accent,
            Align::Center,
        );
    }
}
