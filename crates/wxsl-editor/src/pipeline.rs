//! The pipeline canvas: the editor's second document, over the pipeline
//! registry
//! ([plan3 P5](../../../plan3.md)).
//!
//! A pipeline is a [`Graph`] like a material is; what differs is the
//! registry it is edited against and what an edit *does*. A material edit
//! regenerates a shader; a pipeline edit **compiles**: the document goes
//! through the public compiler
//! (`wxsl_render::pipeline_doc::compile`) and lands on the preview's
//! renderer with [`Preview::install_document`] — the same move the
//! gallery's document demos make — so the preview renders the real pass
//! list the nodes describe, live, by construction. A compile failure
//! keeps the last good pass list running and lands in the problems tab,
//! named by the document node at fault, which is what the compiler's
//! error type exists to guarantee.
//!
//! This module holds what the pipeline canvas is *besides* a material
//! canvas: its state ([`PipelineCanvas`]), the compile loop
//! ([`compile`]), its palette rows (the document vocabulary plus one row
//! per screen effect, [`palette_matches`]), its inspector (the selected
//! node's settings, and the G-buffer channel plan — plan2 P12's data,
//! finally with a face), and its bottom panel, which lists the compiled
//! **passes** instead of generated code. The canvas, the palette panel,
//! the widgets and the immediate-mode layer are the material editor's
//! own, as plan2's guard rail required — a second canvas over the same
//! model, not a second editor.

use wxsl_core::graph::{Graph, NodeId};
use wxsl_core::node::{GraphDomain, NodeRegistry, Value, ValueType};
use wxsl_core::pipeline as doc;
use wxsl_render::effect::EffectRegistry;
use wxsl_render::graph::RenderGraph;
use wxsl_render::pass::{PassKind, Policy};
use wxsl_render::pipeline::{
    gbuffer_layout_bytes_per_sample, PipelineConfig, StockPipeline, MAX_GBUFFER_BYTES_PER_SAMPLE,
};
use wxsl_render::pipeline_doc::{self, PipelineError};
use wxsl_render::ui::draw::Rect;

use crate::app::Requests;
use crate::canvas::{self, Canvas};
use crate::palette::{score, Match, NodePicker};
use crate::preview::Preview;
use crate::ui::{Align, Id, Ui};
use crate::widgets;

use glam::Vec2;

/// Which canvas is up.
///
/// The editor holds both documents at once; this only says which one the
/// panels and the shortcuts are pointed at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CanvasMode {
    /// The material graph, and the preview rendered with a stock
    /// pipeline.
    #[default]
    Material,
    /// The pipeline document, compiled live onto the same renderer.
    Pipeline,
}

impl CanvasMode {
    /// The modes, in toolbar order.
    pub const ALL: &'static [CanvasMode] = &[CanvasMode::Material, CanvasMode::Pipeline];

    /// The name the toolbar button shows.
    pub fn name(self) -> &'static str {
        match self {
            CanvasMode::Material => "material",
            CanvasMode::Pipeline => "pipeline",
        }
    }

    /// The other one.
    pub fn other(self) -> Self {
        match self {
            CanvasMode::Material => CanvasMode::Pipeline,
            CanvasMode::Pipeline => CanvasMode::Material,
        }
    }
}

/// Which bottom panel is showing while the pipeline canvas is up.
///
/// The material editor's WXSL/WGSL tabs have no pipeline analogue — a
/// document compiles to a pass list, not to text — so the tabs are its
/// two honest halves: what the document compiled *to*, and what stopped
/// it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PipelineTab {
    /// The compiled pass list, one row per pass.
    #[default]
    Passes,
    /// The compile errors, named by the node at fault.
    Problems,
}

impl PipelineTab {
    const ALL: &'static [PipelineTab] = &[PipelineTab::Passes, PipelineTab::Problems];

    fn index(self) -> usize {
        PipelineTab::ALL
            .iter()
            .position(|tab| *tab == self)
            .unwrap_or(0)
    }
}

/// The pipeline canvas's own state: the document, its view of it, its
/// palette, and the registry it is edited against.
///
/// The registry starts as the shipped vocabulary and is refreshed from
/// the renderer's effect registry at every compile — which is how the
/// derived `pass.compute.<effect>` nodes (plan3 N3) and an application's
/// own effects appear without this module knowing an effect by name.
pub struct PipelineCanvas {
    /// The document being edited.
    pub(crate) graph: Graph,
    /// The canvas drawing it — its own pan, zoom and selection, so
    /// switching canvases does not lose either.
    pub(crate) canvas: Canvas,
    /// The palette searching this canvas's rows.
    pub(crate) picker: NodePicker,
    /// What the document validates and draws against. Refreshed from the
    /// effect registry at every compile.
    pub(crate) registry: NodeRegistry,
    /// The last compile's errors; empty means the document is running.
    pub(crate) errors: Vec<String>,
    /// The document changed and the compiled pass list is stale.
    pub(crate) dirty: bool,
    /// The canvas has never been fitted to the graph.
    pub(crate) fit_pending: bool,
}

impl PipelineCanvas {
    /// Open on a stock preset's document — the starting point the plan
    /// names: the deferred pipeline, one edit away from a bloom chain.
    ///
    /// Preset documents carry no positions, and a pile of nodes at the
    /// origin is unusable, so they are laid out the way `Editor::new`
    /// lays out a loaded material.
    pub fn open_on_preset(preset: StockPipeline) -> Self {
        let mut graph = preset.document();
        canvas::auto_layout(&mut graph, &wxsl_core::pipeline::registry());
        PipelineCanvas {
            graph,
            canvas: Canvas::new(),
            picker: NodePicker::new(),
            registry: wxsl_core::pipeline::registry(),
            errors: Vec::new(),
            dirty: true,
            fit_pending: true,
        }
    }

    /// The document, for a caller that wants to change it — an
    /// application's own edit, or the screenshot mode. Marks it for
    /// recompilation.
    pub fn graph_mut(&mut self) -> &mut Graph {
        self.dirty = true;
        &mut self.graph
    }
}

/// Validate and compile a pipeline document.
///
/// Returns every error — an empty vec meaning "compiles" — and the pass
/// list when there were none. The flattening is the compiler's contract
/// made visible: a `PipelineError` already names the document node at
/// fault, and an invalid *graph* reports every problem at once, the way
/// the material preview has always done.
pub fn compile(
    document: &Graph,
    registry: &NodeRegistry,
    effects: &EffectRegistry,
    config: &PipelineConfig,
) -> (Vec<String>, Option<RenderGraph>) {
    match pipeline_doc::compile(document, registry, effects, config) {
        Ok(graph) => (Vec::new(), Some(graph)),
        Err(PipelineError::InvalidDocument(errors)) => (
            errors.0.iter().map(|error| error.to_string()).collect(),
            None,
        ),
        Err(error) => (vec![error.to_string()], None),
    }
}

/// The pipeline palette's rows: the document vocabulary, plus one row per
/// *screen* effect in the effect registry.
///
/// Compute effects are already rows — `document_registry` derives a
/// `pass.compute.<effect>` node per one (plan3 N3). A screen effect maps
/// onto the fixed `pass.screen`, so its row is that node with the effect
/// already named in the match's `preset`: dropping "bloom" places a
/// `pass.screen` whose `effect` is set, and the wire to its `image` is
/// the only thing left to draw. Searching and ranking are the palette's
/// own, so both sources order the same way.
pub fn palette_matches(
    picker: &NodePicker,
    registry: &NodeRegistry,
    effects: &EffectRegistry,
) -> Vec<Match> {
    let mut matches = picker.matches(registry, GraphDomain::Document, 200);
    if picker
        .category
        .as_ref()
        .is_none_or(|category| category == EFFECT_CATEGORY)
    {
        let query = picker.query.trim();
        let mut rows: Vec<Match> = effects
            .iter()
            .filter(|effect| !effect.is_compute())
            .filter_map(|effect| {
                let score = score(query, effect.id, effect.label, effect.description)?;
                Some(Match {
                    id: doc::PASS_SCREEN.to_string(),
                    label: effect.label.to_string(),
                    category: EFFECT_CATEGORY.to_string(),
                    score,
                    preset: Some((doc::SETTING_EFFECT.to_string(), effect.id.to_string())),
                })
            })
            .collect();
        matches.append(&mut rows);
        // Same order the registry's rows got, with the label as the
        // tie-break so two effect rows (which share an id) keep a stable
        // order between frames.
        matches.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.id.cmp(&b.id))
                .then_with(|| a.label.cmp(&b.label))
        });
        matches.truncate(200);
    }
    matches
}

/// The palette row that stands for a screen effect.
const EFFECT_CATEGORY: &str = "effect";

/// The pipeline palette's categories: the document vocabulary's, plus the
/// effect rows' own.
pub fn palette_categories(registry: &NodeRegistry, effects: &EffectRegistry) -> Vec<String> {
    let mut categories: Vec<String> = registry
        .categories_in(GraphDomain::Document)
        .into_iter()
        .map(str::to_string)
        .collect();
    if effects.iter().any(|effect| !effect.is_compute()) {
        categories.push(EFFECT_CATEGORY.to_string());
    }
    categories
}

/// The pipeline inspector: the selected node's settings and its effect's
/// parameters, then the channel plan the pipeline compiles its G-buffer
/// from.
///
/// The settings are the same rows the material inspector draws — a
/// document node's knobs are settings, and a setting edit is a
/// recompile. Below them, the *effect's* declared parameters (ADR 0042)
/// as live sliders — a move is a buffer write, not a recompile, which is
/// the whole point of a parameter being a uniform. The plan is
/// unconditional: it is not about the selection but about the pipeline,
/// and it is the data plan2 P12 promised a face.
pub(crate) fn inspector_panel(
    ui: &mut Ui<'_>,
    rect: Rect,
    state: &mut PipelineCanvas,
    preview: &mut Preview,
    selected: Option<NodeId>,
    requests: &mut Requests,
) {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let inner = ui.panel(rect, Some("pipeline"));

    let plan = preview.pipeline_config().plan();
    let plan = match plan {
        Ok(plan) => plan,
        // The renderer's own config is validated when its features are
        // set, so this arm exists for honesty rather than expectation.
        Err(error) => {
            ui.small_label(inner, &error.to_string(), theme.palette.error, Align::Left);
            return;
        }
    };
    let layout = plan.layout();
    let bytes = gbuffer_layout_bytes_per_sample(layout);

    // -- what will be drawn, so it can scroll ----------------------------
    let row = metrics.row_height + metrics.row_gap;
    let small_row = metrics.small_text_size * 1.4 + metrics.row_gap;
    let width = inner.width() - metrics.scrollbar_width;
    let mut content = metrics.row_gap;

    let selected_definition = selected
        .and_then(|id| state.graph.node(id))
        .map(|node| node.def.clone())
        .and_then(|def| state.registry.get(&def).cloned());

    // The selected pass's effect parameters, with the defaults the
    // descriptor declares: a slider starts from the value the renderer is
    // running, and falls back to the default before anything has moved.
    // An effect pass is the node whose `effect` setting names one; every
    // other document node has no parameters to show.
    let selected_effect: Vec<(String, ValueType, Value)> = match (&selected, &selected_definition) {
        (Some(id), Some(definition))
            if definition
                .settings
                .iter()
                .any(|setting| setting.name.as_str() == doc::SETTING_EFFECT) =>
        {
            let effect_id = state
                .graph
                .setting(&state.registry, *id, doc::SETTING_EFFECT)
                .unwrap_or_default()
                .to_string();
            preview
                .effects()
                .get(&effect_id)
                .map(|effect| {
                    effect
                        .parameters
                        .iter()
                        .map(|parameter| {
                            (
                                parameter.name.to_string(),
                                parameter.ty(),
                                parameter.default,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
        _ => Vec::new(),
    };

    let mut doc_height = 0.0;
    if let Some(definition) = &selected_definition {
        content += row * 3.0; // name, definition, category
        if !definition.doc.is_empty() {
            doc_height = ui
                .layout(
                    ui.state.ui_font,
                    &definition.doc,
                    wxsl_render::ui::TextOptions::new(metrics.small_text_size).wrapped(width),
                )
                .size
                .y;
            content += doc_height + metrics.row_gap;
        }
        content += (small_row + row) * definition.settings.len() as f32;
        for (name, ty, _) in &selected_effect {
            let param_id = Id::new("pipeline.param")
                .with(selected.map(|id| u64::from(id.0)).unwrap_or(0))
                .with(Id::new(name.as_str()).0);
            content += small_row
                + widgets::value_editor_height(ui, param_id, *ty, width)
                + metrics.row_gap;
        }
    }
    // The plan: header, one row per target, the budget bar, the sources.
    content += row * 2.0 + small_row * layout.len() as f32 + row * 2.0;

    let area = ui.scroll_area(
        Id::new("pipeline.inspector"),
        inner,
        Vec2::new(width, content),
    );
    let mut cursor = area.origin().y;
    let mut next = |height: f32| {
        let rect = Rect::from_min_size(Vec2::new(inner.min.x, cursor), Vec2::new(width, height));
        cursor += height + metrics.row_gap;
        rect
    };

    // -- the selected node ----------------------------------------------
    if let (Some(id), Some(definition)) = (selected, selected_definition) {
        let name_row = next(metrics.row_height);
        let (name_label, name_field) = name_row.split_left(width * 0.3);
        ui.small_label(name_label, "name", theme.palette.text_dim, Align::Left);
        let mut name = state
            .graph
            .node(id)
            .and_then(|node| node.label.clone())
            .unwrap_or_else(|| definition.label.clone());
        if ui
            .text_field(
                Id::new("pipeline.name").with(u64::from(id.0)),
                name_field,
                &mut name,
            )
            .changed
        {
            if let Some(node) = state.graph.node_mut(id) {
                let trimmed = name.trim();
                node.label = (!trimmed.is_empty() && trimmed != definition.label)
                    .then(|| trimmed.to_string());
            }
            requests.changed_metadata = true;
        }
        widgets::field_row(ui, next(metrics.row_height), "definition", &definition.id);
        widgets::field_row(
            ui,
            next(metrics.row_height),
            "category",
            &definition.category,
        );
        if doc_height > 0.0 {
            let doc_layout = ui.layout(
                ui.state.ui_font,
                &definition.doc,
                wxsl_render::ui::TextOptions::new(metrics.small_text_size).wrapped(width),
            );
            ui.draw()
                .text(&doc_layout, next(doc_height).min, theme.palette.text_dim);
        }

        // A document node's knobs are settings: a tag expression, a stage,
        // an effect id, a policy, a precision. Editing one is a
        // *declaration* change — the compiled pass list cannot mean the
        // same thing after it — so it is a recompile, not a redraw.
        for setting in &definition.settings {
            let setting_name = setting.name.as_str();
            ui.small_label(
                next(metrics.small_text_size * 1.4),
                &format!("{}  {}", setting.label, setting.doc),
                theme.palette.text_dim,
                Align::Left,
            );
            let mut value = state
                .graph
                .setting(&state.registry, id, setting_name)
                .unwrap_or_default()
                .to_string();
            let field_id = Id::new("pipeline.setting")
                .with(u64::from(id.0))
                .with(Id::new(setting_name).0);
            if ui
                .text_field(field_id, next(metrics.row_height), &mut value)
                .changed
            {
                state.graph.set_setting(id, setting_name, value.trim());
                state.dirty = true;
                requests.changed_metadata = true;
            }
        }

        // The effect's declared parameters — the knobs ADR 0042 put
        // behind `set_pass_param` — as live sliders. A move is a buffer
        // write the next frame presents: no variant, no recompile, which
        // is the whole point of a parameter being a uniform instead of a
        // `const`. The pass is addressed by its label, the name the
        // compiled pass list carries.
        if !selected_effect.is_empty() {
            let pass_label = state
                .graph
                .node(id)
                .and_then(|node| node.label.clone())
                .unwrap_or_else(|| definition.label.clone());
            for (name, ty, default) in &selected_effect {
                ui.small_label(
                    next(metrics.small_text_size * 1.4),
                    &format!("{name}  {ty}"),
                    theme.palette.text_dim,
                    Align::Left,
                );
                let param_id = Id::new("pipeline.param")
                    .with(u64::from(id.0))
                    .with(Id::new(name.as_str()).0);
                let editor_rect = next(widgets::value_editor_height(ui, param_id, *ty, width));
                let mut value = preview.pass_param(&pass_label, name).unwrap_or(*default);
                if widgets::value_editor(ui, param_id, editor_rect, &mut value) {
                    if let Err(error) = preview.set_pass_param(&pass_label, name, value) {
                        requests.message = Some(error.to_string());
                    }
                }
            }
        }
    }

    // -- the channel plan ------------------------------------------------
    let header = next(metrics.row_height);
    ui.separator(header);
    ui.label(header, "G-buffer plan", theme.palette.text, Align::Left);
    for target in layout {
        let row_rect = next(metrics.small_text_size * 1.4);
        // `split_right` returns (the right piece, the rest).
        let (source_rect, field_rect) = row_rect.split_right(row_rect.width() * 0.45);
        ui.truncated_label(
            field_rect,
            &format!("{}  {}", target.field, target.precision.name()),
            theme.palette.text,
            Align::Left,
        );
        let source = plan
            .requests()
            .iter()
            .find(|request| request.target.field == target.field)
            .map(|request| request.source.describe())
            .unwrap_or_else(|| "the base layout".to_string());
        ui.small_label(source_rect, &source, theme.palette.text_dim, Align::Right);
    }

    // The attachment budget, as a bar: the plan's bytes per sample against
    // what every device honours. A set that overruns it is a named error
    // at `set_lighting`; this bar is how it is seen before that.
    let budget_row = next(metrics.row_height);
    // (the right piece, the rest): the label sits right of the bar.
    let (label_rect, bar_rect) = budget_row.split_right(width * 0.42);
    let fraction = (bytes as f32 / MAX_GBUFFER_BYTES_PER_SAMPLE as f32).clamp(0.0, 1.0);
    ui.draw()
        .round_rect(bar_rect, metrics.radius, theme.palette.control);
    let filled = Rect::from_min_size(
        bar_rect.min,
        Vec2::new(bar_rect.width() * fraction, bar_rect.height()),
    );
    ui.draw()
        .round_rect(filled, metrics.radius, theme.palette.accent);
    ui.small_label(
        label_rect,
        &format!("{bytes} / {MAX_GBUFFER_BYTES_PER_SAMPLE} bytes"),
        theme.palette.text_dim,
        Align::Left,
    );

    let models = config_models(preview);
    let models_row = next(metrics.row_height);
    ui.small_label(
        models_row,
        &format!("models: {models}"),
        theme.palette.text_dim,
        Align::Left,
    );
    let features = config_features(preview);
    let features_row = next(metrics.row_height);
    ui.small_label(
        features_row,
        &format!("features: {features}"),
        theme.palette.text_dim,
        Align::Left,
    );
    area.end(ui);
}

/// The enabled lighting models, as one line for the plan section.
fn config_models(preview: &Preview) -> String {
    let names: Vec<&str> = preview
        .pipeline_config()
        .lighting
        .models()
        .iter()
        .map(|model| model.name)
        .collect();
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

/// The enabled material features, as one line for the plan section.
fn config_features(preview: &Preview) -> String {
    let names: Vec<&str> = preview
        .pipeline_config()
        .features
        .iter()
        .map(|request| request.source.name())
        .collect();
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

/// The bottom panel while the pipeline canvas is up: the compiled pass
/// list, and the compile errors.
///
/// The pass list is what the document *became* — the material editor's
/// code panels show generated text, and this is the pipeline's generated
/// thing. It is read off the renderer, so what it lists is what the
/// frame runs, not what the last successful compile produced.
pub(crate) fn passes_panel(
    ui: &mut Ui<'_>,
    rect: Rect,
    tab: PipelineTab,
    preview: &Preview,
    errors: &[String],
) -> PipelineTab {
    let theme = *ui.theme();
    let metrics = theme.metrics;
    let inner = ui.panel(rect, None);
    let (tabs_rect, body) = inner.split_top(metrics.row_height);

    let problem_label = if errors.is_empty() {
        "problems".to_string()
    } else {
        format!("problems ({})", errors.len())
    };
    let labels = ["passes", problem_label.as_str()];
    let (tabs_rect, meta_rect) = tabs_rect.split_left(280.0 * theme.scale);
    let chosen = ui.tabs(Id::new("pipeline.tabs"), tabs_rect, &labels, tab.index());
    let tab = PipelineTab::ALL[chosen.min(PipelineTab::ALL.len() - 1)];

    let passes = preview.render_graph().passes();
    let per_frame = passes
        .iter()
        .filter(|pass| pass.policy == Policy::PerFrame)
        .count();
    let meta = match tab {
        PipelineTab::Passes => format!(
            "{} passes · {} per frame · running",
            passes.len(),
            per_frame
        ),
        PipelineTab::Problems if errors.is_empty() => "the document compiles".to_string(),
        PipelineTab::Problems => format!("{} to fix", errors.len()),
    };
    ui.small_label(meta_rect, &meta, theme.palette.text_dim, Align::Right);

    let body = Rect::from_min_max(body.min + Vec2::new(0.0, metrics.row_gap), body.max);
    match tab {
        PipelineTab::Passes => {
            let row_height = metrics.row_height * 1.2;
            let content = Vec2::new(body.width(), passes.len() as f32 * row_height);
            let area = ui.scroll_area(Id::new("pipeline.passes"), body, content);
            let origin = area.origin();
            let first = ((area.offset.y / row_height).floor() as usize).saturating_sub(1);
            let visible = (body.height() / row_height).ceil() as usize + 2;
            let last = (first + visible).min(passes.len());
            for (offset, pass) in passes[first..last].iter().enumerate() {
                let index = first + offset;
                let row = Rect::from_min_size(
                    Vec2::new(origin.x, origin.y + index as f32 * row_height),
                    Vec2::new(body.width() - metrics.scrollbar_width, row_height),
                );
                let (index_rect, rest) = row.split_left(metrics.small_text_size * 2.5);
                ui.small_label(
                    index_rect,
                    &format!("{}", index + 1),
                    theme.palette.text_dim,
                    Align::Left,
                );
                let (kind_rect, rest) = rest.split_left(body.width() * 0.28);
                ui.small_label(
                    kind_rect,
                    &pass_kind_text(&pass.kind),
                    theme.palette.text_dim,
                    Align::Left,
                );
                // (the right piece, the rest): policy hugs the row's edge.
                let (policy_rect, label_rect) = rest.split_right(metrics.small_text_size * 8.0);
                ui.truncated_label(label_rect, &pass.label, theme.palette.text, Align::Left);
                ui.small_label(
                    policy_rect,
                    pass.policy.name(),
                    theme.palette.text_dim,
                    Align::Right,
                );
            }
            area.end(ui);
        }
        PipelineTab::Problems => {
            if errors.is_empty() {
                ui.label(
                    body,
                    "the document compiles",
                    theme.palette.text_dim,
                    Align::Center,
                );
            } else {
                // One error per paragraph, wrapped — the material
                // problems tab's reasoning, unchanged: a diagnostic's
                // offending line is the useful half.
                ui.code_view(Id::new("pipeline.problems"), body, &errors.join("\n\n"));
            }
        }
    }
    tab
}

/// What a compiled pass does, as the passes panel's second column.
fn pass_kind_text(kind: &PassKind) -> String {
    match kind {
        PassKind::Geometry { stage, .. } => format!("draws · {}", stage.name()),
        PassKind::Screen { effect } => format!("screen · {effect}"),
        PassKind::Compute { effect } => format!("compute · {effect}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxsl_core::graph::SocketRef;
    use wxsl_render::pipeline::TargetConfig;
    use wxsl_render::EffectRegistry;

    /// The registry, effects and config a document compiles against, the
    /// way the canvas assembles them at compile time.
    fn harness() -> (NodeRegistry, EffectRegistry, PipelineConfig) {
        let effects = EffectRegistry::shipped();
        let registry = pipeline_doc::document_registry(&effects);
        let config =
            PipelineConfig::new(TargetConfig::new(512, 512, wgpu::TextureFormat::Rgba8Unorm));
        (registry, effects, config)
    }

    /// The bloom edit the done-when names: a `pass.screen` running bloom
    /// dropped between the deferred preset's intermediate target and its
    /// display transform — with a `resource.color` of its own to write
    /// into, because what a chained pass writes is a resource, not the
    /// frame. This is what the canvas's user draws by hand, as data.
    fn add_bloom(document: &mut Graph, registry: &NodeRegistry) {
        let tonemap = document
            .nodes()
            .find(|(_, node)| node.label.as_deref() == Some("tonemap"))
            .map(|(id, _)| id)
            .expect("the deferred preset ends in tonemap");
        let scene = document
            .nodes()
            .find(|(_, node)| node.def == doc::RESOURCE_COLOR)
            .map(|(id, _)| id)
            .expect("the deferred preset has an intermediate target");

        let bloom = document.add_node(doc::PASS_SCREEN);
        document.set_setting(bloom, doc::SETTING_EFFECT, "bloom");
        if let Some(node) = document.node_mut(bloom) {
            node.label = Some("bloom".to_string());
        }
        let bloomed = document.add_node(doc::RESOURCE_COLOR);
        document.set_setting(bloomed, doc::SETTING_PRECISION, "hdr");
        if let Some(node) = document.node_mut(bloomed) {
            node.label = Some("bloomed".to_string());
        }
        // The tonemap's image wire is picked up and re-dropped on bloom's
        // output — the same move dragging a connected input makes on the
        // canvas.
        document
            .wire(registry, (scene, "color"), (bloom, "image"))
            .expect("bloom takes an image");
        document
            .wire(registry, (bloomed, "color"), (bloom, "into"))
            .expect("bloom writes the new resource");
        document.disconnect(registry, &SocketRef::new(tonemap, "image"));
        document
            .wire(registry, (bloomed, "color"), (tonemap, "image"))
            .expect("tonemap takes bloom's output");
    }

    #[test]
    fn the_deferred_preset_opens_and_compiles() {
        let (registry, effects, config) = harness();
        let state = PipelineCanvas::open_on_preset(StockPipeline::Deferred);
        let (errors, graph) = compile(&state.graph, &registry, &effects, &config);
        assert!(errors.is_empty(), "{errors:?}");
        let graph = graph.expect("compiles");
        assert!(graph.passes().len() > 3, "a real pass list");
    }

    #[test]
    fn a_bloom_node_dropped_on_the_preset_compiles_to_a_bloom_pass() {
        let (registry, effects, config) = harness();
        let mut state = PipelineCanvas::open_on_preset(StockPipeline::Deferred);
        add_bloom(state.graph_mut(), &registry);
        let (errors, graph) = compile(&state.graph, &registry, &effects, &config);
        assert!(errors.is_empty(), "{errors:?}");
        let graph = graph.expect("compiles");
        assert!(
            graph
                .passes()
                .iter()
                .any(|pass| pass.label == "bloom" && pass.policy == Policy::PerFrame),
            "the compiled pass list runs bloom: {:?}",
            graph
                .passes()
                .iter()
                .map(|pass| &pass.label)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_broken_edit_reports_the_node_and_keeps_its_errors_flattened() {
        let (registry, effects, config) = harness();
        let mut state = PipelineCanvas::open_on_preset(StockPipeline::Deferred);
        // Unfeed `present`: a structural mistake the graph model catches.
        let present = state
            .graph
            .nodes()
            .find(|(_, node)| node.def == doc::PRESENT)
            .map(|(id, _)| id)
            .expect("the preset presents");
        state
            .graph
            .disconnect(&registry, &SocketRef::new(present, "surface"));
        let (errors, graph) = compile(&state.graph, &registry, &effects, &config);
        assert!(graph.is_none());
        assert!(
            errors
                .iter()
                .any(|error| error.contains("bloom") || error.contains("surface")),
            "the compiler speaks about the placed node: {errors:?}"
        );
    }

    #[test]
    fn the_palette_lists_the_vocabulary_and_the_effects() {
        let (registry, effects, _) = harness();
        let mut picker = NodePicker::new();
        let matches = palette_matches(&picker, &registry, &effects);
        // One row per document node plus one per *screen* effect; the
        // shipped set carries no compute effects, but an application's
        // would already be registry rows of their own (plan3 N3).
        let screen_count = effects.iter().filter(|effect| !effect.is_compute()).count();
        assert!(screen_count >= 3, "the shipped set has screen effects");
        assert_eq!(
            matches.len(),
            registry.in_domain(GraphDomain::Document).count() + screen_count,
            "one row per document node plus one per screen effect"
        );

        picker.query = "bloom".to_string();
        let matches = palette_matches(&picker, &registry, &effects);
        let bloom = matches
            .iter()
            .find(|found| found.label == "bloom")
            .expect("bloom is offered");
        assert_eq!(bloom.id, doc::PASS_SCREEN);
        assert_eq!(
            bloom.preset,
            Some((doc::SETTING_EFFECT.to_string(), "wxsl.bloom".to_string())),
            "the row arrives with its effect named"
        );

        // And the bare pass is offered beside it, for wiring an effect by
        // hand from the inspector.
        picker.query = "screen".to_string();
        let matches = palette_matches(&picker, &registry, &effects);
        assert!(matches.iter().any(|found| found.id == doc::PASS_SCREEN));
    }

    #[test]
    fn an_effect_row_placed_places_a_pass_screen_with_its_effect_set() {
        let (registry, effects, config) = harness();
        let mut document = doc::document("placed");
        let scene = document.add_node(doc::SOURCE_SCENE);
        let bloom = document.add_node(doc::PASS_SCREEN);
        // What applying a row's preset does — the same call the canvas
        // makes when an effect row is dropped.
        document.set_setting(bloom, doc::SETTING_EFFECT, "bloom");
        document
            .wire(&registry, (scene, "draws"), (bloom, "image"))
            .expect_err("an image is not a draw list");

        // The placement is the point: the node exists and carries the
        // effect its row named; what compile then says is the document's
        // own business — here, that nothing reaches the frame.
        assert_eq!(
            document.setting(&registry, bloom, doc::SETTING_EFFECT),
            Some("bloom"),
            "the preset landed"
        );
        let (errors, _) = compile(&document, &registry, &effects, &config);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("bloom") || error.contains("surface")),
            "the compiler speaks about the placed node: {errors:?}"
        );
    }

    #[test]
    fn the_categories_include_the_effects_once_there_are_screen_ones() {
        let (registry, effects, _) = harness();
        let categories = palette_categories(&registry, &effects);
        assert!(categories.contains(&EFFECT_CATEGORY.to_string()));
        assert!(categories.contains(&"pass".to_string()));
    }
}
