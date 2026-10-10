//! Original environment bake effects and a neutral face-pass recipe (ADR 0062).
use super::{Effect, EffectInput, EffectInputKind, EffectKind, EffectParameter, EffectShader};
use crate::{
    graph::RenderGraph,
    pass::{Attachment, Dimension, Extent, PassDesc, Policy, Read, ResourceDesc, ResourceId},
    types::{Color, TextureFormat},
};
use wxsl_core::node::Value;

const FACE: EffectParameter = EffectParameter {
    name: "face",
    default: Value::U32(0),
};
const INPUT: EffectInput = EffectInput {
    name: "image",
    kind: EffectInputKind::Image,
    description: "Linear radiance",
    history: 0,
};
const KIND: EffectKind = EffectKind::Screen {
    vertex_entry: "vertex",
    fragment_entry: "fragment",
};

/// Composite camera-oriented linear environment radiance where opaque depth is clear.
pub const BACKGROUND: Effect = Effect {
    id: "wxsl.environment_background",
    label: "Environment background",
    description: "Camera rays into radiance cube on untouched opaque depth, before tonemap",
    kind: KIND,
    inputs: &[
        EffectInput {
            name: "color",
            kind: EffectInputKind::Image,
            description: "Linear opaque colour",
            history: 0,
        },
        EffectInput {
            name: "depth",
            kind: EffectInputKind::DepthImage,
            description: "Opaque standard depth",
            history: 0,
        },
        EffectInput {
            name: "radiance",
            kind: EffectInputKind::Image,
            description: "Unconvolved radiance cube",
            history: 0,
        },
    ],
    outputs: &[],
    parameters: &[],
    shader: EffectShader::Source {
        path: "package::wxsl::environment_background",
        wxsl: include_str!("../../shaders/environment_background.wxsl"),
    },
};

/// Append a depth-aware background to an opaque linear-colour chain.
/// Composite transparent layers after this output, then tonemap once.
pub fn append_background(
    graph: &mut RenderGraph,
    color: ResourceId,
    depth: ResourceId,
    radiance: ResourceId,
    label: &str,
) -> ResourceId {
    let output = graph.resource(ResourceDesc::color(label, TextureFormat::Rgba16Float));
    graph.pass(
        PassDesc::screen(label, BACKGROUND.id)
            .with_reads([
                Read::current(color),
                Read::current(depth),
                Read::current(radiance),
            ])
            .with_color(Attachment::clear(output, Color::TRANSPARENT)),
    );
    output
}

/// Bilinear, longitude-wrapped equirectangular HDR to a selected cube face.
pub const EQUIRECT_TO_CUBE: Effect = Effect {
    id: "wxsl.equirect_to_cube",
    label: "Equirectangular to cube",
    description: "Convert linear HDR radiance to WebGPU cube faces",
    kind: KIND,
    inputs: &[INPUT],
    outputs: &[],
    parameters: &[FACE],
    shader: EffectShader::Source {
        path: "package::wxsl::equirect_to_cube",
        wxsl: concat!(
            include_str!("../../shaders/ibl_common.wxsl"),
            include_str!("../../shaders/equirect_to_cube.wxsl")
        ),
    },
};

/// Cosine-weighted environment convolution; stores irradiance divided by PI.
pub const DIFFUSE: Effect = Effect {
    id: "wxsl.ibl_diffuse",
    label: "Diffuse environment",
    description: "Cosine convolution, normalized for Lambert albedo",
    kind: KIND,
    inputs: &[INPUT],
    outputs: &[],
    parameters: &[FACE],
    shader: EffectShader::Source {
        path: "package::wxsl::ibl_diffuse",
        wxsl: concat!(
            include_str!("../../shaders/ibl_common.wxsl"),
            include_str!("../../shaders/ibl_diffuse.wxsl")
        ),
    },
};

/// GGX prefiltered radiance, the environment half of the split-sum approximation.
pub const SPECULAR: Effect = Effect {
    id: "wxsl.ibl_specular",
    label: "GGX environment",
    description: "Deterministic broad GGX lobes and importance-sampled sharp lobes",
    kind: KIND,
    inputs: &[INPUT],
    outputs: &[],
    parameters: &[
        FACE,
        EffectParameter {
            name: "roughness",
            default: Value::F32(0.0),
        },
    ],
    shader: EffectShader::Source {
        path: "package::wxsl::ibl_specular",
        wxsl: concat!(
            include_str!("../../shaders/ibl_common.wxsl"),
            include_str!("../../shaders/ibl_specular.wxsl")
        ),
    },
};

/// Copy a cube level or box-filter it into a half-size face.
pub const RESAMPLE: Effect = Effect {
    id: "wxsl.ibl_resample",
    label: "Environment mip filter",
    description: "Seam-aware cube sampling into a separate face attachment",
    kind: KIND,
    inputs: &[INPUT],
    outputs: &[],
    parameters: &[
        FACE,
        EffectParameter {
            name: "box_filter",
            default: Value::U32(0),
        },
    ],
    shader: EffectShader::Source {
        path: "package::wxsl::ibl_resample",
        wxsl: concat!(
            include_str!("../../shaders/ibl_common.wxsl"),
            include_str!("../../shaders/ibl_resample.wxsl")
        ),
    },
};

/// Stable outputs from [`append_bake`]. Reads of these resources order consumers
/// after every face writer; material bindings are not connected by this recipe.
#[derive(Clone, Copy, Debug)]
pub struct BakeOutputs {
    /// Radiance cube; a complete source mip chain with [`append_filtered_bake`].
    pub radiance: ResourceId,
    /// Lambert-ready irradiance / PI cube.
    pub diffuse: ResourceId,
    /// GGX radiance cube with roughness mip / (mip count - 1).
    pub specular: ResourceId,
}

/// Append a Once bake reading an equirectangular 2D linear-HDR resource.
/// Register the three effects before running the plan. Sizes/counts are
/// validated by the ordinary scheduler, without clamping invalid descriptors.
/// `label` must be unique within the graph; it namespaces resources and passes.
pub fn append_bake(
    graph: &mut RenderGraph,
    source: ResourceId,
    label: &str,
    cube_size: u32,
    diffuse_size: u32,
    specular_mips: u32,
) -> BakeOutputs {
    append_bake_inner(
        graph,
        source,
        label,
        cube_size,
        diffuse_size,
        specular_mips,
        false,
    )
}

/// Bake with a complete box-filtered source chain for GGX PDF-selected LOD.
/// Register [`RESAMPLE`] as well as the three convolution effects. Separate
/// staging cubes retain the scheduler's whole-resource hazard contract.
pub fn append_filtered_bake(
    graph: &mut RenderGraph,
    source: ResourceId,
    label: &str,
    cube_size: u32,
    diffuse_size: u32,
    specular_mips: u32,
) -> BakeOutputs {
    append_bake_inner(
        graph,
        source,
        label,
        cube_size,
        diffuse_size,
        specular_mips,
        true,
    )
}

fn append_bake_inner(
    graph: &mut RenderGraph,
    source: ResourceId,
    label: &str,
    cube_size: u32,
    diffuse_size: u32,
    specular_mips: u32,
    filtered: bool,
) -> BakeOutputs {
    let source_mips = if filtered {
        u32::BITS - cube_size.max(1).leading_zeros()
    } else {
        1
    };
    let mut cube = |name: &str, size, mips| {
        graph.resource(
            ResourceDesc::color(format!("{label} {name}"), TextureFormat::Rgba16Float)
                .with_extent(Extent::Fixed {
                    width: size,
                    height: size,
                })
                .with_dimension(Dimension::Cube, 6)
                .with_mip_levels(mips)
                .persistent(0),
        )
    };
    let radiance = cube("radiance", cube_size, source_mips);
    let diffuse = cube("diffuse", diffuse_size, 1);
    let specular = cube("specular", cube_size, specular_mips);
    let mut stages = Vec::new();
    if filtered {
        for mip in 0..source_mips {
            stages.push(cube(
                &format!("source level {mip}"),
                (cube_size >> mip).max(1),
                1,
            ));
        }
        for (mip, stage) in stages.iter().copied().enumerate() {
            for face in 0..6 {
                if mip > 0 {
                    graph.pass(
                        face_pass(
                            format!("{label} source {face}/{mip}"),
                            RESAMPLE.id,
                            stages[mip - 1],
                            stage,
                            face,
                            0,
                        )
                        .with_parameter("box_filter", Value::U32(1)),
                    );
                }
                graph.pass(face_pass(
                    format!("{label} radiance mip {face}/{mip}"),
                    RESAMPLE.id,
                    stage,
                    radiance,
                    face,
                    mip as u32,
                ));
            }
        }
    }
    for face in 0..6 {
        graph.pass(face_pass(
            format!("{label} radiance {face}"),
            EQUIRECT_TO_CUBE.id,
            source,
            stages.first().copied().unwrap_or(radiance),
            face,
            0,
        ));
        graph.pass(face_pass(
            format!("{label} diffuse {face}"),
            DIFFUSE.id,
            radiance,
            diffuse,
            face,
            0,
        ));
        for mip in 0..specular_mips {
            graph.pass(
                face_pass(
                    format!("{label} specular {face}/{mip}"),
                    SPECULAR.id,
                    radiance,
                    specular,
                    face,
                    mip,
                )
                .with_parameter(
                    "roughness",
                    Value::F32(mip as f32 / specular_mips.saturating_sub(1).max(1) as f32),
                ),
            );
        }
    }
    BakeOutputs {
        radiance,
        diffuse,
        specular,
    }
}

fn face_pass(
    label: String,
    effect: &str,
    source: ResourceId,
    target: ResourceId,
    face: u32,
    mip: u32,
) -> PassDesc {
    PassDesc::screen(label, effect)
        .with_reads([Read::current(source)])
        .with_color(
            Attachment::clear(target, Color::BLACK)
                .with_layer(face)
                .with_mip(mip),
        )
        .with_parameter("face", Value::U32(face))
        .with_policy(Policy::Once)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filtered_chain_and_frame_bindings_order_lighting_without_pass_bindings() {
        let mut graph = RenderGraph::new(TextureFormat::Rgba8Unorm);
        let hdr = graph.resource(ResourceDesc::imported("HDR", TextureFormat::Rgba32Float));
        graph.pass(
            PassDesc::screen("lighting", super::super::DEFERRED_LIGHTING.id)
                .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK)),
        );
        let maps = append_filtered_bake(&mut graph, hdr, "bake", 16, 4, 5);
        graph.declare_environment_maps(maps.diffuse, maps.specular);
        let schedule = graph.schedule().unwrap();
        assert!(
            graph.passes()[0].reads.is_empty(),
            "frame reads must not change pass bindings"
        );
        let lighting = schedule
            .order()
            .iter()
            .position(|index| *index == 0)
            .unwrap();
        for (position, index) in schedule.order().iter().enumerate() {
            if graph.passes()[*index]
                .written()
                .any(|id| id == maps.diffuse || id == maps.specular)
            {
                assert!(position < lighting);
            }
        }
        assert!(matches!(
            graph.resource_desc(maps.radiance).unwrap().shape,
            crate::pass::ResourceShape::Texture { mip_levels: 5, .. }
        ));
        graph.set_environment_scale(f32::INFINITY);
        assert!(graph
            .schedule()
            .unwrap_err()
            .to_string()
            .contains("environment scale"));
        graph.set_environment_scale(1.0);
        graph.declare_environment_maps(hdr, maps.specular);
        assert!(graph.schedule().unwrap_err().to_string().contains("HDR"));
    }
    #[test]
    fn recipe_orders_every_source_face_before_convolution() {
        let mut graph = RenderGraph::new(TextureFormat::Rgba8Unorm);
        let source = graph.resource(ResourceDesc::imported("hdr", TextureFormat::Rgba16Float));
        let outputs = append_bake(&mut graph, source, "ibl", 16, 4, 5);
        graph.pass(
            PassDesc::screen("consume", "test.view")
                .with_reads([
                    Read::current(outputs.specular),
                    Read::current(outputs.diffuse),
                ])
                .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK)),
        );
        let schedule = graph.schedule().unwrap();
        assert_eq!(graph.passes().len(), 43);
        let last_radiance = schedule
            .order()
            .iter()
            .enumerate()
            .filter(|(_, index)| {
                graph.passes()[**index]
                    .written()
                    .any(|id| id == outputs.radiance)
            })
            .map(|(position, _)| position)
            .max()
            .unwrap();
        for (position, index) in schedule.order().iter().enumerate() {
            if graph.passes()[*index]
                .reads
                .iter()
                .any(|read| read.resource == outputs.radiance)
            {
                assert!(position > last_radiance);
            }
        }
        // The scheduler's whole-resource hazard contract already pins ordering;
        // here every authored value must also pack against the shared descriptor.
        for pass in graph.passes().iter().take(42) {
            let crate::pass::PassKind::Screen { effect } = &pass.kind else {
                unreachable!()
            };
            let descriptor = [EQUIRECT_TO_CUBE, DIFFUSE, SPECULAR]
                .into_iter()
                .find(|e| e.id == effect)
                .unwrap();
            assert!(descriptor.initial_parameters(&pass.parameters).is_ok());
        }
        assert!(!schedule.slots().is_empty());
    }
    #[test]
    fn initial_parameters_reject_unknown_names_and_wrong_types() {
        assert!(DIFFUSE
            .initial_parameters(&[("face".into(), Value::F32(1.0))].into())
            .is_err());
        assert!(DIFFUSE
            .initial_parameters(&[("typo".into(), Value::U32(1))].into())
            .is_err());
        let bytes = SPECULAR
            .initial_parameters(&[("face".into(), Value::U32(5))].into())
            .unwrap();
        assert_eq!(
            SPECULAR.param_layout().read(&bytes, "face"),
            Some(Value::U32(5))
        );
        assert_eq!(
            SPECULAR.param_layout().read(&bytes, "roughness"),
            Some(Value::F32(0.0))
        );
    }
}
