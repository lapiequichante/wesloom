//! wgpu pipeline caches and adapters for shared pipeline plans (ADR 0048).

use crate::graph::{PassBinding, RenderGraph};
use crate::mesh::{AttributeValues, Vertex};
use crate::pass::PassState;
use crate::types::{self, WgpuType};
use crate::variants::{ShaderVariant, VariantKey};
use std::collections::HashMap;
use wxsl_core::abi::{self, GBufferPrecision};
use wxsl_core::lighting::LightingSet;
use wxsl_core::resources::VertexAttributeBinding;

pub use wxsl_frame::pipeline::{
    gbuffer_bytes_per_sample, gbuffer_layout_bytes_per_sample, PipelineConfig, StockPipeline,
    MAX_GBUFFER_BYTES_PER_SAMPLE,
};

/// Size, format and clear colour of what is being rendered into.
#[derive(Clone, Copy, Debug, PartialEq)]
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
    pub format: wgpu::TextureFormat,
    /// Colour to clear to where nothing is drawn, as **linear radiance**.
    ///
    /// It is what the head of the chain clears to, so it goes through the
    /// display transform like everything else drawn — which is why the
    /// default below is a much smaller number than the near-black it
    /// produces on screen.
    pub clear_color: wgpu::Color,
}

impl TargetConfig {
    /// A config for `width` x `height` in `format`, cleared to a near-black
    /// with a little blue in it.
    pub fn new(width: u32, height: u32, format: wgpu::TextureFormat) -> Self {
        TargetConfig {
            width: width.max(1),
            height: height.max(1),
            format,
            clear_color: wgpu::Color {
                r: 0.001,
                g: 0.001,
                b: 0.0015,
                a: 1.0,
            },
        }
    }
}

impl From<TargetConfig> for wxsl_frame::pipeline::TargetConfig {
    fn from(target: TargetConfig) -> Self {
        Self {
            width: target.width,
            height: target.height,
            format: types::TextureFormat::from_wgpu(target.format),
            clear_color: types::Color::from_wgpu(target.clear_color),
        }
    }
}

impl From<wxsl_frame::pipeline::TargetConfig> for TargetConfig {
    fn from(target: wxsl_frame::pipeline::TargetConfig) -> Self {
        Self {
            width: target.width,
            height: target.height,
            format: target.format.to_wgpu(),
            clear_color: target.clear_color.to_wgpu(),
        }
    }
}

/// Native format for a shared G-buffer precision.
pub fn gbuffer_format(precision: GBufferPrecision) -> wgpu::TextureFormat {
    wxsl_frame::pipeline::gbuffer_format(precision).to_wgpu()
}

/// Native G-buffer formats in attachment order.
pub fn gbuffer_formats(lighting: &LightingSet) -> Vec<wgpu::TextureFormat> {
    wxsl_frame::pipeline::gbuffer_formats(lighting)
        .into_iter()
        .map(WgpuType::to_wgpu)
        .collect()
}

/// Reference forward graph, backed by the shared planner.
pub fn forward_graph(target: TargetConfig) -> RenderGraph {
    wxsl_frame::pipeline::forward_graph(target.into()).into()
}

/// Reference deferred graph, backed by the shared planner.
pub fn deferred_graph(target: TargetConfig, lighting: &LightingSet) -> RenderGraph {
    wxsl_frame::pipeline::deferred_graph(target.into(), lighting).into()
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
    /// Format and blend of each colour target. The blend is per target:
    /// a peel resolve under-blends one accumulator and over-blends the
    /// other, and the pass-wide [`PassState`] blend cannot say both.
    targets: Vec<(wgpu::TextureFormat, Option<wgpu::BlendState>)>,
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
    compute: HashMap<(VariantKey, Vec<PassBinding>), wgpu::ComputePipeline>,
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
            compute: HashMap::new(),
            layouts: HashMap::new(),
        }
    }

    /// How many pipelines are cached.
    pub fn len(&self) -> usize {
        self.render.len() + self.compute.len()
    }

    /// Whether nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.render.is_empty() && self.compute.is_empty()
    }

    /// Forget everything, e.g. because the frame group's layout changed.
    pub fn clear(&mut self) {
        self.render.clear();
        self.compute.clear();
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

    /// The pipeline for a fullscreen pass running `variant` — the module
    /// of some effect, whose entry points name the stages here.
    ///
    /// No vertex buffer: the triangle comes from `@builtin(vertex_index)`.
    #[allow(clippy::too_many_arguments)]
    pub fn screen(
        &mut self,
        device: &wgpu::Device,
        frame: &wgpu::BindGroupLayout,
        pass_layout: Option<&wgpu::BindGroupLayout>,
        variant: &ShaderVariant,
        vertex_entry: &str,
        fragment_entry: &str,
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
            vertex_entry,
            Some(fragment_entry),
            false,
        )
    }

    /// The compute pipeline for a compute effect's variant.
    ///
    /// The layout is the pass group and nothing else: a compute effect
    /// declares only `@group(3)`, having no draws to transform and — so
    /// far — no uniforms of its own. A compute effect that wants the
    /// frame group is a real consumer away from getting it.
    pub fn compute(
        &mut self,
        device: &wgpu::Device,
        pass_layout: Option<&wgpu::BindGroupLayout>,
        variant: &ShaderVariant,
        entry: &str,
        pass_shape: &[PassBinding],
    ) -> &wgpu::ComputePipeline {
        let key = (variant.key, pass_shape.to_vec());
        if !self.compute.contains_key(&key) {
            // Groups 0–2 absent, the pass group at `abi::GROUP_PASS` —
            // where the effect's shader declares it.
            let groups: [Option<&wgpu::BindGroupLayout>; 4] = [None, None, None, pass_layout];
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&variant.label),
                bind_group_layouts: &groups,
                immediate_size: 0,
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(&variant.label),
                layout: Some(&layout),
                module: &variant.module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            });
            self.compute.insert(key.clone(), pipeline);
        }
        &self.compute[&key]
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
                .map(|target| (target.format, target.blend))
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
                    cull_mode: state.cull_mode.map(WgpuType::to_wgpu),
                    ..Default::default()
                },
                depth_stencil: types::depth_stencil(state),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
            self.render.insert(key.clone(), pipeline);
        }
        &self.render[&key]
    }
}
