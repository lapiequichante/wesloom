//! What a frame is made of: [`TargetConfig`], the stock pipelines, and
//! the `wgpu` pipeline cache they draw with.
//!
//! Until M1 this module held two hand-written structs, `ForwardPipeline` and
//! `DeferredPipeline`, each owning its attachments, its depth texture and a
//! pipeline cache, each writing `begin_render_pass` out by hand. A fourth
//! pass meant a fourth struct repeating all of it. Then a pipeline became a
//! [`crate::graph::RenderGraph`] — a list of [`crate::pass::PassDesc`]s
//! ([ADR 0021](../../../docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md)) —
//! and since the pipelines became *documents* (plan2 P3,
//! [ADR 0033](../../docs/adr/0033-pipelines-are-documents.md)) the shipped
//! pass lists come from the preset files under `assets/presets/`, compiled
//! by [`crate::pipeline_doc`]. [`forward_graph`] and [`deferred_graph`]
//! stay as the hand-built references those presets are tested against.
//!
//! `wxsl_render::PipelineCache` is what survives from the oldest shape, generalized: a
//! `wgpu` pipeline is keyed on the shader variant *and* the pass state and
//! target formats it was built for, so the same material draws opaque in
//! one pass, blended in another and front-face-culled into a shadow map,
//! without any of those being baked into one hardcoded descriptor.

use core::fmt;

use wxsl_core::abi::{self, GBufferPrecision, GBufferTarget, MaterialStage};
use wxsl_core::graph::Graph;
use wxsl_core::lighting::{ChannelRequest, GBufferPlan, LightingError, LightingSet};
use wxsl_core::macros::MacroSet;
use wxsl_core::scene::TagExpr;

use crate::graph::RenderGraph;
use crate::pass::{
    Attachment, DepthAttachment, Dimension, DrawSource, Extent, PassDesc, PassState, PassView,
    Read, ResourceDesc, ResourceId, DEPTH_FORMAT,
};
use crate::types::{Color, CompareFunction, TextureFormat, TextureUsages};

/// One of the two pass lists this crate ships.
///
/// What is left of `RenderPath` once material stages exist. That enum did
/// two jobs — "which pass list" and "which shader variant" — and they came
/// apart the moment a pass list had two geometry passes wanting different
/// variants. Which pass list is this; which variant is
/// [`abi::MaterialStage`]
/// ([ADR 0022](../../../docs/adr/0022-material-stages-replace-the-render-path-enum.md)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum StockPipeline {
    /// Depth prepass, then shade where the surface is evaluated.
    #[default]
    Forward,
    /// Write the surface to a G-buffer, then light it in a fullscreen pass.
    Deferred,
}

impl StockPipeline {
    /// Both, in declaration order.
    pub const ALL: &'static [StockPipeline] = &[StockPipeline::Forward, StockPipeline::Deferred];

    /// The preset document each stock pipeline is, embedded at compile
    /// time from `assets/presets/` — the shipped truth, readable and
    /// diffable as the data it is
    /// ([ADR 0033](../../docs/adr/0033-pipelines-are-documents.md)).
    const PRESET_SOURCE: &'static [&'static str] = &[
        include_str!("../assets/presets/forward.pipeline.json"),
        include_str!("../assets/presets/deferred.pipeline.json"),
    ];

    /// The name, as used in labels and on the command line.
    pub fn name(&self) -> &'static str {
        match self {
            StockPipeline::Forward => "forward",
            StockPipeline::Deferred => "deferred",
        }
    }

    /// Parse one from its [`StockPipeline::name`].
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        StockPipeline::ALL
            .iter()
            .copied()
            .find(|pipeline| pipeline.name().eq_ignore_ascii_case(text))
    }

    /// Whether this pipeline shades in a later pass.
    pub fn is_deferred(&self) -> bool {
        matches!(self, StockPipeline::Deferred)
    }

    /// This pipeline's document, parsed from the shipped preset file.
    ///
    /// A parse failure is a broken embedded asset — a programming error,
    /// not a runtime condition, so it panics with the file's name in the
    /// message.
    pub fn document(&self) -> Graph {
        let source = StockPipeline::PRESET_SOURCE[self.index()];
        serde_json::from_str(source)
            .unwrap_or_else(|error| panic!("the shipped `{self}` preset does not parse: {error}"))
    }

    /// This pipeline's pass list, under `config`: the preset document,
    /// compiled.
    ///
    /// A compile failure would mean a shipped preset that does not
    /// schedule — the parity tests in `pipeline_doc` and
    /// `stock_pipeline_names_round_trip` exist so that this `expect` is
    /// never the first place a mistake shows up.
    pub fn graph(&self, config: &PipelineConfig) -> RenderGraph {
        let document = self.document();
        crate::pipeline_doc::compile(
            &document,
            &wxsl_core::pipeline::registry(),
            &crate::effect::EffectRegistry::shipped(),
            config,
        )
        .unwrap_or_else(|error| panic!("the shipped `{self}` preset does not compile: {error}"))
    }

    /// Position in [`StockPipeline::ALL`], indexing [`Self::PRESET_SOURCE`].
    fn index(&self) -> usize {
        match self {
            StockPipeline::Forward => 0,
            StockPipeline::Deferred => 1,
        }
    }
}

/// Every knob a stock pipeline varies by, in one value.
///
/// Before this existed, each knob was threaded by hand through
/// `StockPipeline::graph`, `Renderer::new`, `rebuild`, `set_pipeline`,
/// `set_lighting`, the variant cache keys and the tests — seven sites per
/// concept, which is what made adding one a milestone-shaped event. Now a
/// knob is a field here, the pass list reads it, and everything
/// downstream derives from the same struct. `deferred_graph` and
/// `forward_graph` stay as free functions taking what they take: they are
/// the *reference* pass lists, and the preset-parity tests want them
/// callable without a config (plan2 P2).
#[derive(Clone, Debug)]
pub struct PipelineConfig {
    /// Size, format and clear colour of the final target.
    pub target: TargetConfig,
    /// The lighting models enabled for the deferred path, which decide the
    /// G-buffer's shape.
    pub lighting: LightingSet,
    /// The material features enabled for the deferred path — the second
    /// source of G-buffer channels (plan2 P12). Each asks for a channel
    /// beside the lighting set's own; the plan is what the compiler
    /// declares its resources from and what every gbuffer-stage material
    /// is generated against.
    pub features: Vec<ChannelRequest>,
    /// Macros the document compiler reads. `wxsl_peel_layers` and
    /// `wxsl_peel_native` change a `pass.peel` expansion (ADR 0047). Empty
    /// is every pipeline that has no peel.
    pub macros: MacroSet,
}

impl PipelineConfig {
    /// A config for `target` with the default lighting set and no
    /// features — the shape every pipeline had before either existed.
    pub fn new(target: impl Into<TargetConfig>) -> Self {
        PipelineConfig {
            target: target.into(),
            lighting: LightingSet::default(),
            features: Vec::new(),
            macros: MacroSet::new(),
        }
    }

    /// The collected channel plan: the lighting set's requests plus the
    /// features'. Fails only when the two disagree about a field name —
    /// a collision the plan names by both sources.
    pub fn plan(&self) -> Result<GBufferPlan, LightingError> {
        self.lighting.plan(&self.features)
    }

    /// The plan's layout, for callers that have already validated —
    /// the compiler's happy path, and the tests'.
    pub fn gbuffer_layout(&self) -> Vec<GBufferTarget> {
        self.plan()
            .expect("a config's channels were validated when its features were set")
            .layout()
            .to_vec()
    }
}

impl fmt::Display for StockPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `pad` so `{:>8}` in a progress line actually aligns.
        f.pad(self.name())
    }
}

/// The `wgpu` format for a G-buffer target of the given precision.
///
/// Each row names the target's channel count too, and the channel count is
/// what the attachment budget cares about: every four-channel target here
/// costs 8 bytes per sample whatever its bit depth, while a scalar costs 1
/// and a pair 4 — which is why the lighting models' small requests declare
/// themselves small.
pub fn gbuffer_format(precision: GBufferPrecision) -> TextureFormat {
    match precision {
        // Base colour and metallic are both in 0..1, and this is the one
        // four-channel target where 8 bits is visually enough.
        GBufferPrecision::Normalized => TextureFormat::Rgba8Unorm,
        // Normals need a sign and emissive can exceed 1.
        GBufferPrecision::HighDynamicRange => TextureFormat::Rgba16Float,
        // The dispatch id: one normalized channel, one byte.
        GBufferPrecision::NormalizedScalar => TextureFormat::R8Unorm,
        // A pair of half floats: two signed quantities, half the cost.
        GBufferPrecision::HighDynamicRangePair => TextureFormat::Rg16Float,
    }
}

/// The most bytes per sample a pass's colour attachments may total.
///
/// WebGPU's floor for `maxColorAttachmentBytesPerSample`, so it is a
/// budget every device honours and therefore a budget a lighting-model set
/// can be checked against before a device is asked. The base targets
/// spend 24 of it — three vec4 targets at 8 bytes each, whatever their bit
/// depths — which is what a set's requests are measured against; a scalar
/// id channel and a pair-precision request are what let the shipped full
/// set fit the rest. See `Renderer::set_lighting`.
pub const MAX_GBUFFER_BYTES_PER_SAMPLE: u32 = 32;

/// The G-buffer's formats, in `@location` order, for `lighting`'s layout:
/// the ABI's base targets, then whatever the enabled set requests.
pub fn gbuffer_formats(lighting: &LightingSet) -> Vec<TextureFormat> {
    lighting
        .gbuffer_layout()
        .iter()
        .map(|target| gbuffer_format(target.precision))
        .collect()
}

/// What a G-buffer layout costs against the attachment budget, using
/// the same arithmetic the WebGPU spec does — including the alignment
/// round-up — so a layout that fits by this number fits on every device,
/// not just the ones whose drivers are forgiving.
pub fn gbuffer_layout_bytes_per_sample(layout: &[GBufferTarget]) -> u32 {
    let mut total: u32 = 0;
    for target in layout {
        let format = gbuffer_format(target.precision);
        // The spec's own table: a four-channel target costs 8 bytes per
        // sample whatever its bit depth ("despite being 4 bytes per pixel,
        // these are 8 bytes per pixel in the table", says wgpu), a pair
        // costs 4, a scalar 1. Alignment rounds up before each add.
        let (cost, alignment) = match format {
            TextureFormat::Rgba8Unorm => (8, 1),
            TextureFormat::Rgba16Float => (8, 2),
            TextureFormat::R8Unorm => (1, 1),
            TextureFormat::Rg16Float => (4, 2),
            other => unreachable!("the G-buffer maps only to byte-cost formats, not {other:?}"),
        };
        total = total.next_multiple_of(alignment);
        total += cost;
    }
    total
}

/// What `lighting`'s G-buffer costs against the attachment budget — the
/// layout cost of the set's own requests, features aside (plan2 P12).
pub fn gbuffer_bytes_per_sample(lighting: &LightingSet) -> u32 {
    gbuffer_layout_bytes_per_sample(&lighting.gbuffer_layout())
}

/// Size, format and clear colour of what is being rendered into.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TargetConfig {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Format of the final colour target.
    ///
    /// A non-`Srgb` format is expected: the `tonemap` effect every stock
    /// chain ends in encodes sRGB itself, so that the forward path and the
    /// deferred lighting pass produce identical values
    /// ([ADR 0039](../../docs/adr/0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md)).
    pub format: TextureFormat,
    /// Colour to clear to where nothing is drawn, as **linear radiance**.
    ///
    /// It is what the head of the chain clears to, so it goes through the
    /// display transform like everything else drawn — which is why the
    /// default below is a much smaller number than the near-black it
    /// produces on screen.
    pub clear_color: Color,
}

impl TargetConfig {
    /// A config for `width` x `height` in `format`, cleared to a near-black
    /// with a little blue in it.
    pub fn new(width: u32, height: u32, format: TextureFormat) -> Self {
        TargetConfig {
            width: width.max(1),
            height: height.max(1),
            format,
            clear_color: Color {
                r: 0.001,
                g: 0.001,
                b: 0.0015,
                a: 1.0,
            },
        }
    }
}

/// Everything the geometry passes draw, in one pass list.
///
/// Every stock pass draws `*` rather than `opaque`, because until M5 splits
/// transparency out there is one queue and filtering it would only mean an
/// untagged scene rendering as nothing.
fn everything() -> DrawSource {
    DrawSource::Scene(TagExpr::Always)
}

/// Declare the shadow maps and the passes that fill them.
///
/// One pass per light slot, always — not one per light that happens to be
/// casting this frame. A pass list is built once and scheduled against
/// every frame after it, and which lights cast shadows is the
/// *environment*'s business and changes whenever the application says so.
/// So the shape is fixed at [`abi::MAX_LIGHTS`] and a slot whose light
/// casts nothing is cleared and left alone, which reads as "fully lit" —
/// the same answer, for the cost of a clear.
///
/// Front faces are *not* culled, which is the usual trick for hiding
/// self-shadowing acne. It only works on closed geometry, and this
/// milestone exists for the two cases that are not closed: an alpha-tested
/// leaf and a displaced surface. The normal-offset bias in `shadow.wxsl`
/// is what handles the acne instead.
fn shadow_passes(graph: &mut RenderGraph) -> ResourceId {
    let maps = graph.declare_shadow_maps(
        ResourceDesc::color("shadow maps", DEPTH_FORMAT)
            .with_extent(Extent::Fixed {
                width: abi::SHADOW_MAP_RESOLUTION,
                height: abi::SHADOW_MAP_RESOLUTION,
            })
            .with_dimension(Dimension::D2Array, abi::MAX_LIGHTS as u32)
            // Both because nothing in the pass list reads this resource:
            // it is sampled through the frame group, so the graph infers
            // neither the usage nor the lifetime and is told both.
            .with_usage(TextureUsages::TEXTURE_BINDING)
            .persistent(0),
    );
    for light in 0..abi::MAX_LIGHTS as u32 {
        graph.pass(
            PassDesc::geometry(
                format!("shadow {light}"),
                everything(),
                MaterialStage::SHADOW,
            )
            .with_view(PassView::Light { index: light })
            .with_depth(DepthAttachment::clear(maps, 1.0).with_layer(light))
            .with_state(PassState::OPAQUE.with_cull_mode(None)),
        );
    }
    maps
}

/// The forward pipeline: a depth prepass, then shade what survived it.
///
/// Since the stock pipelines became preset documents
/// ([ADR 0033](../../docs/adr/0033-pipelines-are-documents.md)) this is no
/// longer what [`StockPipeline::graph`] runs — the shipped
/// `assets/presets/*.pipeline.json` are. It stays as the hand-built
/// *reference*: `pipeline_doc`'s parity tests compile the preset document
/// and assert the result is this graph, field for field, which is what
/// keeps "two implementations of forward" one pipeline.
///
/// Two passes rather than one, and the second is the reason M2 exists: the
/// prepass runs the same materials compiled for
/// [`MaterialStage::DEPTH_ONLY`], which has no fragment entry at all, and
/// the shading pass then tests `LessEqual` against the depth it left
/// without writing depth again. Every fragment that reaches the expensive
/// shader is one that will be visible.
///
/// `LessEqual` rather than `Equal`: WGSL makes no promise that two
/// pipelines running the same vertex code produce bit-identical clip
/// positions unless the builtin is marked `@invariant`, and `Equal` turns
/// a one-ulp difference into a hole in the surface. `LessEqual` costs
/// nothing and tolerates it.
pub fn forward_graph(target: TargetConfig) -> RenderGraph {
    let mut graph = RenderGraph::new(target.format);
    shadow_passes(&mut graph);
    let depth = graph.resource(ResourceDesc::color("forward depth", DEPTH_FORMAT));
    let scene = graph.resource(hdr_chain_target("scene"));
    graph.pass(
        PassDesc::geometry("depth prepass", everything(), MaterialStage::DEPTH_ONLY)
            .with_depth(DepthAttachment::clear(depth, 1.0)),
    );
    graph.pass(
        PassDesc::geometry("forward", everything(), MaterialStage::FORWARD_LIT)
            .with_color(Attachment::clear(scene, target.clear_color))
            .with_depth(DepthAttachment::load(depth))
            .with_state(PassState::OPAQUE.with_depth_test(CompareFunction::LessEqual, false)),
    );
    graph.pass(display_transform(scene, target));
    graph
}

/// The colour target the head of a stock chain shades into: full size,
/// half float, transient — linear radiance for the display transform to
/// read.
fn hdr_chain_target(name: &str) -> ResourceDesc {
    ResourceDesc::color(
        name,
        gbuffer_format(abi::GBufferPrecision::HighDynamicRange),
    )
    .with_extent(Extent::Viewport { scale: 1.0 })
}

/// The pass every stock pipeline ends in: the `tonemap` effect over what
/// the chain wrote, into the frame's own target
/// ([ADR 0039](../../docs/adr/0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md)).
fn display_transform(scene: ResourceId, target: TargetConfig) -> PassDesc {
    PassDesc::screen("tonemap", "wxsl.tonemap")
        .with_color(Attachment::clear(RenderGraph::TARGET, target.clear_color))
        .with_reads([Read::current(scene)])
}

/// The deferred pipeline: write the surface into a G-buffer, then shade it.
///
/// Like [`forward_graph`], the hand-built reference the preset document's
/// parity tests compile against — not what [`StockPipeline::graph`] runs
/// any more.
///
/// The G-buffer's depth is sampled by the lighting pass rather than attached
/// to it — a depth texture cannot be attached and sampled in the same pass —
/// which the graph expresses as a read, and therefore as the edge that
/// orders the two passes.
pub fn deferred_graph(target: TargetConfig, lighting: &LightingSet) -> RenderGraph {
    let layout = lighting.gbuffer_layout();
    let mut graph = RenderGraph::new(target.format)
        // The scheduler checks the material pass's attachment count against
        // this, not against the ABI's base table: a set that requests an id
        // channel or a model target writes them here.
        .with_gbuffer_layout(layout.clone());
    shadow_passes(&mut graph);
    let gbuffer: Vec<ResourceId> = layout
        .iter()
        .map(|entry| {
            graph.resource(ResourceDesc::color(
                format!("gbuffer {}", entry.field),
                gbuffer_format(entry.precision),
            ))
        })
        .collect();
    let depth = graph.resource(ResourceDesc::color("gbuffer depth", DEPTH_FORMAT));
    let scene = graph.resource(hdr_chain_target("scene"));

    graph.pass(
        PassDesc::geometry("deferred material", everything(), MaterialStage::GBUFFER)
            // Cleared to zero, which matters for the depth-based background
            // test in the lighting pass: that is what keeps the clear colour
            // visible where nothing was drawn.
            .with_colors(
                gbuffer
                    .iter()
                    .map(|id| Attachment::clear(*id, Color::TRANSPARENT)),
            )
            .with_depth(DepthAttachment::clear(depth, 1.0)),
    );
    graph.pass(
        PassDesc::screen("deferred lighting", "wxsl.deferred_lighting")
            .with_color(Attachment::clear(scene, target.clear_color))
            // Bindings in `abi::GBUFFER_BASE_TARGETS` order, with depth last —
            // the order the generated lighting pass declares them in.
            // Bindings in layout order, with depth last — the order the
            // generated pass declares them in.
            .with_reads(
                gbuffer
                    .iter()
                    .chain(core::iter::once(&depth))
                    .map(|id| Read::current(*id)),
            ),
    );
    graph.pass(display_transform(scene, target));
    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::PassKind;

    /// The tests run against the default — a set of one, the shape every
    /// pipeline had before lighting models existed — plus, where the
    /// dispatch is what is under test, the full shipped set.
    fn default_lighting() -> LightingSet {
        LightingSet::default()
    }

    /// The config every stock pass list is built under here.
    fn default_config(target: TargetConfig) -> PipelineConfig {
        PipelineConfig::new(target)
    }

    #[test]
    fn the_gbuffer_formats_come_from_the_abi_table() {
        let formats = gbuffer_formats(&default_lighting());
        assert_eq!(formats.len(), abi::GBUFFER_BASE_TARGETS.len());
        assert_eq!(formats[0], TextureFormat::Rgba8Unorm);
        assert_eq!(formats[1], TextureFormat::Rgba16Float);
    }

    #[test]
    fn target_config_never_has_a_zero_dimension() {
        // A minimized window reports zero, and a zero-sized texture is a
        // validation error rather than a no-op.
        let config = TargetConfig::new(0, 0, TextureFormat::Rgba8Unorm);
        assert_eq!((config.width, config.height), (1, 1));
    }

    fn config() -> TargetConfig {
        TargetConfig::new(64, 64, TextureFormat::Rgba8Unorm)
    }

    #[test]
    fn the_forward_pipeline_is_a_depth_prepass_and_a_shading_pass() {
        let graph = forward_graph(config());
        // A shadow pass per light slot comes first; the two that make this
        // pipeline what it is follow, and then the display transform every
        // stock chain ends in (ADR 0039).
        assert_eq!(graph.passes().len(), abi::MAX_LIGHTS + 3);

        let prepass = &graph.passes()[abi::MAX_LIGHTS];
        assert!(matches!(
            prepass.kind,
            PassKind::Geometry {
                stage: MaterialStage::DEPTH_ONLY,
                ..
            }
        ));
        // No colour at all, and — unless the material discards — no
        // fragment shader to run either: the whole point.
        assert!(prepass.color.is_empty());
        assert!(!MaterialStage::DEPTH_ONLY.needs_surface());
        assert!(prepass.state.depth_write);

        let shading = &graph.passes()[abi::MAX_LIGHTS + 1];
        assert!(matches!(
            shading.kind,
            PassKind::Geometry {
                stage: MaterialStage::FORWARD_LIT,
                ..
            }
        ));
        assert_eq!(shading.color.len(), 1);
        assert!(!shading.state.depth_write, "the prepass already wrote it");
        assert_eq!(shading.state.depth_compare, CompareFunction::LessEqual);

        // The shading pass writes linear radiance into the chain's own
        // target rather than the frame's; the tonemap pass is what the
        // frame's target sees.
        assert_ne!(shading.color[0].resource, RenderGraph::TARGET);
        let tonemap = &graph.passes()[abi::MAX_LIGHTS + 2];
        assert!(matches!(
            &tonemap.kind,
            PassKind::Screen { effect } if effect == "wxsl.tonemap"
        ));
        assert_eq!(tonemap.color[0].resource, RenderGraph::TARGET);
        assert_eq!(tonemap.reads.len(), 1, "the image the chain wrote");
        assert_eq!(tonemap.reads[0].resource, shading.color[0].resource);

        // Loading the depth the prepass wrote is what orders the first
        // two; reading what the second wrote is what orders the third.
        let schedule = graph.schedule().expect("the forward pass list schedules");
        assert_eq!(
            schedule.order(),
            (0..abi::MAX_LIGHTS + 3).collect::<Vec<_>>()
        );
        // The shadow maps, the depth buffer and the chain's colour target:
        // three textures, and the shadow maps are never handed to anything
        // else because the frame group holds a view of them all frame.
        assert_eq!(schedule.slots().len(), 3);
    }

    #[test]
    fn every_pipeline_fills_one_shadow_slice_per_light_from_that_lights_view() {
        for pipeline in StockPipeline::ALL {
            let graph = pipeline.graph(&default_config(config()));
            let maps = graph.shadow_maps().expect("shadow maps are declared");
            let desc = graph.resource_desc(maps).expect("declared resource");
            let crate::pass::ResourceShape::Texture {
                dimension,
                layers,
                format,
                usage,
                ..
            } = desc.shape
            else {
                panic!("shadow maps are a texture");
            };
            assert_eq!(dimension, Dimension::D2Array);
            assert_eq!(layers, abi::MAX_LIGHTS as u32);
            assert_eq!(format, DEPTH_FORMAT);
            // Nothing in the pass list reads it — the frame group does —
            // so both of these have to be spelled out.
            assert!(usage.contains(TextureUsages::TEXTURE_BINDING));
            assert_ne!(desc.persistence, crate::pass::Persistence::Transient);

            let shadow: Vec<&PassDesc> = graph
                .passes()
                .iter()
                .filter(|pass| {
                    matches!(
                        pass.kind,
                        PassKind::Geometry {
                            stage: MaterialStage::SHADOW,
                            ..
                        }
                    )
                })
                .collect();
            assert_eq!(shadow.len(), abi::MAX_LIGHTS, "{pipeline}");
            for (index, pass) in shadow.iter().enumerate() {
                let index = index as u32;
                assert_eq!(pass.view, PassView::Light { index });
                let depth = pass.depth.expect("a shadow pass writes depth");
                assert_eq!(depth.resource, maps);
                assert_eq!(depth.layer, index, "one slice per light");
                assert!(pass.color.is_empty());
                // Two-sided: the milestone's cases are an alpha-tested
                // leaf and a displaced surface, neither of them closed.
                assert_eq!(pass.state.cull_mode, None);
            }
        }
    }

    #[test]
    fn the_deferred_pipeline_is_a_geometry_pass_and_a_screen_pass() {
        let graph = deferred_graph(config(), &default_lighting());
        let material = abi::MAX_LIGHTS;
        let lighting = material + 1;
        let tonemap = lighting + 1;
        assert_eq!(graph.passes().len(), tonemap + 1);
        assert_eq!(
            graph.passes()[material].color.len(),
            abi::GBUFFER_BASE_TARGETS.len()
        );
        assert!(matches!(
            graph.passes()[material].kind,
            PassKind::Geometry {
                stage: MaterialStage::GBUFFER,
                ..
            }
        ));
        assert!(matches!(
            &graph.passes()[lighting].kind,
            PassKind::Screen { effect } if effect == "wxsl.deferred_lighting"
        ));
        // The lighting pass reads every G-buffer target plus depth, and
        // those reads are what order it after the material pass.
        assert_eq!(
            graph.passes()[lighting].reads.len(),
            abi::GBUFFER_BASE_TARGETS.len() + 1
        );
        // And the lighting pass shades into the chain, not onto the
        // frame's target — the display transform is the last word.
        assert_ne!(
            graph.passes()[lighting].color[0].resource,
            RenderGraph::TARGET
        );
        assert!(matches!(
            &graph.passes()[tonemap].kind,
            PassKind::Screen { effect } if effect == "wxsl.tonemap"
        ));
        assert_eq!(
            graph.passes()[tonemap].color[0].resource,
            RenderGraph::TARGET
        );
        let schedule = graph.schedule().expect("the deferred pass list schedules");
        assert_eq!(schedule.order(), (0..=tonemap).collect::<Vec<_>>());
    }

    #[test]
    fn both_stock_pipelines_schedule_at_any_size() {
        for pipeline in StockPipeline::ALL {
            for (width, height) in [(1, 1), (64, 64), (3840, 2160)] {
                let target = TargetConfig::new(width, height, TextureFormat::Rgba8Unorm);
                pipeline
                    .graph(&PipelineConfig {
                        lighting: default_lighting(),
                        features: Vec::new(),
                        target,
                        macros: wxsl_core::macros::MacroSet::new(),
                    })
                    .schedule()
                    .unwrap_or_else(|error| panic!("{pipeline} at {width}x{height}: {error}"));
            }
        }
    }

    /// The budget arithmetic, checked against the spec's own cost table —
    /// the one place a guess has already been wrong once.
    ///
    /// WebGPU's `maxColorAttachmentBytesPerSample` accumulates per
    /// attachment: round the running total up to the attachment's
    /// alignment, then add its cost. The costs are not the bit depths: a
    /// four-channel target costs 8 whatever its format, a pair 4, a scalar
    /// byte. `Rgba16Float` and `Rg16Float` align to 2; `Rgba8Unorm` and
    /// `R8Unorm` align to 1. Nothing here is guessed: each number is the
    /// spec table's, and the totals below pin the arithmetic.
    #[test]
    fn the_gbuffer_budget_follows_the_spec_cost_table() {
        fn model(name: &str) -> wxsl_core::lighting::LightingModel {
            *wxsl_core::lighting::DEFAULT_MODELS
                .iter()
                // Bare spellings resolve against the shipped package.
                .find(|model| model.name == wxsl_core::identity::resolve(name).as_ref())
                .expect("a shipped model")
        }
        fn cost(set: LightingSet) -> u32 {
            gbuffer_bytes_per_sample(&set)
        }

        // The base three targets: 8 + 8 + 8 = 24, and everything a set
        // requests is measured against exactly that remainder.
        assert_eq!(cost(LightingSet::default()), 24);
        assert_eq!(cost(LightingSet::single(model("pbr"))), 24);
        // + the scalar id (1, align 1) = 25; + the clearcoat pair (4,
        // align 2, so 25 rounds to 26 first) = 30. The round-up is the
        // part that was once guessed wrong: the naive sum says 29.
        assert_eq!(cost(wxsl_core::lighting::default_set().unwrap()), 30);
        assert_eq!(cost(LightingSet::single(model("iridescent"))), 32);
        assert_eq!(cost(LightingSet::single(model("sheen"))), 32);
        assert!(
            cost(LightingSet::new([model("pbr"), model("iridescent")]).unwrap())
                > MAX_GBUFFER_BYTES_PER_SAMPLE
        );
        assert_eq!(
            cost(LightingSet::new([model("lambert"), model("clearcoat")]).unwrap()),
            30
        );
        // A set with the id channel and nothing else: 25.
        assert_eq!(
            cost(LightingSet::new([model("pbr"), model("phong")]).unwrap()),
            25
        );
        // Every shipped set fits, with room to spare.
        assert!(cost(wxsl_core::lighting::default_set().unwrap()) <= MAX_GBUFFER_BYTES_PER_SAMPLE);
        assert_eq!(MAX_GBUFFER_BYTES_PER_SAMPLE, 32, "the spec's floor");

        // A feature channel joins the same budget (plan2 P12): base 24,
        // the subsurface pair rounds 24 up to its alignment of 2 — still
        // 24 — and adds 4. 28, with the id channel absent in a
        // single-model set.
        let single = LightingSet::single(model("pbr"));
        let features = wxsl_core::lighting::feature_requests(&["subsurface"]).unwrap();
        let plan = single.plan(&features).unwrap();
        assert_eq!(
            gbuffer_layout_bytes_per_sample(plan.layout()),
            28,
            "24 base + 4 for the pair-precision subsurface channel"
        );
    }

    #[test]
    fn stock_pipeline_names_round_trip() {
        for pipeline in StockPipeline::ALL {
            assert_eq!(StockPipeline::parse(pipeline.name()), Some(*pipeline));
        }
        assert_eq!(
            StockPipeline::parse("  Deferred "),
            Some(StockPipeline::Deferred)
        );
        assert_eq!(StockPipeline::parse("visibility"), None);
    }

    #[test]
    fn every_stock_pass_list_asks_for_a_stage_that_writes_what_it_attaches() {
        // The pairing the scheduler checks, checked here over the lists we
        // ship so a new stage cannot be wired into a pass that cannot hold
        // its output.
        for pipeline in StockPipeline::ALL {
            let graph = pipeline.graph(&default_config(config()));
            for pass in graph.passes() {
                if let PassKind::Geometry { stage, .. } = &pass.kind {
                    assert_eq!(
                        pass.color.len(),
                        stage.color_targets(),
                        "{pipeline}/{} attaches {} targets for {stage}",
                        pass.label,
                        pass.color.len()
                    );
                }
            }
        }
    }

    /// The preset files are the shipped truth, and the document builder in
    /// `pipeline_doc` is how they were written: the two must agree node
    /// for node, or a preset has drifted from the pipeline it claims to
    /// be. Regenerate the files with
    /// `cargo test -p wxsl-frame --lib preset_dump -- --ignored` — this
    /// test is what says the regeneration is honest.
    #[test]
    fn the_shipped_preset_files_are_the_stock_documents() {
        for pipeline in StockPipeline::ALL {
            let shipped = pipeline.document();
            let built = crate::pipeline_doc::stock_document(*pipeline);
            assert_eq!(
                shipped, built,
                "the shipped `{pipeline}` preset does not match the document it claims \
                 to be; regenerate it"
            );
        }
    }
}
