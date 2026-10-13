//! Pass stepping (plan3 N9): the preview's pass list cut after any pass,
//! with one resource drawn into the preview by an editor-owned effect.
//!
//! The cut itself is [`RenderGraph::stepped`](wxsl_render::RenderGraph) —
//! device-free, in `wxsl-frame` — so stepping runs exactly the passes it
//! shows. What lives here is the editor's half: the two inspect effects
//! (colour and depth), which resources they can draw, and the rule that
//! keeps them out of the pipeline canvas's vocabulary.

use wxsl_core::node::Value;
use wxsl_render::effect::{
    Effect, EffectInput, EffectInputKind, EffectKind, EffectParameter, EffectShader,
};
use wxsl_render::pass::{Dimension, ResourceDesc, ResourceShape};
use wxsl_render::types::WgpuType;

/// The package the inspect effects register under: the editor's, never a
/// document's.
const PACKAGE: &str = "editor.";

/// The size the inspect effects scale the shown resource to, until the
/// preview sets its own.
const TARGET: Value = Value::Vec2([512.0, 512.0]);

/// Draw one colour resource, clamped and encoded.
pub(crate) const INSPECT: Effect = Effect {
    id: "editor.inspect",
    label: "inspect",
    description: "Show one colour resource as stored (pass stepping).",
    kind: EffectKind::Screen {
        vertex_entry: "inspect_vs",
        fragment_entry: "inspect_fs",
    },
    inputs: &[EffectInput {
        name: "image",
        kind: EffectInputKind::Image,
        description: "The resource to show.",
        history: 0,
    }],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "view_size",
        default: TARGET,
    }],
    shader: EffectShader::Source {
        path: "package::editor::inspect",
        wxsl: include_str!("../shaders/inspect.wxsl"),
    },
    downlevel: wxsl_render::effect::Downlevel::Portable,
};

/// Draw one depth resource, its near range spread.
pub(crate) const INSPECT_DEPTH: Effect = Effect {
    id: "editor.inspect_depth",
    label: "inspect depth",
    description: "Show one depth resource (pass stepping).",
    kind: EffectKind::Screen {
        vertex_entry: "inspect_depth_vs",
        fragment_entry: "inspect_depth_fs",
    },
    inputs: &[EffectInput {
        name: "image",
        // `Image`, not `DepthImage`: the binding comes from the resource's
        // format either way, and a depth input would earn the effect a
        // derived document row the editor's own effects must not have.
        kind: EffectInputKind::Image,
        description: "The depth resource to show.",
        history: 0,
    }],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "view_size",
        default: TARGET,
    }],
    shader: EffectShader::Source {
        path: "package::editor::inspect_depth",
        wxsl: include_str!("../shaders/inspect_depth.wxsl"),
    },
    downlevel: wxsl_render::effect::Downlevel::Portable,
};

/// Which inspect effect can draw `desc`, if either can: a 2D texture whose
/// texels read as floats or as depth. Integer targets (an id channel),
/// arrays, cubes and buffers are listed but not drawable.
pub(crate) fn effect_for(desc: &ResourceDesc) -> Option<&'static str> {
    let ResourceShape::Texture {
        dimension: Dimension::D2,
        format,
        ..
    } = desc.shape
    else {
        return None;
    };
    match format.to_wgpu().sample_type(None, None)? {
        wgpu::TextureSampleType::Float { .. } => Some(INSPECT.id),
        wgpu::TextureSampleType::Depth => Some(INSPECT_DEPTH.id),
        _ => None,
    }
}

/// Whether `id` is one of the editor's own effects, which a pipeline
/// document cannot name.
pub(crate) fn is_editor_effect(id: &str) -> bool {
    id.starts_with(PACKAGE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxsl_render::pass::ResourceDesc;
    use wxsl_render::types::TextureFormat;

    #[test]
    fn colour_and_depth_resources_pick_their_effect_and_integers_none() {
        let colour = ResourceDesc::color("hdr", TextureFormat::Rgba16Float);
        assert_eq!(effect_for(&colour), Some(INSPECT.id));
        let depth = ResourceDesc::color("depth", TextureFormat::Depth32Float);
        assert_eq!(effect_for(&depth), Some(INSPECT_DEPTH.id));
        let id = ResourceDesc::color("model id", TextureFormat::R8Uint);
        assert_eq!(effect_for(&id), None);
        assert_eq!(effect_for(&ResourceDesc::buffer("ramp", 64)), None);
        assert!(is_editor_effect(INSPECT.id) && !is_editor_effect("wxsl.bloom"));
    }
}
