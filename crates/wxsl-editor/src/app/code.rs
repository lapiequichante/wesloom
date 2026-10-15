//! The bottom panel's tabs: the generated code, the function editor,
//! the frame panel and the feature list.

use super::*;

/// The code panels: what the graph became, and what stopped it.
/// The last frame of `preview`, pass by pass, and the frame's counters.
pub(super) fn frame_summary(preview: &Preview, warnings: &[String]) -> FrameSummary {
    let renderer = preview.renderer();
    let graph = renderer.render_graph();
    let order = graph
        .schedule()
        .map(|schedule| schedule.order().to_vec())
        .unwrap_or_default();
    let passes: Vec<FramePass> = order
        .into_iter()
        .map(|index| {
            let label = graph.passes()[index].label.clone();
            FramePass {
                gpu_ms: renderer.pass_time(&label),
                draws: renderer.pass_draw_count(&label).unwrap_or(0),
                calls: renderer.pass_call_count(&label).unwrap_or(0),
                triangles: renderer.pass_triangle_count(&label).unwrap_or(0),
                label,
            }
        })
        .collect();
    let cache = renderer.cache_stats();
    let (local_lights, dropped_lights) = renderer.frame_bindings().light_grid_counts();
    FrameSummary {
        draws: passes.iter().map(|pass| pass.draws).sum(),
        calls: passes.iter().map(|pass| pass.calls).sum(),
        triangles: passes.iter().map(|pass| pass.triangles).sum(),
        passes,
        gpu_ms: renderer.frame_time(),
        pipelines: renderer.pipeline_count(),
        variants: renderer.variant_count(),
        cache_hits: cache.hits,
        cache_misses: cache.misses,
        local_lights,
        dropped_lights,
        warnings: warnings.to_vec(),
    }
}

pub(super) fn code_panel(
    ui: &mut Ui<'_>,
    rect: Rect,
    tab: CodeTab,
    preview: &Preview,
    live: FrameTab<'_>,
    functions: &mut Functions,
    requests: &mut Requests,
) -> CodeTab {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let inner = ui.panel(rect, None);
    let (tabs_rect, body) = inner.split_top(metrics.row_height);

    let problems = preview.status().errors().len() + preview.reload_errors().len();
    let problem_label = if problems == 0 {
        "problems".to_string()
    } else {
        format!("problems ({problems})")
    };
    let labels = [
        "WXSL",
        "WGSL",
        "features",
        "frame",
        problem_label.as_str(),
        "function",
    ];
    let (tabs_rect, meta_rect) = tabs_rect.split_left(84.0 * labels.len() as f32 * theme.scale);
    let chosen = ui.tabs(Id::new("code.tabs"), tabs_rect, &labels, tab.index());
    let tab = CodeTab::ALL[chosen.min(CodeTab::ALL.len() - 1)];

    let lines = match tab {
        CodeTab::Wxsl => preview.wxsl().lines().count(),
        CodeTab::Wgsl => preview.wgsl().lines().count(),
        _ => 0,
    };
    let meta = match tab {
        CodeTab::Wxsl => format!("{lines} lines · generated from the graph"),
        CodeTab::Wgsl => format!("{lines} lines · {} stage", preview.display_stage()),
        CodeTab::Features => {
            let on = preview.features();
            if on.is_empty() {
                "no channels · the plan is the base layout".to_string()
            } else {
                format!("{} · the budget bar moves with the plan", on.join(", "))
            }
        }
        CodeTab::Frame => match preview.stepping() {
            Some(step) => format!(
                "pass {} of {} · showing {}",
                step.count,
                step.passes.len(),
                step.shown.as_deref().unwrap_or("the frame")
            ),
            None => format!(
                "live · {} · step to freeze the frame",
                pipeline::gpu_time_text(preview)
            ),
        },
        CodeTab::Problems if problems == 0 => "the graph compiles".to_string(),
        CodeTab::Problems => format!("{problems} to fix"),
        CodeTab::Function => function_meta(functions.current()),
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
        CodeTab::Features => features_panel(ui, body, preview, requests),
        CodeTab::Frame => frame_panel(ui, body, preview, live, requests),
        CodeTab::Function => function_panel(ui, body, functions, requests),
        CodeTab::Problems => {
            let errors: Vec<&str> = preview
                .reload_errors()
                .iter()
                .chain(preview.status().errors())
                .map(String::as_str)
                .collect();
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

/// The function tab's meta line: where the caret's diagnostic says the
/// problem is, else what the last save made of the buffer.
fn function_meta(source: &FunctionSource) -> String {
    let (line, _) = source.buffer.line_column(source.buffer.cursor());
    let at_caret = source.marks.iter().find(|mark| mark.line == line);
    match (at_caret, source.marks.first(), &source.node) {
        (Some(mark), _, _) | (None, Some(mark), _) => {
            format!("line {}: {}", mark.line + 1, mark.message)
        }
        (None, None, Some(node)) if !source.is_modified() => {
            format!("saved as {node} · in the palette")
        }
        (None, None, Some(node)) => format!("{node} · edited · save to update the node"),
        (None, None, None) => "save (Ctrl+S) to make it a node".to_string(),
    }
}

/// The function panel (plan3 M11): a buffer list, new and save, and the
/// code editor over the buffer showing — diagnostics in its gutter.
fn function_panel(ui: &mut Ui<'_>, body: Rect, functions: &mut Functions, requests: &mut Requests) {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let gap = metrics.row_gap;
    let (bar, editor) = body.split_top(metrics.row_height);
    let editor = Rect::from_min_max(editor.min + Vec2::new(0.0, gap), editor.max);

    let names: Vec<String> = functions
        .buffers
        .iter()
        .map(|source| {
            if source.is_modified() && source.node.is_some() {
                format!("{} *", source.name())
            } else {
                source.name()
            }
        })
        .collect();
    let options: Vec<(&str, bool)> = names.iter().map(|name| (name.as_str(), true)).collect();
    let (list_rect, rest) = bar.split_left(180.0 * theme.scale);
    let mut current = functions.current;
    ui.dropdown(Id::new("function.list"), list_rect, &options, &mut current);
    functions.current = current.min(functions.buffers.len() - 1);
    let mut cursor = Rect::from_min_max(rest.min + Vec2::new(gap, 0.0), rest.max);
    for (id, label) in [("function.new", "new"), ("function.save", "save")] {
        let width = (ui.measure_ui(label).x + metrics.padding * 2.0).max(48.0);
        let (rect, rest) = cursor.split_left(width);
        cursor = Rect::from_min_max(rest.min + Vec2::new(gap, 0.0), rest.max);
        let fill = (id == "function.save" && functions.current().is_modified())
            .then_some(theme.palette.accent);
        if ui.button_colored(Id::new(id), rect, label, fill).clicked {
            match id {
                "function.new" => functions.open_new(),
                _ => requests.save_function = true,
            }
        }
    }

    let id = Id::new("function.editor").with(functions.current as u64);
    let source = functions.current_mut();
    let runs = source.runs().to_vec();
    ui.code_editor(id, editor, &mut source.buffer, &runs, &source.marks);
    if ui.state.focus() == Some(id) && ui.input.command_pressed(Key::Char('s')) {
        requests.save_function = true;
    }
}

/// The frame panel (plan3 N9): pass stepping.
///
/// Freezes the preview on one frame and runs its pass list cut after any
/// pass, showing what that pass wrote — or any texture the running passes
/// wrote — through the editor's inspect effects. The cut is a pass list
/// like any other, so what is shown is exactly what those passes did.
fn frame_panel(
    ui: &mut Ui<'_>,
    body: Rect,
    preview: &Preview,
    live: FrameTab<'_>,
    requests: &mut Requests,
) {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let gap = metrics.row_gap;
    let (bar, list) = body.split_top(metrics.row_height);
    let list = Rect::from_min_max(list.min + Vec2::new(0.0, gap), list.max);
    let step = preview.stepping();

    let mut cursor = bar;
    let mut button = |ui: &mut Ui<'_>, id: &str, label: &str, on: bool| {
        let width = (ui.measure_ui(label).x + metrics.padding * 2.0).max(32.0);
        let (rect, rest) = cursor.split_left(width);
        cursor = Rect::from_min_max(rest.min + Vec2::new(gap, 0.0), rest.max);
        let fill = on.then_some(theme.palette.accent);
        ui.button_colored(Id::new(id), rect, label, fill).clicked
    };
    let Some(step) = step else {
        if button(ui, "frame.step", "step", false) {
            requests.step = Some(StepRequest::Start);
        }
        if let Some(frame) = live.frame {
            ui.small_label(
                cursor,
                &format!(
                    "{} draws in {} calls · {} triangles · {} pipelines · {} variants ({} hits, {} misses) · \
                     {} local lights, {} dropped",
                    frame.draws,
                    frame.calls,
                    frame.triangles,
                    frame.pipelines,
                    frame.variants,
                    frame.cache_hits,
                    frame.cache_misses,
                    frame.local_lights,
                    frame.dropped_lights,
                ),
                theme.palette.text_dim,
                Align::Left,
            );
            live_frame(ui, list, frame, live.probe);
        }
        return;
    };
    if button(ui, "frame.step", "live", true) {
        requests.step = Some(StepRequest::Stop);
    }
    let pinned = step
        .pinned
        .then(|| {
            step.resources
                .iter()
                .find(|(_, label, _)| Some(label) == step.shown.as_ref())
                .map(|(id, _, _)| *id)
        })
        .flatten();
    if button(ui, "frame.previous", "previous", false) && step.count > 1 {
        requests.step = Some(StepRequest::To(step.count - 1, None));
    }
    if button(ui, "frame.next", "next", false) && step.count < step.passes.len() {
        requests.step = Some(StepRequest::To(step.count + 1, None));
    }

    // What to show: the last pass's output, or any texture written so far.
    let mut labels = vec!["show: last pass".to_string()];
    labels.extend(
        step.resources
            .iter()
            .map(|(_, label, _)| format!("show: {label}")),
    );
    let widest = labels
        .iter()
        .map(|label| ui.measure_ui(label).x)
        .fold(0.0_f32, f32::max);
    let width = (widest + metrics.padding * 2.0 + metrics.row_height).min(cursor.width());
    let (show_rect, _) = cursor.split_left(width);
    let mut options: Vec<(&str, bool)> = vec![(labels[0].as_str(), true)];
    options.extend(
        step.resources
            .iter()
            .zip(&labels[1..])
            .map(|((_, _, drawable), label)| (label.as_str(), *drawable)),
    );
    let mut selected = pinned
        .and_then(|id| step.resources.iter().position(|(r, _, _)| *r == id))
        .map_or(0, |index| index + 1);
    if ui
        .dropdown(Id::new("frame.show"), show_rect, &options, &mut selected)
        .changed
    {
        let show = selected.checked_sub(1).map(|index| step.resources[index].0);
        requests.step = Some(StepRequest::To(step.count, show));
    }

    // Every pass in schedule order: the running ones bright, the cut ones
    // dim, the last one run accented. A click runs up to that pass.
    let row = metrics.row_height;
    let width = list.width() - metrics.scrollbar_width;
    let content = step.passes.len() as f32 * (row + gap);
    let area = ui.scroll_area(Id::new("frame.passes"), list, Vec2::new(width, content));
    let origin = area.origin();
    for (index, label) in step.passes.iter().enumerate() {
        let rect = Rect::from_min_size(
            Vec2::new(list.min.x, origin.y + index as f32 * (row + gap)),
            Vec2::new(width, row),
        );
        let ran = index < step.count;
        let current = index + 1 == step.count;
        let response = ui.interact(Id::new("frame.pass").with(index as u64), rect);
        if current {
            ui.draw()
                .round_rect(rect, metrics.radius, theme.palette.accent);
        } else if response.hovered {
            ui.draw()
                .round_rect(rect, metrics.radius, theme.palette.control);
        }
        let color = if ran {
            theme.palette.text
        } else {
            theme.palette.text_dim
        };
        let text = rect.shrink(metrics.padding * 0.5);
        ui.truncated_label(
            text,
            &format!("{:>2}  {label}", index + 1),
            color,
            Align::Left,
        );
        if response.clicked && !current {
            requests.step = Some(StepRequest::To(index + 1, pinned));
        }
    }
    area.end(ui);
}

/// The frame tab while the frame runs live (plan6 E2): each pass's GPU
/// time, draws and triangles, the capability check's warnings, and what
/// the last probe (Alt+click on the viewport) read.
fn live_frame(ui: &mut Ui<'_>, list: Rect, frame: &FrameSummary, probe: Option<&ProbeSummary>) {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let row = metrics.row_height;
    let gap = metrics.row_gap;
    let mut lines: Vec<(String, Color)> = frame
        .passes
        .iter()
        .map(|pass| {
            let time = pass
                .gpu_ms
                .map_or_else(|| "    –   ".to_string(), |ms| format!("{ms:6.3} ms"));
            (
                format!(
                    "{time}  {:>5} draws  {:>5} calls  {:>9} tris  {}",
                    pass.draws, pass.calls, pass.triangles, pass.label
                ),
                theme.palette.text,
            )
        })
        .collect();
    lines.extend(
        frame
            .warnings
            .iter()
            .map(|warning| (warning.clone(), theme.palette.warning)),
    );
    match probe {
        Some(probe) => {
            let [r, g, b, _] = probe.shown;
            let mut head = format!(
                "probe ({}, {}) shows #{r:02x}{g:02x}{b:02x}",
                probe.pixel[0], probe.pixel[1]
            );
            if let (Some(instance), Some(material)) = (probe.instance, probe.material) {
                head.push_str(&format!(" · instance {instance}, material {material}"));
            }
            lines.push((head, theme.palette.accent));
            for resource in &probe.resources {
                // A buffer's head is 64 words: the line shows the first.
                let value = match &resource.value {
                    Some(channels) => {
                        let mut shown: Vec<String> = channels
                            .iter()
                            .take(8)
                            .map(|channel| format!("{channel:.4}"))
                            .collect();
                        if channels.len() > 8 {
                            shown.push("…".to_string());
                        }
                        shown.join(", ")
                    }
                    None => "not decoded".to_string(),
                };
                let layer = match resource.layer {
                    0 => String::new(),
                    layer => format!(" layer {layer}"),
                };
                lines.push((
                    format!(
                        "  {}{layer} ({}) [{value}]",
                        resource.resource, resource.format
                    ),
                    theme.palette.text,
                ));
            }
        }
        None => lines.push((
            "Alt+click the viewport to probe a pixel".to_string(),
            theme.palette.text_dim,
        )),
    }
    let width = list.width() - metrics.scrollbar_width;
    let content = lines.len() as f32 * (row + gap);
    let area = ui.scroll_area(Id::new("frame.live"), list, Vec2::new(width, content));
    let origin = area.origin();
    for (index, (text, color)) in lines.iter().enumerate() {
        let rect = Rect::from_min_size(
            Vec2::new(list.min.x, origin.y + index as f32 * (row + gap)),
            Vec2::new(width, row),
        );
        ui.truncated_label(
            rect.shrink(metrics.padding * 0.5),
            text,
            *color,
            Align::Left,
        );
    }
    area.end(ui);
}

/// The features panel (plan3 N9): the shipped feature registry as live
/// checkboxes over the pipeline's channel plan, and the attachment
/// budget as a bar.
///
/// Toggling a box asks for the whole set of checked names at once — a
/// channel change is a pass-list rebuild, not an increment — and the
/// editor regenerates the material against the widened plan on the next
/// frame, which is what makes the *image* move with the plan rather than
/// the frame failing its handshake check. The bar is the same
/// bytes-per-sample accounting the pipeline canvas's inspector draws;
/// here it is the thing the checkboxes spend.
fn features_panel(ui: &mut Ui<'_>, body: Rect, preview: &Preview, requests: &mut Requests) {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let features = wxsl_core::lighting::FEATURES;
    if features.is_empty() {
        ui.label(
            body,
            "no shipped features",
            theme.palette.text_dim,
            Align::Center,
        );
        return;
    }
    let enabled = preview.features();
    let (bytes, max) = match preview.pipeline_config().plan() {
        Ok(plan) => (
            wxsl_render::pipeline::gbuffer_layout_bytes_per_sample(plan.layout()),
            wxsl_render::pipeline::MAX_GBUFFER_BYTES_PER_SAMPLE,
        ),
        // The renderer's config is validated when its features are set,
        // so this arm is honesty rather than expectation.
        Err(error) => {
            ui.small_label(body, &error.to_string(), theme.palette.error, Align::Left);
            return;
        }
    };

    // The doc paragraphs are measurable, so the content height is exact.
    let width = body.width() - metrics.scrollbar_width;
    let small_row = metrics.small_text_size * 1.4 + metrics.row_gap;
    let mut content = metrics.row_gap * 2.0 + metrics.row_height;
    for feature in features {
        let doc = ui.layout(
            ui.state.ui_font,
            feature.doc,
            TextOptions::new(metrics.small_text_size).wrapped(width),
        );
        content += metrics.row_height + doc.size.y + small_row + metrics.row_gap;
    }
    let area = ui.scroll_area(Id::new("features.scroll"), body, Vec2::new(width, content));
    let mut cursor = area.origin().y;
    let mut next = |height: f32| {
        let rect = Rect::from_min_size(Vec2::new(body.min.x, cursor), Vec2::new(width, height));
        cursor += height + metrics.row_gap;
        rect
    };

    // The budget bar first: what every checkbox spends against.
    let budget_row = next(metrics.row_height);
    let (label_rect, bar_rect) = budget_row.split_right(width * 0.42);
    ui.small_label(
        label_rect,
        &format!("{bytes} / {max} bytes per sample"),
        theme.palette.text_dim,
        Align::Left,
    );
    ui.draw()
        .round_rect(bar_rect, metrics.radius, theme.palette.control);
    let fraction = (bytes as f32 / max as f32).clamp(0.0, 1.0);
    let filled = Rect::from_min_size(
        bar_rect.min,
        Vec2::new(bar_rect.width() * fraction, bar_rect.height()),
    );
    ui.draw()
        .round_rect(filled, metrics.radius, theme.palette.accent);

    // The request is the full set of names the boxes left — a channel
    // change is one pass-list rebuild, applied as one `set_features`.
    let mut names: Vec<&'static str> = Vec::new();
    let mut changed = false;
    for feature in features {
        let row = next(metrics.row_height);
        let mut on = enabled.contains(&feature.name);
        let response = ui.checkbox(
            Id::new("features.box").with(Id::new(feature.name).0),
            row,
            feature.name,
            &mut on,
        );
        if response.changed {
            changed = true;
        }
        if on {
            names.push(feature.name);
        }
        // The macro a material pins to turn the feature on, and the
        // channel it asks for — the two halves of the handshake, named.
        let macro_row = next(small_row);
        ui.small_label(
            macro_row,
            &format!(
                "macro {} · {}",
                feature.macro_name,
                feature.target.precision.name()
            ),
            theme.palette.text_dim,
            Align::Left,
        );
        let doc = ui.layout(
            ui.state.ui_font,
            feature.doc,
            TextOptions::new(metrics.small_text_size).wrapped(width),
        );
        let doc_rect = next(doc.size.y);
        ui.draw().text(&doc, doc_rect.min, theme.palette.text_dim);
    }
    area.end(ui);
    if changed {
        requests.features = Some(names);
    }
}
