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

use std::collections::HashMap;

use wxsl_core::abi::{self, GBufferPrecision};
use wxsl_core::scene::TagExpr;

use crate::graph::{PassBinding, RenderGraph};
use crate::mesh::Vertex;
use crate::pass::{
    Attachment, DepthAttachment, DrawSource, PassDesc, PassState, Read, ResourceDesc, ResourceId,
    ScreenShader, DEPTH_FORMAT,
};
use crate::path::RenderPath;
use crate::variants::{ShaderVariant, VariantKey};

/// The `wgpu` format for a G-buffer target of the given precision.
pub fn gbuffer_format(precision: GBufferPrecision) -> wgpu::TextureFormat {
    match precision {
        // Base colour and metallic are both in 0..1, and this is the one
        // target where 8 bits is visually enough.
        GBufferPrecision::Normalized => wgpu::TextureFormat::Rgba8Unorm,
        // Normals need a sign and emissive can exceed 1.
        GBufferPrecision::HighDynamicRange => wgpu::TextureFormat::Rgba16Float,
    }
}

/// The G-buffer's formats, in `@location` order, from the ABI's table.
pub fn gbuffer_formats() -> Vec<wgpu::TextureFormat> {
    abi::GBUFFER_TARGETS
        .iter()
        .map(|target| gbuffer_format(target.precision))
        .collect()
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
    /// produce identical values (see `shaders/wxsl/shading.wxsl`).
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

/// The forward pipeline: one pass, the material shades its own surface.
pub fn forward_graph(target: TargetConfig) -> RenderGraph {
    let mut graph = RenderGraph::new(target.format);
    let depth = graph.resource(ResourceDesc::color("forward depth", DEPTH_FORMAT));
    graph.pass(
        PassDesc::geometry("forward", everything(), RenderPath::Forward)
            .with_color(Attachment::clear(RenderGraph::TARGET, target.clear_color))
            .with_depth(DepthAttachment::clear(depth, 1.0)),
    );
    graph
}

/// The deferred pipeline: write the surface into a G-buffer, then shade it.
///
/// The G-buffer's depth is sampled by the lighting pass rather than attached
/// to it — a depth texture cannot be attached and sampled in the same pass —
/// which the graph expresses as a read, and therefore as the edge that
/// orders the two passes.
pub fn deferred_graph(target: TargetConfig) -> RenderGraph {
    let mut graph = RenderGraph::new(target.format);
    let gbuffer: Vec<ResourceId> = abi::GBUFFER_TARGETS
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
        PassDesc::geometry("deferred material", everything(), RenderPath::Deferred)
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
            // Bindings in `abi::GBUFFER_TARGETS` order, with depth last —
            // the order `lighting_pass.wxsl` declares them in.
            .with_reads(
                gbuffer
                    .iter()
                    .chain(core::iter::once(&depth))
                    .map(|id| Read::current(*id)),
            ),
    );
    graph
}

/// The stock pass list for `path`.
pub fn graph_for(path: RenderPath, target: TargetConfig) -> RenderGraph {
    match path {
        RenderPath::Forward => forward_graph(target),
        RenderPath::Deferred => deferred_graph(target),
    }
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
}

/// One `wgpu` pipeline per (variant, pass state, target formats).
///
/// Shared by every pass in a frame, so a forward pass and a shadow pass
/// drawing the same material each pay one pipeline creation, once.
pub struct PipelineCache {
    render: HashMap<PipelineKey, wgpu::RenderPipeline>,
    layouts: HashMap<Vec<PassBinding>, wgpu::PipelineLayout>,
}

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

    /// The pipeline layout for a pass with or without a pass group.
    ///
    /// Groups 1 (material) and 2 (user) are genuinely empty here: nothing
    /// declares parameters until M3, and the user group is the
    /// application's. `wgpu` takes `Option`s, so the holes are expressible
    /// rather than needing filler layouts (ADR 0010).
    fn layout(
        &mut self,
        device: &wgpu::Device,
        frame: &wgpu::BindGroupLayout,
        pass: Option<&wgpu::BindGroupLayout>,
        shape: &[PassBinding],
    ) -> &wgpu::PipelineLayout {
        // Keyed on the pass group's *shape*: layouts with the same entries
        // are interchangeable to `wgpu`, and layouts with different ones
        // must not share a pipeline layout.
        self.layouts.entry(shape.to_vec()).or_insert_with(|| {
            let groups: Vec<Option<&wgpu::BindGroupLayout>> = match pass {
                Some(pass) => vec![Some(frame), None, None, Some(pass)],
                None => vec![Some(frame)],
            };
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("wxsl pass"),
                bind_group_layouts: &groups,
                immediate_size: 0,
            })
        })
    }

    /// The pipeline for drawing `variant`'s geometry in a pass with this
    /// state and these targets, created on first use.
    #[allow(clippy::too_many_arguments)]
    pub fn geometry(
        &mut self,
        device: &wgpu::Device,
        frame: &wgpu::BindGroupLayout,
        pass_layout: Option<&wgpu::BindGroupLayout>,
        variant: &ShaderVariant,
        state: PassState,
        targets: &[Option<wgpu::ColorTargetState>],
        pass_shape: &[PassBinding],
    ) -> &wgpu::RenderPipeline {
        self.create(
            device,
            frame,
            pass_layout,
            variant,
            state,
            targets,
            pass_shape,
            abi::VERTEX_ENTRY,
            abi::FRAGMENT_ENTRY,
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
        self.create(
            device,
            frame,
            pass_layout,
            variant,
            state,
            targets,
            pass_shape,
            abi::LIGHTING_PASS_VERTEX_ENTRY,
            abi::LIGHTING_PASS_FRAGMENT_ENTRY,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn create(
        &mut self,
        device: &wgpu::Device,
        frame: &wgpu::BindGroupLayout,
        pass_layout: Option<&wgpu::BindGroupLayout>,
        variant: &ShaderVariant,
        state: PassState,
        targets: &[Option<wgpu::ColorTargetState>],
        pass_shape: &[PassBinding],
        vertex_entry: &str,
        fragment_entry: &str,
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
        };
        if !self.render.contains_key(&key) {
            // Two statements rather than `entry().or_insert_with()`: the
            // layout cache is also `&mut self`, and the borrow checker is
            // right that it cannot be borrowed inside the closure.
            let layout = self.layout(device, frame, pass_layout, pass_shape).clone();
            let buffers: &[Option<wgpu::VertexBufferLayout>] = if vertex_buffers {
                &[Some(Vertex::LAYOUT)]
            } else {
                &[]
            };
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&variant.label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &variant.module,
                    entry_point: Some(vertex_entry),
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &variant.module,
                    entry_point: Some(fragment_entry),
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

    #[test]
    fn the_gbuffer_formats_come_from_the_abi_table() {
        let formats = gbuffer_formats();
        assert_eq!(formats.len(), abi::GBUFFER_TARGETS.len());
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
    fn the_forward_pipeline_is_one_geometry_pass() {
        let graph = forward_graph(config());
        assert_eq!(graph.passes().len(), 1);
        assert!(matches!(
            graph.passes()[0].kind,
            PassKind::Geometry {
                path: RenderPath::Forward,
                ..
            }
        ));
        assert_eq!(graph.passes()[0].color.len(), 1);
        graph.schedule().expect("the forward pass list schedules");
    }

    #[test]
    fn the_deferred_pipeline_is_a_geometry_pass_and_a_screen_pass() {
        let graph = deferred_graph(config());
        assert_eq!(graph.passes().len(), 2);
        assert_eq!(graph.passes()[0].color.len(), abi::GBUFFER_TARGETS.len());
        assert!(matches!(
            graph.passes()[1].kind,
            PassKind::Screen {
                shader: ScreenShader::DeferredLighting
            }
        ));
        // The lighting pass reads every G-buffer target plus depth, and
        // those reads are what order it after the material pass.
        assert_eq!(
            graph.passes()[1].reads.len(),
            abi::GBUFFER_TARGETS.len() + 1
        );
        let schedule = graph.schedule().expect("the deferred pass list schedules");
        assert_eq!(schedule.order(), &[0, 1]);
    }

    #[test]
    fn both_stock_pipelines_schedule_at_any_size() {
        for path in RenderPath::ALL {
            for (width, height) in [(1, 1), (64, 64), (3840, 2160)] {
                let target = TargetConfig::new(width, height, wgpu::TextureFormat::Rgba8Unorm);
                graph_for(*path, target)
                    .schedule()
                    .unwrap_or_else(|error| panic!("{path} at {width}x{height}: {error}"));
            }
        }
    }
}
