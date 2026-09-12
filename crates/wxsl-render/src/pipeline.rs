//! What a frame is made of: [`TargetConfig`], the two stock pass lists, and
//! the `wgpu` pipeline cache they draw with.
//!
//! Until M1 this module held two hand-written structs, `ForwardPipeline` and
//! `DeferredPipeline`, each owning its attachments, its depth texture and a
//! pipeline cache, each writing `begin_render_pass` out by hand. A fourth
//! pass meant a fourth struct repeating all of it. Now a pipeline is a
//! [`crate::graph::RenderGraph`] — a list of [`crate::pass::PassDesc`]s —
//! and the two shipped ones are built here by
//! [`forward_graph`] and [`deferred_graph`]
//! ([ADR 0021](../../../docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md)).
//!
//! [`PipelineCache`] is what survives from the old shape, generalized: a
//! `wgpu` pipeline is keyed on the shader variant *and* the pass state and
//! target formats it was built for, so the same material draws opaque in
//! one pass, blended in another and front-face-culled into a shadow map,
//! without any of those being baked into one hardcoded descriptor.

use core::fmt;
use std::collections::HashMap;

use wxsl_core::abi::{self, GBufferPrecision, MaterialStage};
use wxsl_core::lighting::LightingSet;
use wxsl_core::resources::VertexAttributeBinding;
use wxsl_core::scene::TagExpr;

use crate::graph::{PassBinding, RenderGraph};
use crate::mesh::{AttributeValues, Vertex};
use crate::pass::{
    Attachment, DepthAttachment, Dimension, DrawSource, Extent, PassDesc, PassState, PassView,
    Read, ResourceDesc, ResourceId, ScreenShader, DEPTH_FORMAT,
};
use crate::variants::{ShaderVariant, VariantKey};

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

    /// This pipeline's pass list, at `target`, under `lighting`.
    pub fn graph(&self, target: TargetConfig, lighting: &LightingSet) -> RenderGraph {
        match self {
            StockPipeline::Forward => forward_graph(target),
            StockPipeline::Deferred => deferred_graph(target, lighting),
        }
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
pub fn gbuffer_format(precision: GBufferPrecision) -> wgpu::TextureFormat {
    match precision {
        // Base colour and metallic are both in 0..1, and this is the one
        // four-channel target where 8 bits is visually enough.
        GBufferPrecision::Normalized => wgpu::TextureFormat::Rgba8Unorm,
        // Normals need a sign and emissive can exceed 1.
        GBufferPrecision::HighDynamicRange => wgpu::TextureFormat::Rgba16Float,
        // The dispatch id: one normalized channel, one byte.
        GBufferPrecision::NormalizedScalar => wgpu::TextureFormat::R8Unorm,
        // A pair of half floats: two signed quantities, half the cost.
        GBufferPrecision::HighDynamicRangePair => wgpu::TextureFormat::Rg16Float,
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
pub fn gbuffer_formats(lighting: &LightingSet) -> Vec<wgpu::TextureFormat> {
    lighting
        .gbuffer_layout()
        .iter()
        .map(|target| gbuffer_format(target.precision))
        .collect()
}

/// What `lighting`'s G-buffer costs against the attachment budget, using
/// the same arithmetic the WebGPU spec does — including the alignment
/// round-up — so a set that fits by this number fits on every device,
/// not just the ones whose drivers are forgiving.
pub fn gbuffer_bytes_per_sample(lighting: &LightingSet) -> u32 {
    let mut total: u32 = 0;
    for format in gbuffer_formats(lighting) {
        // The spec's own table: a four-channel target costs 8 bytes per
        // sample whatever its bit depth ("despite being 4 bytes per pixel,
        // these are 8 bytes per pixel in the table", says wgpu), a pair
        // costs 4, a scalar 1. Alignment rounds up before each add.
        let (cost, alignment) = match format {
            wgpu::TextureFormat::Rgba8Unorm => (8, 1),
            wgpu::TextureFormat::Rgba16Float => (8, 2),
            wgpu::TextureFormat::R8Unorm => (1, 1),
            wgpu::TextureFormat::Rg16Float => (4, 2),
            other => unreachable!("the G-buffer maps only to byte-cost formats, not {other:?}"),
        };
        total = total.next_multiple_of(alignment);
        total += cost;
    }
    total
}

/// Size, format and clear colour of what is being rendered into.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetConfig {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Format of the final colour target.
    ///
    /// A non-`Srgb` format is expected: the ABI's shading function encodes
    /// sRGB itself so that the forward path and the deferred lighting pass
    /// produce identical values (see `wxsl_core::lighting`, which
    /// generates the shading function).
    pub format: wgpu::TextureFormat,
    /// Colour to clear to where nothing is drawn.
    pub clear_color: wgpu::Color,
}

impl TargetConfig {
    /// A config for `width` x `height` in `format`, cleared to a dark grey.
    pub fn new(width: u32, height: u32, format: wgpu::TextureFormat) -> Self {
        TargetConfig {
            width: width.max(1),
            height: height.max(1),
            format,
            clear_color: wgpu::Color {
                r: 0.02,
                g: 0.02,
                b: 0.03,
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
            .with_usage(wgpu::TextureUsages::TEXTURE_BINDING)
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
    graph.pass(
        PassDesc::geometry("depth prepass", everything(), MaterialStage::DEPTH_ONLY)
            .with_depth(DepthAttachment::clear(depth, 1.0)),
    );
    graph.pass(
        PassDesc::geometry("forward", everything(), MaterialStage::FORWARD_LIT)
            .with_color(Attachment::clear(RenderGraph::TARGET, target.clear_color))
            .with_depth(DepthAttachment::load(depth))
            .with_state(PassState::OPAQUE.with_depth_test(wgpu::CompareFunction::LessEqual, false)),
    );
    graph
}

/// The deferred pipeline: write the surface into a G-buffer, then shade it.
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

    graph.pass(
        PassDesc::geometry("deferred material", everything(), MaterialStage::GBUFFER)
            // Cleared to zero, which matters for the depth-based background
            // test in the lighting pass: that is what keeps the clear colour
            // visible where nothing was drawn.
            .with_colors(
                gbuffer
                    .iter()
                    .map(|id| Attachment::clear(*id, wgpu::Color::TRANSPARENT)),
            )
            .with_depth(DepthAttachment::clear(depth, 1.0)),
    );
    graph.pass(
        PassDesc::screen("deferred lighting", ScreenShader::DeferredLighting)
            .with_color(Attachment::clear(RenderGraph::TARGET, target.clear_color))
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
    graph
}

/// The bind group layouts a material's interface asks for, and the shape
/// they came from.
///
/// Borrowed rather than owned because they live in
/// [`crate::bindings::BindingLayouts`], which shares one layout between
/// every material of the same shape.
#[derive(Clone, Copy)]
pub struct MaterialGroups<'a> {
    /// The per-vertex streams the material declares, which decide the
    /// pipeline's vertex buffer layout. Empty for a material that
    /// declares none, and then the layout is exactly `Vertex::LAYOUT` and
    /// nothing else.
    pub vertex: &'a [VertexAttributeBinding],
    /// `abi::GROUP_MATERIAL`, or `None` for a material declaring neither
    /// a parameter nor a texture.
    pub material: Option<&'a wgpu::BindGroupLayout>,
    /// `abi::GROUP_USER`, or `None` for a material declaring no block.
    pub user: Option<&'a wgpu::BindGroupLayout>,
    /// [`wxsl_core::resources::MaterialInterface::signature`], which is
    /// what the caches are keyed on.
    pub signature: &'a str,
}

impl MaterialGroups<'_> {
    /// A material that declares nothing at all — and what a pass with no
    /// material behind it uses.
    pub const NONE: MaterialGroups<'static> = MaterialGroups {
        vertex: &[],
        material: None,
        user: None,
        signature: "",
    };
}

/// Identity of one `wgpu` pipeline.
///
/// The variant alone is not enough any more: the same compiled shader is a
/// different pipeline in a pass that culls front faces, in one that blends,
/// and in one writing a different set of formats.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PipelineKey {
    variant: VariantKey,
    state: PassState,
    targets: Vec<wgpu::TextureFormat>,
    /// The shape of the pass group the pipeline was laid out for. Two
    /// passes may run the same shader over different inputs — a screen
    /// effect with a G-buffer and one with a single image — and a pipeline
    /// built for one cannot be used with the other's bind group.
    pass_group: Vec<PassBinding>,
    /// The shape of the material's own groups, from
    /// [`wxsl_core::resources::MaterialInterface::signature`]. A *shape*
    /// and never a value: two materials with the same parameters at
    /// different settings share this pipeline, which is exactly what
    /// makes a slider free.
    material_group: String,
}

/// One `wgpu` pipeline per (variant, pass state, target formats).
///
/// Shared by every pass in a frame, so a forward pass and a shadow pass
/// drawing the same material each pay one pipeline creation, once.
pub struct PipelineCache {
    render: HashMap<PipelineKey, wgpu::RenderPipeline>,
    layouts: HashMap<LayoutKey, wgpu::PipelineLayout>,
}

/// Which four bind group layouts a pipeline layout was built from.
///
/// Groups 1 and 2 are the material's, and both may be absent; group 3 is
/// the pass's, described by its bindings' shapes. Group 0 is the frame's
/// and is the same for every pipeline, so it is not part of the key.
type LayoutKey = (Vec<PassBinding>, String);

impl Default for PipelineCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PipelineCache {
    /// An empty cache.
    pub fn new() -> Self {
        PipelineCache {
            render: HashMap::new(),
            layouts: HashMap::new(),
        }
    }

    /// How many pipelines are cached.
    pub fn len(&self) -> usize {
        self.render.len()
    }

    /// Whether nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.render.is_empty()
    }

    /// Forget everything, e.g. because the frame group's layout changed.
    pub fn clear(&mut self) {
        self.render.clear();
        self.layouts.clear();
    }

    /// The pipeline layout for a draw: the frame group, whatever the
    /// material declares, and the pass group if the pass reads anything.
    ///
    /// `wgpu` takes `Option`s, so a hole is expressible rather than
    /// needing a filler layout — which matters because a material that
    /// declares no parameters really does leave group 1 empty, while a
    /// pass that reads a G-buffer occupies group 3 above it (ADR 0010).
    /// Trailing `None`s are trimmed, because a layout that stops at the
    /// last group it uses is the same layout.
    fn layout(
        &mut self,
        device: &wgpu::Device,
        frame: &wgpu::BindGroupLayout,
        material: &MaterialGroups<'_>,
        pass: Option<&wgpu::BindGroupLayout>,
        shape: &[PassBinding],
    ) -> &wgpu::PipelineLayout {
        // Keyed on *shapes*: layouts with the same entries are
        // interchangeable to `wgpu`, and layouts with different ones must
        // not share a pipeline layout.
        let key = (shape.to_vec(), material.signature.to_string());
        self.layouts.entry(key).or_insert_with(|| {
            let mut groups: Vec<Option<&wgpu::BindGroupLayout>> =
                vec![Some(frame), material.material, material.user, pass];
            while matches!(groups.last(), Some(None)) {
                groups.pop();
            }
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("wxsl pass"),
                bind_group_layouts: &groups,
                immediate_size: 0,
            })
        })
    }

    /// The pipeline for drawing `variant`'s geometry in a pass with this
    /// state and these targets, created on first use.
    ///
    /// `fragment_entry` is the *module's*, not the stage's: whether a
    /// depth or shadow stage has a fragment program is a property of the
    /// material — one that discards needs one, one that does not gets a
    /// pipeline with no fragment state at all, which is why a depth
    /// prepass is cheap (ADR 0025).
    #[allow(clippy::too_many_arguments)]
    pub fn geometry(
        &mut self,
        device: &wgpu::Device,
        frame: &wgpu::BindGroupLayout,
        material: &MaterialGroups<'_>,
        pass_layout: Option<&wgpu::BindGroupLayout>,
        variant: &ShaderVariant,
        fragment_entry: Option<&str>,
        state: PassState,
        targets: &[Option<wgpu::ColorTargetState>],
        pass_shape: &[PassBinding],
    ) -> &wgpu::RenderPipeline {
        self.create(
            device,
            frame,
            material,
            pass_layout,
            variant,
            state,
            targets,
            pass_shape,
            abi::VERTEX_ENTRY,
            fragment_entry,
            true,
        )
    }

    /// The pipeline for a fullscreen pass running `variant`.
    ///
    /// No vertex buffer: the triangle comes from `@builtin(vertex_index)`.
    #[allow(clippy::too_many_arguments)]
    pub fn screen(
        &mut self,
        device: &wgpu::Device,
        frame: &wgpu::BindGroupLayout,
        pass_layout: Option<&wgpu::BindGroupLayout>,
        variant: &ShaderVariant,
        state: PassState,
        targets: &[Option<wgpu::ColorTargetState>],
        pass_shape: &[PassBinding],
    ) -> &wgpu::RenderPipeline {
        // No material, and therefore no material groups: by the time this
        // pass runs the material has already been resolved into the
        // G-buffer.
        self.create(
            device,
            frame,
            &MaterialGroups::NONE,
            pass_layout,
            variant,
            state,
            targets,
            pass_shape,
            abi::LIGHTING_PASS_VERTEX_ENTRY,
            Some(abi::LIGHTING_PASS_FRAGMENT_ENTRY),
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn create(
        &mut self,
        device: &wgpu::Device,
        frame: &wgpu::BindGroupLayout,
        material: &MaterialGroups<'_>,
        pass_layout: Option<&wgpu::BindGroupLayout>,
        variant: &ShaderVariant,
        state: PassState,
        targets: &[Option<wgpu::ColorTargetState>],
        pass_shape: &[PassBinding],
        vertex_entry: &str,
        fragment_entry: Option<&str>,
        vertex_buffers: bool,
    ) -> &wgpu::RenderPipeline {
        let key = PipelineKey {
            variant: variant.key,
            state,
            targets: targets
                .iter()
                .flatten()
                .map(|target| target.format)
                .collect(),
            pass_group: pass_shape.to_vec(),
            material_group: material.signature.to_string(),
        };
        if !self.render.contains_key(&key) {
            // Two statements rather than `entry().or_insert_with()`: the
            // layout cache is also `&mut self`, and the borrow checker is
            // right that it cannot be borrowed inside the closure.
            let layout = self
                .layout(device, frame, material, pass_layout, pass_shape)
                .clone();
            // One buffer per declared attribute, at the slot the
            // interface numbered it — not a widened interleaved struct,
            // so a mesh can serve a material that wants colours and one
            // that does not without re-uploading its positions.
            let attributes: Vec<[wgpu::VertexAttribute; 1]> = material
                .vertex
                .iter()
                .filter_map(|attribute| {
                    Some([wgpu::VertexAttribute {
                        format: AttributeValues::format(attribute.ty)?,
                        offset: 0,
                        shader_location: attribute.location,
                    }])
                })
                .collect();
            let mut layouts: Vec<Option<wgpu::VertexBufferLayout>> = Vec::new();
            if vertex_buffers {
                layouts.push(Some(Vertex::LAYOUT));
                for (attribute, entry) in material.vertex.iter().zip(&attributes) {
                    layouts.push(Some(wgpu::VertexBufferLayout {
                        array_stride: u64::from(attribute.ty.buffer_size().unwrap_or(4)),
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: entry,
                    }));
                }
            }
            let buffers: &[Option<wgpu::VertexBufferLayout>] = &layouts;
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&variant.label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &variant.module,
                    entry_point: Some(vertex_entry),
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment: fragment_entry.map(|entry| wgpu::FragmentState {
                    module: &variant.module,
                    entry_point: Some(entry),
                    targets,
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    cull_mode: state.cull_mode,
                    ..Default::default()
                },
                depth_stencil: state.depth_stencil(),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
            self.render.insert(key.clone(), pipeline);
        }
        &self.render[&key]
    }
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

    #[test]
    fn the_gbuffer_formats_come_from_the_abi_table() {
        let formats = gbuffer_formats(&default_lighting());
        assert_eq!(formats.len(), abi::GBUFFER_BASE_TARGETS.len());
        assert_eq!(formats[0], wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!(formats[1], wgpu::TextureFormat::Rgba16Float);
    }

    #[test]
    fn target_config_never_has_a_zero_dimension() {
        // A minimized window reports zero, and a zero-sized texture is a
        // validation error rather than a no-op.
        let config = TargetConfig::new(0, 0, wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!((config.width, config.height), (1, 1));
    }

    fn config() -> TargetConfig {
        TargetConfig::new(64, 64, wgpu::TextureFormat::Rgba8Unorm)
    }

    #[test]
    fn the_forward_pipeline_is_a_depth_prepass_and_a_shading_pass() {
        let graph = forward_graph(config());
        // A shadow pass per light slot comes first; the two that make this
        // pipeline what it is follow.
        assert_eq!(graph.passes().len(), abi::MAX_LIGHTS + 2);

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
        assert_eq!(
            shading.state.depth_compare,
            wgpu::CompareFunction::LessEqual
        );

        // Loading the depth the prepass wrote is what orders the two.
        let schedule = graph.schedule().expect("the forward pass list schedules");
        assert_eq!(
            schedule.order(),
            (0..abi::MAX_LIGHTS + 2).collect::<Vec<_>>()
        );
        // The shadow maps and the depth buffer: two textures, and the
        // shadow maps are never handed to anything else because the frame
        // group holds a view of them all frame.
        assert_eq!(schedule.slots().len(), 2);
    }

    #[test]
    fn every_pipeline_fills_one_shadow_slice_per_light_from_that_lights_view() {
        for pipeline in StockPipeline::ALL {
            let graph = pipeline.graph(config(), &default_lighting());
            let maps = graph.shadow_maps().expect("shadow maps are declared");
            let desc = graph.resource_desc(maps).expect("declared resource");
            assert_eq!(desc.dimension, Dimension::D2Array);
            assert_eq!(desc.layers, abi::MAX_LIGHTS as u32);
            assert_eq!(desc.format, DEPTH_FORMAT);
            // Nothing in the pass list reads it — the frame group does —
            // so both of these have to be spelled out.
            assert!(desc.usage.contains(wgpu::TextureUsages::TEXTURE_BINDING));
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
        assert_eq!(graph.passes().len(), lighting + 1);
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
            graph.passes()[lighting].kind,
            PassKind::Screen {
                shader: ScreenShader::DeferredLighting
            }
        ));
        // The lighting pass reads every G-buffer target plus depth, and
        // those reads are what order it after the material pass.
        assert_eq!(
            graph.passes()[lighting].reads.len(),
            abi::GBUFFER_BASE_TARGETS.len() + 1
        );
        let schedule = graph.schedule().expect("the deferred pass list schedules");
        assert_eq!(schedule.order(), (0..=lighting).collect::<Vec<_>>());
    }

    #[test]
    fn both_stock_pipelines_schedule_at_any_size() {
        for pipeline in StockPipeline::ALL {
            for (width, height) in [(1, 1), (64, 64), (3840, 2160)] {
                let target = TargetConfig::new(width, height, wgpu::TextureFormat::Rgba8Unorm);
                pipeline
                    .graph(target, &default_lighting())
                    .schedule()
                    .unwrap_or_else(|error| panic!("{pipeline} at {width}x{height}: {error}"));
            }
        }
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
            let graph = pipeline.graph(config(), &default_lighting());
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
}
