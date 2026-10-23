//! The sun's disc on the sky.
//!
//! The environment maps hold the sky's scattered light and no sun: a disc
//! a few texels across would alias in them and light everything twice,
//! over the directional light the sun already is. [`append`] draws it
//! where the frame shows sky instead, after the background; the host sets
//! where it is and how bright every frame, through [`SUN`]'s `direction`
//! and `radiance` parameters on the pass it labelled.

use super::{
    Downlevel, Effect, EffectInput, EffectInputKind, EffectKind, EffectParameter, EffectShader,
};
use crate::graph::FrameGraph;
use crate::pass::{Attachment, PassDesc, PassState, Read, ResourceId, ADDITIVE};
use wxsl_core::node::Value;

/// The Sun's angular radius as seen from the Earth, radians.
pub const SUN_RADIUS: f32 = 0.004_65;

/// The sun's disc where the depth is the far plane: `direction` towards
/// it, its `radiance` (linear rgb, 0 for none), `radius` its angular
/// radius.
pub const SUN: Effect = Effect {
    id: "wxsl.sun_disc",
    label: "sun disc",
    description: "Draw the sun's disc, limb-darkened, where the frame shows sky.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[EffectInput {
        name: "depth",
        kind: EffectInputKind::DepthImage,
        description: "The opaque depth: the sky is where it is the far plane.",
        history: 0,
    }],
    outputs: &[],
    parameters: &[
        EffectParameter {
            name: "direction",
            default: Value::Vec4([0.0, 1.0, 0.0, 0.0]),
        },
        EffectParameter {
            name: "radiance",
            default: Value::Vec4([0.0; 4]),
        },
        EffectParameter {
            name: "radius",
            default: Value::F32(SUN_RADIUS),
        },
    ],
    shader: EffectShader::Source {
        path: "package::wxsl::sun_disc",
        wxsl: include_str!("../../shaders/sun_disc.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// The sun's disc, `radius` radians, added onto `color` where `depth` is
/// the far plane, under the pass labelled `name`. Its `radiance` starts
/// at none.
pub fn append(
    graph: &mut FrameGraph,
    color: ResourceId,
    depth: ResourceId,
    radius: f32,
    name: &str,
) {
    graph.pass(
        PassDesc::screen(name, SUN.id)
            .with_color(Attachment::load(color))
            .with_state(PassState::FULLSCREEN.with_blend(ADDITIVE))
            .with_reads([Read::current(depth)])
            .with_parameter("radius", Value::F32(radius)),
    );
}
