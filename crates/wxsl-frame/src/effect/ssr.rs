//! Screen-space reflections over the opaque frame, from the G-buffer's
//! normal, roughness, base colour and metalness.
//!
//! A forward frame has no G-buffer: [`gbuffer`] turns its depth prepass
//! into a G-buffer pass on the same depth, which the shading pass still
//! tests `LessEqual` against; a deferred frame's is read as it is.
//! [`append`] then reflects the frame into a new target, which goes before
//! whatever fogs or refracts the opaque frame, and [`append_glass`] the
//! nearest glass `refraction::append_optics` composed over it.

use super::refraction::Optics;
use super::{
    Downlevel, Effect, EffectInput, EffectInputKind, EffectKind, EffectParameter, EffectShader,
};
use crate::graph::FrameGraph;
use crate::pass::{Attachment, PassDesc, PassKind, Read, ResourceDesc, ResourceId};
use crate::pipeline::gbuffer_format;
use crate::types::{Color, TextureFormat};
use wxsl_core::abi::{GBufferTarget, MaterialStage};
use wxsl_core::node::Value;

/// The reflections: the frame, its depth, and the G-buffer's normal and
/// base colour targets. `strength` scales them, surfaces rougher than
/// `max_roughness` reflect nothing, and rays fade out by `distance`
/// metres; `steps`, `stride` and `growth` are the march's [`Quality`].
pub const SSR: Effect = Effect {
    id: "wxsl.ssr",
    label: "reflections",
    description: "Screen-space reflections over the opaque frame, in place of the environment's where a ray hits.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[
        EffectInput {
            name: "color",
            kind: EffectInputKind::Image,
            description: "The opaque frame, as linear radiance.",
            history: 0,
        },
        EffectInput {
            name: "depth",
            kind: EffectInputKind::DepthImage,
            description: "The opaque depth.",
            history: 0,
        },
        EffectInput {
            name: "normal",
            kind: EffectInputKind::Image,
            description: "The G-buffer's normal target: octahedral normal, roughness.",
            history: 0,
        },
        EffectInput {
            name: "base_color",
            kind: EffectInputKind::Image,
            description: "The G-buffer's base colour target: base colour, metalness.",
            history: 0,
        },
    ],
    outputs: &[],
    parameters: &[
        EffectParameter {
            name: "strength",
            default: Value::F32(1.0),
        },
        EffectParameter {
            name: "max_roughness",
            default: Value::F32(0.5),
        },
        EffectParameter {
            name: "distance",
            default: Value::F32(80.0),
        },
        // The march, [`Quality::Medium`]'s until set.
        EffectParameter {
            name: "steps",
            default: Value::F32(64.0),
        },
        EffectParameter {
            name: "stride",
            default: Value::F32(2.5),
        },
        EffectParameter {
            name: "growth",
            default: Value::F32(1.04),
        },
    ],
    shader: EffectShader::Source {
        path: "package::wxsl::ssr",
        wxsl: include_str!("../../shaders/ssr.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// The reflections on glass: the composed frame, the opaque frame and its
/// depth the rays are marched against, the radiance cube the composition
/// reflected, and the refraction's peeled normal and optical layers, of
/// which the nearest reflects. `strength`, `distance` and the march are
/// the opaque pass's.
pub const GLASS: Effect = Effect {
    id: "wxsl.ssr_glass",
    label: "glass reflections",
    description: "Screen-space reflections on the nearest glass, in place of the radiance cube's where a ray hits.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[
        EffectInput {
            name: "color",
            kind: EffectInputKind::Image,
            description: "The frame with its glass composed, as linear radiance.",
            history: 0,
        },
        EffectInput {
            name: "opaque",
            kind: EffectInputKind::Image,
            description: "The opaque frame the glass was composed over.",
            history: 0,
        },
        EffectInput {
            name: "depth",
            kind: EffectInputKind::DepthImage,
            description: "The opaque depth.",
            history: 0,
        },
        EffectInput {
            name: "environment",
            kind: EffectInputKind::Image,
            description: "The radiance cube the composition reflected.",
            history: 0,
        },
        EffectInput {
            name: "normal_layers",
            kind: EffectInputKind::Image,
            description: "The refraction's peeled normals, depth in w.",
            history: 0,
        },
        EffectInput {
            name: "optical_layers",
            kind: EffectInputKind::Image,
            description: "The refraction's peeled optical materials.",
            history: 0,
        },
    ],
    outputs: &[],
    parameters: &[
        EffectParameter {
            name: "strength",
            default: Value::F32(1.0),
        },
        EffectParameter {
            name: "distance",
            default: Value::F32(80.0),
        },
        // The march, [`Quality::Medium`]'s until set.
        EffectParameter {
            name: "steps",
            default: Value::F32(64.0),
        },
        EffectParameter {
            name: "stride",
            default: Value::F32(2.5),
        },
        EffectParameter {
            name: "growth",
            default: Value::F32(1.04),
        },
    ],
    shader: EffectShader::Source {
        path: "package::wxsl::ssr_glass",
        wxsl: include_str!("../../shaders/ssr_glass.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// How finely the rays are marched: the more steps, the shorter, the
/// thinner what they can hit, and the costlier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quality {
    /// 32 steps from 6 pixels: what is a few pixels wide may be missed.
    Coarse,
    /// 64 steps from 2.5 pixels.
    #[default]
    Medium,
    /// 160 steps from one pixel: thin things (a wiper, a railing) reflect
    /// whole.
    Fine,
}

impl Quality {
    /// The passes' `steps`, `stride` (the first step, pixels) and
    /// `growth` (each next step's factor) for this quality. Each preset
    /// covers about as many pixels in all, 650 to 800.
    pub fn parameters(self) -> [(&'static str, Value); 3] {
        let (steps, stride, growth) = match self {
            Quality::Coarse => (32.0, 6.0, 1.08),
            Quality::Medium => (64.0, 2.5, 1.04),
            Quality::Fine => (160.0, 1.0, 1.015),
        };
        [
            ("steps", Value::F32(steps)),
            ("stride", Value::F32(stride)),
            ("growth", Value::F32(growth)),
        ]
    }
}

/// The G-buffer's base colour and normal targets, in that order, for
/// `layout` (the config's plan): those a G-buffer pass already writes, or
/// those the depth prepass is turned into a G-buffer pass for, its
/// targets named after `name`.
pub fn gbuffer(
    graph: &mut FrameGraph,
    layout: Vec<GBufferTarget>,
    name: &str,
) -> Result<[ResourceId; 2], String> {
    let field = |name: &str| {
        layout
            .iter()
            .position(|target| target.field == name)
            .ok_or_else(|| format!("the G-buffer has no {name} target"))
    };
    let (base_color, normal) = (field("base_color")?, field("normal")?);
    let pass_of = |graph: &FrameGraph, wanted: MaterialStage| {
        graph.passes().iter().position(
            |pass| matches!(&pass.kind, PassKind::Geometry { stage, .. } if *stage == wanted),
        )
    };
    if let Some(index) = pass_of(graph, MaterialStage::GBUFFER) {
        let color = &graph.passes()[index].color;
        return Ok([color[base_color].resource, color[normal].resource]);
    }
    let prepass =
        pass_of(graph, MaterialStage::DEPTH_ONLY).ok_or("the frame has no depth prepass")?;
    let targets: Vec<ResourceId> = layout
        .iter()
        .map(|target| {
            graph.resource(ResourceDesc::color(
                format!("{name} gbuffer {}", target.field),
                gbuffer_format(target.precision),
            ))
        })
        .collect();
    let pass = graph.pass_mut(prepass).ok_or("the depth prepass")?;
    pass.color = targets
        .iter()
        .map(|target| Attachment::clear(*target, Color::TRANSPARENT))
        .collect();
    if let PassKind::Geometry { stage, .. } = &mut pass.kind {
        *stage = MaterialStage::GBUFFER;
    }
    graph.set_gbuffer_layout(layout);
    Ok([targets[base_color], targets[normal]])
}

/// Reflect `color` (the opaque frame over `depth`) by the G-buffer's
/// `[base_color, normal]` targets, from [`gbuffer`], marching at
/// `quality`, into a new target named `name`, which is returned.
pub fn append(
    graph: &mut FrameGraph,
    color: ResourceId,
    depth: ResourceId,
    [base_color, normal]: [ResourceId; 2],
    quality: Quality,
    name: &str,
) -> ResourceId {
    let reflected = graph.resource(ResourceDesc::color(name, TextureFormat::Rgba16Float));
    graph.pass(
        with_quality(PassDesc::screen(name, SSR.id), quality)
            .with_color(Attachment::clear(reflected, Color::TRANSPARENT))
            .with_reads([
                Read::current(color),
                Read::current(depth),
                Read::current(normal),
                Read::current(base_color),
            ]),
    );
    reflected
}

/// Reflect the nearest glass of `optics`, composed by `refraction` over
/// `opaque` (its depth `depth`, its reflections from `environment`), by
/// marching its mirror rays over `opaque` at `quality`, into a new target
/// named `name`, which is returned.
pub fn append_glass(
    graph: &mut FrameGraph,
    optics: Optics,
    opaque: ResourceId,
    depth: ResourceId,
    environment: ResourceId,
    quality: Quality,
    name: &str,
) -> ResourceId {
    let reflected = graph.resource(ResourceDesc::color(name, TextureFormat::Rgba16Float));
    graph.pass(
        with_quality(PassDesc::screen(name, GLASS.id), quality)
            .with_color(Attachment::clear(reflected, Color::TRANSPARENT))
            .with_reads([
                Read::current(optics.color),
                Read::current(opaque),
                Read::current(depth),
                Read::current(environment),
                Read::current(optics.normals),
                Read::current(optics.optics),
            ]),
    );
    reflected
}

/// `pass` with `quality`'s march parameters.
fn with_quality(pass: PassDesc, quality: Quality) -> PassDesc {
    quality
        .parameters()
        .into_iter()
        .fold(pass, |pass, (name, value)| pass.with_parameter(name, value))
}
