//! Original environment bake effects and a neutral face-pass recipe
//! (ADR 0062), and a local probe captured in the frame (ADR 0102).
//!
//! The prefilters are native-only under ADR 0075: they query
//! `textureNumLevels`, which GLSL ES 3.00 cannot say. The background runs
//! everywhere.
use super::{
    Downlevel, Effect, EffectInput, EffectInputKind, EffectKind, EffectParameter, EffectShader,
};
use crate::{
    graph::FrameGraph,
    pass::{
        Attachment, DepthAttachment, Dimension, DrawSource, Extent, PassDesc, PassState, PassView,
        Policy, Read, ResourceDesc, ResourceId, DEPTH_FORMAT,
    },
    types::{Color, TextureFormat},
};
use wxsl_core::abi::MaterialStage;
use wxsl_core::node::Value;
use wxsl_core::scene::TagExpr;

const FACE: EffectParameter = EffectParameter {
    name: "face",
    default: Value::U32(0),
};
/// Importance samples per texel, or 0 — a bake's — for the quadrature
/// below. A per-frame probe needs a few: one texel integrating the source
/// grid is thousands of dependent reads (ADR 0102).
const SAMPLES: EffectParameter = EffectParameter {
    name: "samples",
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
    downlevel: Downlevel::Portable,
};

/// Append a depth-aware background to an opaque linear-colour chain.
/// Composite transparent layers after this output, then tonemap once.
pub fn append_background(
    graph: &mut FrameGraph,
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
    downlevel: Downlevel::NativeOnly,
};

/// Cosine-weighted environment convolution; stores irradiance divided by PI.
pub const DIFFUSE: Effect = Effect {
    id: "wxsl.ibl_diffuse",
    label: "Diffuse environment",
    description: "Cosine convolution, normalized for Lambert albedo",
    kind: KIND,
    inputs: &[INPUT],
    outputs: &[],
    parameters: &[FACE, SAMPLES],
    shader: EffectShader::Source {
        path: "package::wxsl::ibl_diffuse",
        wxsl: concat!(
            include_str!("../../shaders/ibl_common.wxsl"),
            include_str!("../../shaders/ibl_diffuse.wxsl")
        ),
    },
    downlevel: Downlevel::NativeOnly,
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
        SAMPLES,
    ],
    shader: EffectShader::Source {
        path: "package::wxsl::ibl_specular",
        wxsl: concat!(
            include_str!("../../shaders/ibl_common.wxsl"),
            include_str!("../../shaders/ibl_specular.wxsl")
        ),
    },
    downlevel: Downlevel::NativeOnly,
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
    downlevel: Downlevel::NativeOnly,
};

/// One face of a local probe: the forward capture from the pass's probe
/// view flipped into the cube face, with the radiance cube behind it, in
/// the environment cube's units (ADR 0102).
pub const PROBE_FACE: Effect = Effect {
    id: "wxsl.probe_face",
    label: "Probe face",
    description: "A probe view's capture, flipped into its cube face over the environment",
    kind: KIND,
    inputs: &[
        EffectInput {
            name: "color",
            kind: EffectInputKind::Image,
            description: "The face's linear forward capture",
            history: 0,
        },
        EffectInput {
            name: "depth",
            kind: EffectInputKind::DepthImage,
            description: "The face's standard depth",
            history: 0,
        },
        EffectInput {
            name: "radiance",
            kind: EffectInputKind::Image,
            description: "The environment's radiance cube, behind what was drawn",
            history: 0,
        },
    ],
    outputs: &[],
    parameters: &[],
    shader: EffectShader::Source {
        path: "package::wxsl::probe_face",
        wxsl: include_str!("../../shaders/probe_face.wxsl"),
    },
    downlevel: Downlevel::Portable,
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
    graph: &mut FrameGraph,
    source: ResourceId,
    label: &str,
    cube_size: u32,
    diffuse_size: u32,
    specular_mips: u32,
) -> BakeOutputs {
    append_bake_inner(
        graph,
        Source::Equirect(source),
        label,
        Bake {
            cube_size,
            diffuse_size,
            specular_mips,
            filtered: false,
            policy: Policy::Once,
            samples: None,
        },
    )
}

/// Bake with a complete box-filtered source chain for GGX PDF-selected LOD.
/// Register [`RESAMPLE`] as well as the three convolution effects. Separate
/// staging cubes retain the scheduler's whole-resource hazard contract.
pub fn append_filtered_bake(
    graph: &mut FrameGraph,
    source: ResourceId,
    label: &str,
    cube_size: u32,
    diffuse_size: u32,
    specular_mips: u32,
) -> BakeOutputs {
    append_bake_inner(
        graph,
        Source::Equirect(source),
        label,
        Bake {
            cube_size,
            diffuse_size,
            specular_mips,
            filtered: true,
            policy: Policy::Once,
            samples: None,
        },
    )
}

/// Capture a local probe from `Environment::probe` (ADR 0102), every pass
/// on `policy`: six forward passes from its views drawing `draws`, each
/// composited by [`PROBE_FACE`] over `environment` (a radiance cube in the
/// frame environment's units) into its cube face, then filtered and
/// convolved as [`append_filtered_bake`] does, each texel from `samples`
/// importance samples rather than the bake's quadrature. Register
/// [`PROBE_FACE`] beside that bake's effects. Under a policy other than
/// `PerFrame` (`OnDemand` for a probe the application refreshes every few
/// frames), the faces are persistent too, as a skipping pass's targets
/// must be (ADR 0035).
/// A pass binds the outputs with `PassDesc::with_environment`, which needs
/// the specular cube to have the frame maps' mip count.
#[allow(clippy::too_many_arguments)]
pub fn append_probe(
    graph: &mut FrameGraph,
    draws: TagExpr,
    environment: ResourceId,
    label: &str,
    cube_size: u32,
    diffuse_size: u32,
    specular_mips: u32,
    samples: u32,
    policy: Policy,
) -> BakeOutputs {
    append_bake_inner(
        graph,
        Source::Probe { draws, environment },
        label,
        Bake {
            cube_size,
            diffuse_size,
            specular_mips,
            filtered: true,
            policy,
            samples: Some(samples),
        },
    )
}

/// What a bake's first level is made from.
enum Source {
    /// An equirectangular 2D linear-HDR resource.
    Equirect(ResourceId),
    /// The frame's local probe, drawing `draws` over `environment`.
    Probe {
        draws: TagExpr,
        environment: ResourceId,
    },
}

/// A bake's sizes, and how often it runs.
struct Bake {
    cube_size: u32,
    diffuse_size: u32,
    specular_mips: u32,
    filtered: bool,
    policy: Policy,
    /// Importance samples per texel, or the bake's quadrature.
    samples: Option<u32>,
}

fn append_bake_inner(
    graph: &mut FrameGraph,
    source: Source,
    label: &str,
    bake: Bake,
) -> BakeOutputs {
    let Bake {
        cube_size,
        diffuse_size,
        specular_mips,
        filtered,
        policy,
        samples,
    } = bake;
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
                            policy,
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
                    policy,
                ));
            }
        }
    }
    let first = stages.first().copied().unwrap_or(radiance);
    for face in 0..6 {
        match &source {
            Source::Equirect(source) => {
                graph.pass(face_pass(
                    format!("{label} radiance {face}"),
                    EQUIRECT_TO_CUBE.id,
                    *source,
                    first,
                    face,
                    0,
                    policy,
                ));
            }
            Source::Probe { draws, environment } => {
                probe_face(
                    graph,
                    draws,
                    *environment,
                    first,
                    label,
                    cube_size,
                    face,
                    policy,
                );
            }
        }
        let mut pass = face_pass(
            format!("{label} diffuse {face}"),
            DIFFUSE.id,
            radiance,
            diffuse,
            face,
            0,
            policy,
        );
        if let Some(samples) = samples {
            pass = pass.with_parameter("samples", Value::U32(samples));
        }
        graph.pass(pass);
        for mip in 0..specular_mips {
            let mut pass = face_pass(
                format!("{label} specular {face}/{mip}"),
                SPECULAR.id,
                radiance,
                specular,
                face,
                mip,
                policy,
            )
            .with_parameter(
                "roughness",
                Value::F32(mip as f32 / specular_mips.saturating_sub(1).max(1) as f32),
            );
            if let Some(samples) = samples {
                pass = pass.with_parameter("samples", Value::U32(samples));
            }
            graph.pass(pass);
        }
    }
    BakeOutputs {
        radiance,
        diffuse,
        specular,
    }
}

/// Face `face` of a local probe into layer `face` of `cube`: `draws`
/// forward-lit from the probe view, then composited over `environment`.
#[allow(clippy::too_many_arguments)]
fn probe_face(
    graph: &mut FrameGraph,
    draws: &TagExpr,
    environment: ResourceId,
    cube: ResourceId,
    label: &str,
    size: u32,
    face: u32,
    policy: Policy,
) {
    let view = PassView::Probe { face };
    let extent = Extent::Fixed {
        width: size,
        height: size,
    };
    let stable = |desc: ResourceDesc| {
        if policy == Policy::PerFrame {
            desc
        } else {
            desc.persistent(0)
        }
    };
    let color = graph.resource(stable(
        ResourceDesc::color(format!("{label} face {face}"), TextureFormat::Rgba16Float)
            .with_extent(extent),
    ));
    let depth = graph.resource(stable(
        ResourceDesc::color(format!("{label} face depth {face}"), DEPTH_FORMAT).with_extent(extent),
    ));
    graph.pass(
        PassDesc::geometry(
            format!("{label} face {face}"),
            DrawSource::Scene(draws.clone()),
            MaterialStage::FORWARD_LIT,
        )
        .with_view(view)
        .with_color(Attachment::clear(color, Color::BLACK))
        .with_depth(DepthAttachment::clear(depth, 1.0))
        .with_state(PassState::OPAQUE)
        .with_policy(policy),
    );
    graph.pass(
        PassDesc::screen(format!("{label} capture {face}"), PROBE_FACE.id)
            .with_view(view)
            .with_reads([
                Read::current(color),
                Read::current(depth),
                Read::current(environment),
            ])
            .with_color(Attachment::clear(cube, Color::BLACK).with_layer(face))
            .with_policy(policy),
    );
}

fn face_pass(
    label: String,
    effect: &str,
    source: ResourceId,
    target: ResourceId,
    face: u32,
    mip: u32,
    policy: Policy,
) -> PassDesc {
    PassDesc::screen(label, effect)
        .with_reads([Read::current(source)])
        .with_color(
            Attachment::clear(target, Color::BLACK)
                .with_layer(face)
                .with_mip(mip),
        )
        .with_parameter("face", Value::U32(face))
        .with_policy(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filtered_chain_and_frame_bindings_order_lighting_without_pass_bindings() {
        let mut graph = FrameGraph::new(TextureFormat::Rgba8Unorm);
        let hdr = graph.resource(ResourceDesc::imported("HDR", TextureFormat::Rgba32Float));
        graph.pass(
            PassDesc::screen("lighting", super::super::DEFERRED_LIGHTING.id)
                .with_color(Attachment::clear(FrameGraph::TARGET, Color::BLACK)),
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
        let mut graph = FrameGraph::new(TextureFormat::Rgba8Unorm);
        let source = graph.resource(ResourceDesc::imported("hdr", TextureFormat::Rgba16Float));
        let outputs = append_bake(&mut graph, source, "ibl", 16, 4, 5);
        graph.pass(
            PassDesc::screen("consume", "test.view")
                .with_reads([
                    Read::current(outputs.specular),
                    Read::current(outputs.diffuse),
                ])
                .with_color(Attachment::clear(FrameGraph::TARGET, Color::BLACK)),
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
    fn a_probe_is_captured_from_its_views_and_read_by_the_passes_that_ask() {
        let mut graph = FrameGraph::new(TextureFormat::Rgba8Unorm);
        let hdr = graph.resource(ResourceDesc::imported("HDR", TextureFormat::Rgba32Float));
        let sky = append_filtered_bake(&mut graph, hdr, "sky", 16, 4, 5);
        graph.declare_environment_maps(sky.diffuse, sky.specular);
        let draws = TagExpr::parse("!car").unwrap();
        let probe = append_probe(
            &mut graph,
            draws,
            sky.radiance,
            "probe",
            16,
            4,
            5,
            64,
            Policy::PerFrame,
        );
        let car = |environment: Option<[ResourceId; 2]>| {
            let mut pass = PassDesc::geometry(
                "car",
                DrawSource::Scene(TagExpr::parse("car").unwrap()),
                MaterialStage::FORWARD_LIT,
            )
            .with_color(Attachment::clear(FrameGraph::TARGET, Color::BLACK))
            .with_state(PassState::FULLSCREEN);
            pass.environment = environment;
            pass
        };
        graph.pass(car(Some([probe.diffuse, probe.specular])));
        let schedule = graph.schedule().unwrap();
        let position = |index: usize| schedule.order().iter().position(|i| *i == index).unwrap();
        let passes = graph.passes();
        let last = passes.len() - 1;
        for (index, pass) in passes.iter().enumerate() {
            if pass.label.starts_with("probe") {
                // Every probe pass runs each frame, before the car.
                assert_eq!(pass.policy, Policy::PerFrame, "{}", pass.label);
                assert!(position(index) < position(last), "{}", pass.label);
            }
            if let Some(face) = pass.label.strip_prefix("probe face ") {
                let face: u32 = face.parse().unwrap();
                assert_eq!(pass.view, PassView::Probe { face });
            }
            if pass.label.starts_with("probe specular") || pass.label.starts_with("probe diffuse") {
                assert_eq!(pass.parameters.get("samples"), Some(&Value::U32(64)));
            }
            if pass.label.starts_with("sky specular") || pass.label.starts_with("sky diffuse") {
                assert_eq!(pass.policy, Policy::Once);
                assert!(!pass.parameters.contains_key("samples"));
            }
        }
        // The probe's specular cube has to have the frame's levels.
        let mut graph_short = graph.clone();
        let short = append_probe(
            &mut graph_short,
            TagExpr::Never,
            sky.radiance,
            "short",
            16,
            4,
            3,
            64,
            Policy::PerFrame,
        );
        graph_short.pass(car(Some([short.diffuse, short.specular])));
        let error = graph_short.schedule().unwrap_err().to_string();
        assert!(error.contains("5 mip levels"), "{error}");
        // And it replaces the frame's maps, so those have to exist.
        let mut bare = FrameGraph::new(TextureFormat::Rgba8Unorm);
        let radiance = bare.resource(ResourceDesc::imported("sky", TextureFormat::Rgba16Float));
        let probe = append_probe(
            &mut bare,
            TagExpr::Never,
            radiance,
            "probe",
            16,
            4,
            5,
            64,
            Policy::PerFrame,
        );
        bare.pass(car(Some([probe.diffuse, probe.specular])));
        let error = bare.schedule().unwrap_err().to_string();
        assert!(error.contains("does not declare"), "{error}");
    }

    #[test]
    fn an_on_demand_probe_writes_only_stable_storage() {
        let mut graph = FrameGraph::new(TextureFormat::Rgba8Unorm);
        let hdr = graph.resource(ResourceDesc::imported("HDR", TextureFormat::Rgba32Float));
        let sky = append_filtered_bake(&mut graph, hdr, "sky", 16, 4, 5);
        graph.declare_environment_maps(sky.diffuse, sky.specular);
        let draws = TagExpr::parse("!car").unwrap();
        append_probe(
            &mut graph,
            draws,
            sky.radiance,
            "probe",
            16,
            4,
            5,
            64,
            Policy::OnDemand,
        );
        // The scheduler refuses a skipping pass that writes a transient.
        graph.schedule().unwrap();
        for pass in graph.passes() {
            if pass.label.starts_with("probe") {
                assert_eq!(pass.policy, Policy::OnDemand, "{}", pass.label);
            }
        }
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
