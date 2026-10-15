//! The scene debug views that read the frame rather than a surface
//! (plan6 E2): the light grid's count per cluster, the shadow cascades, the
//! shadow atlas, wireframe and overdraw.
//!
//! Most are an editor effect, like the inspect effects of pass stepping,
//! appended to the running pass list as one screen pass over the resource
//! it reads: the scene's depth or the atlas. They stay out of the document
//! vocabulary for the same reason (`stepping::is_editor_effect`).
//! Wireframe is the renderer's line rasterization of the camera's passes,
//! and overdraw the forward passes blending a constant additively without
//! a depth test. The views that read a surface (albedo, normal, uv, ...)
//! are the shading passes' own (`lighting::debug_views`).

use wxsl_core::node::Value;
use wxsl_render::effect::{
    Effect, EffectInput, EffectInputKind, EffectKind, EffectParameter, EffectShader,
};
use wxsl_render::pass::{PassKind, PassView, ResourceId};
use wxsl_render::types::{
    BlendComponent, BlendFactor, BlendOperation, BlendState, CompareFunction,
};
use wxsl_render::RenderGraph;

use crate::preview::Preview;
use crate::stepping;

/// A view of the frame, by what it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameView {
    /// The camera's passes drawn as lines, shaded as usual.
    Wireframe,
    /// How many fragments each pixel shades, as brightness.
    Overdraw,
    /// How many local lights each cluster lists (ADR 0076).
    LightCount,
    /// Which shadow cascade covers each surface (ADR 0071).
    Cascades,
    /// The shadow atlas itself (ADR 0066).
    ShadowAtlas,
}

impl FrameView {
    /// Every frame view, in menu order.
    pub const ALL: [FrameView; 5] = [
        FrameView::Wireframe,
        FrameView::Overdraw,
        FrameView::LightCount,
        FrameView::Cascades,
        FrameView::ShadowAtlas,
    ];

    /// What a menu and a command call it.
    pub fn label(self) -> &'static str {
        match self {
            FrameView::Wireframe => "wireframe",
            FrameView::Overdraw => wxsl_core::lighting::OVERDRAW_VIEW,
            FrameView::LightCount => "light count",
            FrameView::Cascades => "cascades",
            FrameView::ShadowAtlas => "shadow atlas",
        }
    }

    /// The effect that draws it, for a view appended as a pass.
    pub(crate) fn effect(self) -> Option<&'static Effect> {
        match self {
            FrameView::LightCount => Some(&LIGHT_COUNT),
            FrameView::Cascades => Some(&CASCADES),
            FrameView::ShadowAtlas => Some(&ATLAS),
            FrameView::Wireframe | FrameView::Overdraw => None,
        }
    }

    /// The resource an appended view reads in `graph`, if the pass list
    /// has one: the atlas, or the depth the camera's shading passes test
    /// against.
    pub(crate) fn source(self, graph: &RenderGraph) -> Option<ResourceId> {
        match self {
            FrameView::ShadowAtlas => graph.shadow_maps(),
            FrameView::LightCount | FrameView::Cascades => scene_depth(graph),
            FrameView::Wireframe | FrameView::Overdraw => None,
        }
    }

    /// Whether `graph` has what the view reads; `lines` is whether the
    /// device rasterizes lines.
    pub(crate) fn available(self, graph: &RenderGraph, lines: bool) -> bool {
        match self {
            FrameView::Wireframe => lines && scene_depth(graph).is_some(),
            FrameView::Overdraw => {
                forward_shading(graph)
                    && !graph.passes().iter().any(|pass| {
                        matches!(&pass.kind, PassKind::Screen { effect }
                            if effect == "wxsl.deferred_lighting")
                    })
            }
            _ => self.source(graph).is_some(),
        }
    }

    /// The pass list that shows the view over `full`, every pass of which
    /// runs.
    pub(crate) fn install(self, full: &RenderGraph) -> Result<RenderGraph, String> {
        match self.effect() {
            Some(effect) => {
                let source = self.source(full).ok_or_else(|| {
                    format!("the running pass list has nothing for `{}`", self.label())
                })?;
                full.stepped(full.passes().len(), Some((source, effect.id)))
                    .map(RenderGraph::from)
                    .map_err(|error| error.to_string())
            }
            None if self == FrameView::Overdraw => Ok(overdraw(full)),
            None => Ok(full.clone()),
        }
    }
}

/// Whether a camera pass shades into colour targets: a forward pass.
pub(crate) fn forward_shading(graph: &RenderGraph) -> bool {
    graph.passes().iter().any(|pass| {
        matches!(&pass.kind, PassKind::Geometry { stage, .. }
            if pass.view == PassView::Camera
                && stage.output() == wxsl_core::abi::StageOutput::Color)
    })
}

/// `graph` with every forward pass counting fragments: no depth test or
/// write, no culling, each fragment added to the target. The materials
/// return a constant under the overdraw surface view.
fn overdraw(graph: &RenderGraph) -> RenderGraph {
    const ADD: BlendComponent = BlendComponent {
        src_factor: BlendFactor::One,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    };
    let mut counted = graph.clone();
    for index in 0..counted.passes().len() {
        let Some(pass) = counted.pass_mut(index) else {
            continue;
        };
        let forward = matches!(&pass.kind, PassKind::Geometry { stage, .. }
            if stage.output() == wxsl_core::abi::StageOutput::Color);
        if forward && pass.view == PassView::Camera {
            pass.state.depth_compare = CompareFunction::Always;
            pass.state.depth_write = false;
            pass.state.cull_mode = None;
            pass.state.blend = Some(BlendState {
                color: ADD,
                alpha: ADD,
            });
        }
    }
    counted
}

/// One entry of the view menu, which `set_debug_view` names by label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DebugChoice {
    /// Shade.
    Shaded,
    /// A surface view, by its `lighting::DebugView` id.
    Surface(i32),
    /// A frame view.
    Frame(FrameView),
}

impl DebugChoice {
    /// What the menu and a command call it.
    pub(crate) fn label(self) -> &'static str {
        match self {
            DebugChoice::Shaded => "shaded",
            DebugChoice::Surface(id) => wxsl_core::lighting::debug_views()
                .into_iter()
                .find(|view| view.id == id)
                .map_or("shaded", |view| view.label),
            DebugChoice::Frame(view) => view.label(),
        }
    }

    /// Every view, shaded first, and whether the running pass list can
    /// show it.
    pub(crate) fn all(preview: &Preview) -> Vec<(DebugChoice, bool)> {
        let mut choices = vec![(DebugChoice::Shaded, true)];
        // Overdraw's surface view is a constant that only its frame view
        // makes a count, so the menu offers the frame view alone.
        choices.extend(
            preview
                .debug_views()
                .into_iter()
                .filter(|(view, _)| view.label != wxsl_core::lighting::OVERDRAW_VIEW)
                .map(|(view, available)| (DebugChoice::Surface(view.id), available)),
        );
        choices.extend(
            FrameView::ALL
                .into_iter()
                .map(|view| (DebugChoice::Frame(view), preview.frame_view_available(view))),
        );
        choices
    }

    /// The view the preview shows.
    pub(crate) fn current(preview: &Preview) -> DebugChoice {
        match (preview.frame_view(), preview.debug_view()) {
            (Some(view), _) => DebugChoice::Frame(view),
            (None, 0) => DebugChoice::Shaded,
            (None, id) => DebugChoice::Surface(id),
        }
    }
}

/// The depth the camera's last geometry pass writes or tests against: a
/// prepass's depth, which the forward pass loads, or the G-buffer's.
pub(crate) fn scene_depth(graph: &RenderGraph) -> Option<ResourceId> {
    let order = graph.schedule().ok()?.order().to_vec();
    order.into_iter().rev().find_map(|index| {
        let pass = &graph.passes()[index];
        match (&pass.kind, pass.view, pass.depth) {
            (PassKind::Geometry { .. }, PassView::Camera, Some(depth))
                if Some(depth.resource) != graph.shadow_maps() =>
            {
                Some(depth.resource)
            }
            _ => None,
        }
    })
}

/// The size the frame views map the target to, until the preview sets its
/// own.
const TARGET: Value = Value::Vec2([512.0, 512.0]);

/// The light grid's count per cluster.
const LIGHT_COUNT: Effect = Effect {
    id: "editor.view_lights",
    label: "light count",
    description: "Show how many local lights each cluster lists (debug view).",
    kind: EffectKind::Screen {
        vertex_entry: "view_lights_vs",
        fragment_entry: "view_lights_fs",
    },
    inputs: &[EffectInput {
        name: "depth",
        // `Image`, as for `inspect_depth`: the binding follows the format.
        kind: EffectInputKind::Image,
        description: "The scene's depth.",
        history: 0,
    }],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "view_size",
        default: TARGET,
    }],
    shader: EffectShader::Source {
        path: "package::editor::view_lights",
        wxsl: include_str!("../shaders/view_lights.wxsl"),
    },
    downlevel: wxsl_render::effect::Downlevel::Portable,
};

/// The shadow cascades.
const CASCADES: Effect = Effect {
    id: "editor.view_cascades",
    label: "cascades",
    description: "Show which shadow cascade covers each surface (debug view).",
    kind: EffectKind::Screen {
        vertex_entry: "view_cascades_vs",
        fragment_entry: "view_cascades_fs",
    },
    inputs: &[EffectInput {
        name: "depth",
        kind: EffectInputKind::Image,
        description: "The scene's depth.",
        history: 0,
    }],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "view_size",
        default: TARGET,
    }],
    shader: EffectShader::Source {
        path: "package::editor::view_cascades",
        wxsl: include_str!("../shaders/view_cascades.wxsl"),
    },
    downlevel: wxsl_render::effect::Downlevel::Portable,
};

/// The shadow atlas, its depth shown linearly.
const ATLAS: Effect = Effect {
    id: "editor.view_atlas",
    label: "shadow atlas",
    description: "Show the shadow atlas, its depth linear (debug view).",
    kind: EffectKind::Screen {
        vertex_entry: "view_atlas_vs",
        fragment_entry: "view_atlas_fs",
    },
    inputs: &[EffectInput {
        name: "atlas",
        kind: EffectInputKind::Image,
        description: "The shadow atlas.",
        history: 0,
    }],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "view_size",
        default: TARGET,
    }],
    shader: EffectShader::Source {
        path: "package::editor::view_atlas",
        wxsl: include_str!("../shaders/view_atlas.wxsl"),
    },
    downlevel: wxsl_render::effect::Downlevel::Portable,
};

/// Every effect the views and pass stepping register.
pub(crate) const EFFECTS: [&Effect; 5] = [
    &stepping::INSPECT,
    &stepping::INSPECT_DEPTH,
    &LIGHT_COUNT,
    &CASCADES,
    &ATLAS,
];

#[cfg(test)]
mod tests {
    use super::*;
    use wxsl_render::pipeline::PipelineConfig;
    use wxsl_render::{StockPipeline, TargetConfig};

    #[test]
    fn both_stock_pipelines_have_a_scene_depth_and_an_atlas() {
        let config = PipelineConfig::new(TargetConfig::new(
            64,
            64,
            wxsl_render::wgpu::TextureFormat::Rgba8Unorm,
        ));
        for stock in StockPipeline::ALL {
            let graph: RenderGraph = stock.graph(&config).into();
            let depth = scene_depth(&graph).expect("a camera depth");
            assert_ne!(Some(depth), graph.shadow_maps(), "{stock}");
            let label = &graph.resources()[depth.index()].label;
            assert!(label.contains("depth"), "{stock}: {label}");
            for view in FrameView::ALL {
                // Overdraw counts forward fragments; deferred shades none.
                let expected = view != FrameView::Overdraw || *stock == StockPipeline::Forward;
                assert_eq!(
                    view.available(&graph, true),
                    expected,
                    "{stock}: {}",
                    view.label()
                );
                if let Some(effect) = view.effect() {
                    assert!(stepping::is_editor_effect(effect.id));
                }
                assert!(view.install(&graph).is_ok(), "{stock}: {}", view.label());
            }
        }
    }
}
