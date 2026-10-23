//! Back-to-front optical transmission and opt-in precision tracing (ADR 0068).

use super::{
    Downlevel, Effect, EffectInput, EffectInputKind, EffectKind, EffectParameter, EffectShader,
};
use crate::graph::FrameGraph;
use crate::pass::{
    Attachment, DepthAttachment, Dimension, DrawSource, PassDesc, PassState, Read, ResourceDesc,
    ResourceId,
};
use crate::types::{Color, CompareFunction, TextureFormat};
use wxsl_core::node::Value;
use wxsl_core::{abi::MaterialStage, scene::TagExpr};

const FAR_DEPTH: Color = Color {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// Real-time composition or the retained, expensive layered ray marcher.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quality {
    /// Front/back thickness and back-to-front colour/depth composition.
    #[default]
    Layered,
    /// Search the peeled depth fields along each bent ray (ADR 0067).
    Precise,
}

impl std::str::FromStr for Quality {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "layered" => Ok(Self::Layered),
            "precise" => Ok(Self::Precise),
            _ => Err("refraction quality must be layered or precise"),
        }
    }
}

const fn image(name: &'static str) -> EffectInput {
    EffectInput {
        name,
        kind: EffectInputKind::Image,
        description: "Optical layer or linear radiance.",
        history: 0,
    }
}
const DEPTH: EffectInput = EffectInput {
    name: "depth",
    kind: EffectInputKind::DepthImage,
    description: "Opaque or peeled depth.",
    history: 0,
};

/// Advance only the near peel bound; retain the far plane.
pub const BOUNDS: Effect = Effect {
    id: "wxsl.refraction_bounds",
    label: "refraction bounds",
    description: "Advance the optical peel's near bound.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[DEPTH, image("previous")],
    outputs: &[],
    parameters: &[],
    shader: EffectShader::Source {
        path: "package::wxsl::refraction_bounds",
        wxsl: include_str!("../../shaders/refraction_bounds.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// Opt-in precision path. Never appended by the default recipe (ADR 0068).
pub const RESOLVE: Effect = Effect {
    id: "wxsl.refraction",
    label: "refraction",
    description: "Trace clear glass through up to eight optical interfaces.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[
        image("scene"),
        DEPTH,
        image("environment"),
        image("normal_layers"),
        image("optical_layers"),
    ],
    outputs: &[],
    parameters: &[],
    shader: EffectShader::Source {
        path: "package::wxsl::refraction",
        wxsl: include_str!("../../shaders/refraction.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// Seed the composed background's nearest depth from the opaque pass.
pub const SCENE_DEPTH: Effect = Effect {
    id: "wxsl.refraction_scene_depth",
    label: "optical scene depth",
    description: "Copy opaque depth for optical composition.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[DEPTH],
    outputs: &[],
    parameters: &[],
    shader: EffectShader::Source {
        path: "package::wxsl::refraction_scene_depth",
        wxsl: include_str!("../../shaders/refraction_scene_depth.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// One entry/exit pair over the already composed, farther scene.
pub const COMPOSE: Effect = Effect {
    id: "wxsl.refraction_layer",
    label: "optical layer",
    description: "Compose a front/back optical pair over the farther scene.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[
        image("background"),
        image("background_depth"),
        DEPTH,
        image("environment"),
        image("normal_layers"),
        image("optical_layers"),
        image("surface_layers"),
    ],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "layer",
        default: Value::U32(0),
    }],
    shader: EffectShader::Source {
        path: "package::wxsl::refraction_layer",
        wxsl: include_str!("../../shaders/refraction_layer.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// Nearest depth accompanying a composed front/back pair.
pub const COMPOSE_DEPTH: Effect = Effect {
    id: "wxsl.refraction_layer_depth",
    label: "optical layer depth",
    description: "Update nearest composed depth for the next optical pair.",
    kind: EffectKind::Screen {
        vertex_entry: "vertex",
        fragment_entry: "fragment",
    },
    inputs: &[
        image("background_depth"),
        DEPTH,
        image("normal_layers"),
        image("optical_layers"),
    ],
    outputs: &[],
    parameters: &[EffectParameter {
        name: "layer",
        default: Value::U32(0),
    }],
    shader: EffectShader::Source {
        path: "package::wxsl::refraction_layer_depth",
        wxsl: include_str!("../../shaders/refraction_layer_depth.wxsl"),
    },
    downlevel: Downlevel::Portable,
};

/// Append the baseline optical recipe. Inputs must be linear
/// colour, opaque depth and an unencoded radiance cube. Returns linear colour.
/// Selects `refractive`, independently of the alpha peel's per-object budget.
/// `surface_count` is the whole scene's crossing budget (clamped to 1..=4);
/// each crossing has an entry and exit. Two torus crossings use four interfaces.
/// Arrays keep the binding cost independent of this count.
pub fn append(
    graph: &mut FrameGraph,
    scene: ResourceId,
    depth: ResourceId,
    environment: ResourceId,
    name: &str,
    surface_count: u32,
) -> ResourceId {
    append_with_quality(
        graph,
        scene,
        depth,
        environment,
        name,
        surface_count,
        Quality::Layered,
    )
}

/// Same peel data, with an explicit precision/cost choice. `Precise` retains
/// the ray marcher; `Layered` composes front/back pairs back to front.
pub fn append_with_quality(
    graph: &mut FrameGraph,
    scene: ResourceId,
    depth: ResourceId,
    environment: ResourceId,
    name: &str,
    surface_count: u32,
    quality: Quality,
) -> ResourceId {
    append_optics(
        graph,
        scene,
        depth,
        environment,
        name,
        surface_count,
        quality,
    )
    .color
}

/// What [`append_optics`] leaves: the composed colour, and the peeled
/// layers it was composed from, nearest first, for a later pass over the
/// glass (the screen-space reflections' `ssr::append_glass`).
#[derive(Clone, Copy, Debug)]
pub struct Optics {
    /// Linear colour, the glass composed over the scene.
    pub color: ResourceId,
    /// Per layer: the surface normal, and its depth in w.
    pub normals: ResourceId,
    /// Per layer: the optical material the composition reads, its IOR in
    /// w (zero where there is no glass).
    pub optics: ResourceId,
}

/// [`append_with_quality`], with the peeled layers returned beside the
/// colour.
pub fn append_optics(
    graph: &mut FrameGraph,
    scene: ResourceId,
    depth: ResourceId,
    environment: ResourceId,
    name: &str,
    surface_count: u32,
    quality: Quality,
) -> Optics {
    let layers = surface_count.clamp(1, 4) * 2;
    let mut bounds = graph.resource(ResourceDesc::color(
        format!("{name} bounds 0"),
        TextureFormat::Rg32Float,
    ));
    graph.pass(
        PassDesc::geometry(
            format!("{name} init"),
            DrawSource::Scene(TagExpr::Never),
            MaterialStage::FORWARD_LIT,
        )
        .with_color(Attachment::clear(
            bounds,
            Color {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 0.0,
            },
        ))
        .with_state(PassState::FULLSCREEN),
    );
    let normals = graph.resource(
        ResourceDesc::color(format!("{name} normals"), TextureFormat::Rgba32Float)
            .with_dimension(Dimension::D2Array, layers),
    );
    let optics = graph.resource(
        ResourceDesc::color(format!("{name} optics"), TextureFormat::Rgba16Float)
            .with_dimension(Dimension::D2Array, layers),
    );
    // Base colour and transmission factor (ADR 0093). A third RGBA16F
    // target keeps the surface pass at 32 bytes per sample, WebGPU's
    // baseline.
    let surfaces = graph.resource(
        ResourceDesc::color(format!("{name} surfaces"), TextureFormat::Rgba16Float)
            .with_dimension(Dimension::D2Array, layers),
    );
    let reads = vec![
        Read::current(scene),
        Read::current(depth),
        Read::current(environment),
        Read::current(normals),
        Read::current(optics),
    ];
    for layer in 0..layers {
        let peeled = graph.resource(ResourceDesc::color(
            format!("{name} depth {layer}"),
            TextureFormat::Depth32Float,
        ));
        let scratch = graph.resource(ResourceDesc::color(
            format!("{name} pair {layer}"),
            TextureFormat::Rg32Float,
        ));
        let draws = || DrawSource::Scene(TagExpr::parse("refractive").expect("optical tag"));
        graph.pass(
            PassDesc::geometry(
                format!("{name} peel {layer}"),
                draws(),
                MaterialStage::REFRACTION_DEPTH,
            )
            .with_color(Attachment::clear(scratch, Color::TRANSPARENT))
            .with_depth(DepthAttachment::clear(peeled, 1.0))
            .with_state(
                PassState::OPAQUE
                    .with_cull_mode(None)
                    .with_depth_test(CompareFunction::Less, true),
            )
            .with_reads([Read::current(depth), Read::current(bounds)]),
        );
        graph.pass(
            PassDesc::geometry(
                format!("{name} surface {layer}"),
                draws(),
                MaterialStage::REFRACTION_SURFACE,
            )
            .with_color(
                Attachment::clear(
                    normals,
                    Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    },
                )
                .with_layer(layer),
            )
            .with_color(Attachment::clear(optics, Color::TRANSPARENT).with_layer(layer))
            .with_color(Attachment::clear(surfaces, Color::TRANSPARENT).with_layer(layer))
            .with_depth(DepthAttachment::load(peeled))
            .with_state(
                PassState::OPAQUE
                    .with_cull_mode(None)
                    .with_depth_test(CompareFunction::Equal, false),
            ),
        );
        if layer + 1 < layers {
            let next = graph.resource(ResourceDesc::color(
                format!("{name} bounds {}", layer + 1),
                TextureFormat::Rg32Float,
            ));
            graph.pass(
                PassDesc::screen(format!("{name} advance {layer}"), BOUNDS.id)
                    .with_color(Attachment::clear(next, Color::TRANSPARENT))
                    .with_reads([Read::current(peeled), Read::current(bounds)]),
            );
            bounds = next;
        }
    }
    if quality == Quality::Layered {
        let mut color = scene;
        let mut composed_depth = graph.resource(ResourceDesc::color(
            format!("{name} scene depth"),
            TextureFormat::R32Float,
        ));
        graph.pass(
            PassDesc::screen(format!("{name} scene depth"), SCENE_DEPTH.id)
                .with_color(Attachment::clear(composed_depth, FAR_DEPTH))
                .with_reads([Read::current(depth)]),
        );
        for pair in (0..layers / 2).rev() {
            let output = graph.resource(ResourceDesc::color(
                if pair == 0 {
                    format!("{name} color")
                } else {
                    format!("{name} color {pair}")
                },
                TextureFormat::Rgba16Float,
            ));
            let next_depth = graph.resource(ResourceDesc::color(
                format!("{name} composed depth {pair}"),
                TextureFormat::R32Float,
            ));
            graph.pass(
                PassDesc::screen(format!("{name} compose {pair}"), COMPOSE.id)
                    .with_color(Attachment::clear(output, Color::TRANSPARENT))
                    .with_parameter("layer", Value::U32(pair * 2))
                    .with_reads([
                        Read::current(color),
                        Read::current(composed_depth),
                        Read::current(depth),
                        Read::current(environment),
                        Read::current(normals),
                        Read::current(optics),
                        Read::current(surfaces),
                    ]),
            );
            graph.pass(
                PassDesc::screen(format!("{name} compose depth {pair}"), COMPOSE_DEPTH.id)
                    .with_color(Attachment::clear(next_depth, FAR_DEPTH))
                    .with_parameter("layer", Value::U32(pair * 2))
                    .with_reads([
                        Read::current(composed_depth),
                        Read::current(depth),
                        Read::current(normals),
                        Read::current(optics),
                    ]),
            );
            color = output;
            composed_depth = next_depth;
        }
        return Optics {
            color,
            normals,
            optics,
        };
    }
    let output = graph.resource(ResourceDesc::color(
        format!("{name} color"),
        TextureFormat::Rgba16Float,
    ));
    graph.pass(
        PassDesc::screen(format!("{name} resolve"), RESOLVE.id)
            .with_color(Attachment::clear(output, Color::TRANSPARENT))
            .with_reads(reads),
    );
    Optics {
        color: output,
        normals,
        optics,
    }
}
