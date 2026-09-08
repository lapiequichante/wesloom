//! The [`Pipeline`] trait both render paths implement, and the two
//! implementations.
//!
//! The trait exists so an application can hold "the current pipeline" and
//! swap it without touching the code that says "render this scene"
//! ([ADR 0005](../../../docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md)).
//! [`crate::renderer::Renderer`] is that application-facing side; what is
//! here is the two shapes a frame can take:
//!
//! * [`ForwardPipeline`]: one pass. The material's fragment entry shades the
//!   surface and writes a colour.
//! * [`DeferredPipeline`]: two passes. The material's fragment entry writes
//!   the surface into a G-buffer; a fullscreen pass then reads it back,
//!   reconstructs the world position from depth, and shades. The material
//!   shader is the *same graph*, compiled with the deferred flag set.
//!
//! Both build their `wgpu` pipelines lazily and cache them per shader
//! variant, so switching path or flipping a macro costs one pipeline
//! creation, not one per frame.

use std::collections::HashMap;

use wesloom_core::abi::{self, GBufferPrecision};

use crate::error::RenderError;
use crate::mesh::{Mesh, Vertex};
use crate::path::RenderPath;
use crate::scene::SceneBindings;
use crate::variants::ShaderVariant;

/// Depth format used by both paths.
///
/// `Depth32Float` rather than a packed depth-stencil format because the
/// deferred lighting pass samples depth to reconstruct world position, and
/// there is no stencil work to do.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

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
    /// produce identical values (see `shaders/wesloom/shading.wesl`).
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

/// One object to draw: geometry plus the material variant to draw it with.
pub struct Draw<'a> {
    /// The geometry.
    pub mesh: &'a Mesh,
    /// The material variant, compiled for this pipeline's render path.
    pub material: &'a ShaderVariant,
}

/// Everything a pipeline needs to record a frame.
pub struct FrameInput<'a> {
    /// Where the final colour goes.
    pub target: &'a wgpu::TextureView,
    /// Camera, scene and object uniforms (bind group 0).
    pub bindings: &'a SceneBindings,
    /// What to draw.
    pub draws: &'a [Draw<'a>],
    /// The deferred lighting pass shader. Only the deferred path uses it.
    pub lighting: Option<&'a ShaderVariant>,
}

/// A way of turning draws into pixels.
pub trait Pipeline {
    /// Which render path this pipeline implements. A material must be
    /// compiled for this path or its entry points will not match.
    fn path(&self) -> RenderPath;

    /// (Re)create size- and format-dependent resources. Cheap and idempotent
    /// when nothing changed, so it is safe to call every frame.
    fn configure(&mut self, device: &wgpu::Device, target: TargetConfig);

    /// Record this pipeline's passes into `encoder`.
    fn record(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput<'_>,
    ) -> Result<(), RenderError>;
}

/// A depth attachment matching the current target size.
struct DepthAttachment {
    view: wgpu::TextureView,
    size: (u32, u32),
}

impl DepthAttachment {
    fn new(device: &wgpu::Device, width: u32, height: u32, usage: wgpu::TextureUsages) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wesloom depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage,
            view_formats: &[],
        });
        DepthAttachment {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            size: (width, height),
        }
    }

    fn attachment(&self) -> wgpu::RenderPassDepthStencilAttachment<'_> {
        wgpu::RenderPassDepthStencilAttachment {
            view: &self.view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(1.0),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }
    }
}

/// Shared state every material pipeline needs: the layout, and one `wgpu`
/// pipeline per shader variant.
struct MaterialPipelines {
    layout: wgpu::PipelineLayout,
    cache: HashMap<u64, wgpu::RenderPipeline>,
}

impl MaterialPipelines {
    fn new(device: &wgpu::Device, bind_group_layout: &wgpu::BindGroupLayout) -> Self {
        MaterialPipelines {
            layout: device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("wesloom material"),
                bind_group_layouts: &[Some(bind_group_layout)],
                immediate_size: 0,
            }),
            cache: HashMap::new(),
        }
    }

    /// The pipeline for `variant`, created on first use.
    fn get(
        &mut self,
        device: &wgpu::Device,
        variant: &ShaderVariant,
        targets: &[Option<wgpu::ColorTargetState>],
    ) -> &wgpu::RenderPipeline {
        self.cache.entry(variant.key.identity).or_insert_with(|| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&variant.label),
                layout: Some(&self.layout),
                vertex: wgpu::VertexState {
                    module: &variant.module,
                    entry_point: Some(abi::VERTEX_ENTRY),
                    buffers: &[Some(Vertex::LAYOUT)],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &variant.module,
                    entry_point: Some(abi::FRAGMENT_ENTRY),
                    targets,
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        })
    }

    /// Forget every cached pipeline, e.g. because the target format changed.
    fn clear(&mut self) {
        self.cache.clear();
    }
}

fn opaque_target(format: wgpu::TextureFormat) -> Option<wgpu::ColorTargetState> {
    Some(wgpu::ColorTargetState {
        format,
        blend: None,
        write_mask: wgpu::ColorWrites::ALL,
    })
}

/// Shade where the surface is evaluated: one pass, one attachment.
pub struct ForwardPipeline {
    pipelines: MaterialPipelines,
    depth: Option<DepthAttachment>,
    target: Option<TargetConfig>,
}

impl ForwardPipeline {
    /// Create the forward pipeline.
    pub fn new(device: &wgpu::Device, bindings: &SceneBindings) -> Self {
        ForwardPipeline {
            pipelines: MaterialPipelines::new(device, bindings.layout()),
            depth: None,
            target: None,
        }
    }
}

impl Pipeline for ForwardPipeline {
    fn path(&self) -> RenderPath {
        RenderPath::Forward
    }

    fn configure(&mut self, device: &wgpu::Device, target: TargetConfig) {
        if self.target.map(|current| current.format) != Some(target.format) {
            // A different attachment format makes every cached pipeline
            // invalid, since the format is baked into it.
            self.pipelines.clear();
        }
        let size = (target.width, target.height);
        if self.depth.as_ref().map(|depth| depth.size) != Some(size) {
            self.depth = Some(DepthAttachment::new(
                device,
                target.width,
                target.height,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            ));
        }
        self.target = Some(target);
    }

    fn record(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput<'_>,
    ) -> Result<(), RenderError> {
        let target = self.target.ok_or(RenderError::NotConfigured)?;
        let depth = self.depth.as_ref().ok_or(RenderError::NotConfigured)?;
        let color_targets = [opaque_target(target.format)];

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("wesloom forward"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: input.target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(target.clear_color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(depth.attachment()),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, input.bindings.bind_group(), &[]);
        for draw in input.draws {
            pass.set_pipeline(self.pipelines.get(device, draw.material, &color_targets));
            draw.mesh.draw(&mut pass);
        }
        Ok(())
    }
}

/// The G-buffer textures and the bind group that reads them back.
struct GBuffer {
    views: Vec<wgpu::TextureView>,
    depth: DepthAttachment,
    bind_group: wgpu::BindGroup,
    size: (u32, u32),
}

impl GBuffer {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, width: u32, height: u32) -> Self {
        let views: Vec<wgpu::TextureView> = abi::GBUFFER_TARGETS
            .iter()
            .map(|target| {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&format!("wesloom gbuffer {}", target.field)),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: gbuffer_format(target.precision),
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                texture.create_view(&wgpu::TextureViewDescriptor::default())
            })
            .collect();
        let depth = DepthAttachment::new(
            device,
            width,
            height,
            // Also sampled: the lighting pass reconstructs world position
            // from depth rather than storing it in a fourth attachment.
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );

        let mut entries: Vec<wgpu::BindGroupEntry> = views
            .iter()
            .enumerate()
            .map(|(index, view)| wgpu::BindGroupEntry {
                binding: index as u32,
                resource: wgpu::BindingResource::TextureView(view),
            })
            .collect();
        entries.push(wgpu::BindGroupEntry {
            binding: views.len() as u32,
            resource: wgpu::BindingResource::TextureView(&depth.view),
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wesloom gbuffer"),
            layout,
            entries: &entries,
        });

        GBuffer {
            views,
            depth,
            bind_group,
            size: (width, height),
        }
    }

    /// The bind group layout for the G-buffer textures (bind group 1).
    fn layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        let mut entries: Vec<wgpu::BindGroupLayoutEntry> = abi::GBUFFER_TARGETS
            .iter()
            .enumerate()
            .map(|(index, _)| wgpu::BindGroupLayoutEntry {
                binding: index as u32,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            })
            .collect();
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: abi::GBUFFER_TARGETS.len() as u32,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Depth,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wesloom gbuffer"),
            entries: &entries,
        })
    }
}

/// Write the surface to a G-buffer, then light it in a fullscreen pass.
pub struct DeferredPipeline {
    material_pipelines: MaterialPipelines,
    lighting_layout: wgpu::PipelineLayout,
    lighting_pipelines: HashMap<u64, wgpu::RenderPipeline>,
    gbuffer_layout: wgpu::BindGroupLayout,
    gbuffer: Option<GBuffer>,
    target: Option<TargetConfig>,
}

impl DeferredPipeline {
    /// Create the deferred pipeline.
    pub fn new(device: &wgpu::Device, bindings: &SceneBindings) -> Self {
        let gbuffer_layout = GBuffer::layout(device);
        DeferredPipeline {
            material_pipelines: MaterialPipelines::new(device, bindings.layout()),
            lighting_layout: device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("wesloom deferred lighting"),
                bind_group_layouts: &[Some(bindings.layout()), Some(&gbuffer_layout)],
                immediate_size: 0,
            }),
            lighting_pipelines: HashMap::new(),
            gbuffer_layout,
            gbuffer: None,
            target: None,
        }
    }
}

/// Build the fullscreen lighting pipeline for one shader variant.
///
/// A free function rather than a method so that recording a frame can take
/// the pipeline cache and the G-buffer as two separate borrows of
/// [`DeferredPipeline`] instead of borrowing the whole struct mutably.
fn create_lighting_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    variant: &ShaderVariant,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&variant.label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &variant.module,
            entry_point: Some(abi::LIGHTING_PASS_VERTEX_ENTRY),
            // The fullscreen triangle comes from the vertex index; there is
            // nothing to bind.
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &variant.module,
            entry_point: Some(abi::LIGHTING_PASS_FRAGMENT_ENTRY),
            targets: &[opaque_target(format)],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        // No depth attachment: the pass samples depth instead, and a depth
        // texture cannot be attached and sampled in the same pass.
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

impl Pipeline for DeferredPipeline {
    fn path(&self) -> RenderPath {
        RenderPath::Deferred
    }

    fn configure(&mut self, device: &wgpu::Device, target: TargetConfig) {
        if self.target.map(|current| current.format) != Some(target.format) {
            self.lighting_pipelines.clear();
        }
        let size = (target.width, target.height);
        if self.gbuffer.as_ref().map(|gbuffer| gbuffer.size) != Some(size) {
            self.gbuffer = Some(GBuffer::new(
                device,
                &self.gbuffer_layout,
                target.width,
                target.height,
            ));
        }
        self.target = Some(target);
    }

    fn record(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput<'_>,
    ) -> Result<(), RenderError> {
        let target = self.target.ok_or(RenderError::NotConfigured)?;
        let lighting = input.lighting.ok_or(RenderError::NoLightingShader)?;
        let gbuffer = self.gbuffer.as_ref().ok_or(RenderError::NotConfigured)?;
        let gbuffer_targets: Vec<Option<wgpu::ColorTargetState>> =
            gbuffer_formats().into_iter().map(opaque_target).collect();

        {
            // Pass 1: run the material, write the G-buffer. Clearing to zero
            // matters for the depth-based background test in the lighting
            // pass, which is what keeps the clear colour visible.
            let attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = gbuffer
                .views
                .iter()
                .map(|view| {
                    Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })
                })
                .collect();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wesloom deferred material"),
                color_attachments: &attachments,
                depth_stencil_attachment: Some(gbuffer.depth.attachment()),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, input.bindings.bind_group(), &[]);
            for draw in input.draws {
                pass.set_pipeline(self.material_pipelines.get(
                    device,
                    draw.material,
                    &gbuffer_targets,
                ));
                draw.mesh.draw(&mut pass);
            }
        }

        {
            // Pass 2: shade every covered pixel from the G-buffer.
            let layout = &self.lighting_layout;
            let pipeline = self
                .lighting_pipelines
                .entry(lighting.key.identity)
                .or_insert_with(|| {
                    create_lighting_pipeline(device, layout, lighting, target.format)
                });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wesloom deferred lighting"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: input.target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(target.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, input.bindings.bind_group(), &[]);
            pass.set_bind_group(1, &gbuffer.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
