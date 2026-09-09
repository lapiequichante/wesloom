//! [`UiRenderer`]: submitting a [`DrawList`] as one pass.
//!
//! Compiles `package::wxsl::ui` from the application's shader library
//! through the same [`crate::variants::compile`] path a material takes
//! (ADR 0013), keeps one instance buffer, and records a batch as a scissor
//! rectangle plus a `draw(0..6, instances)`. There is no vertex buffer and no
//! index buffer: the quad's corners come from the vertex index, so an
//! instance is the entire per-primitive cost.
//!
//! Textures are registered up front and addressed by [`TextureId`]. The atlas
//! is always id 0 ([`TextureId::ATLAS`]); anything else — a material preview
//! rendered offscreen, an application's own image — is registered with
//! [`UiRenderer::register_texture`], which builds the bind group once rather
//! than per frame.

use std::borrow::Cow;

use wxsl_core::abi;
use wxsl_core::macros::MacroSet;

use crate::error::RenderError;
use crate::library::ShaderLibrary;
use crate::ui::atlas::Atlas;
use crate::ui::draw::{Color, DrawList, TextureId, UiInstance};
use crate::variants;

/// Where a [`DrawList`] is being drawn.
pub struct UiTarget<'a> {
    /// The colour attachment.
    pub view: &'a wgpu::TextureView,
    /// Its format. A change rebuilds the pipeline, so it is cheap to pass
    /// the same one every frame and correct to pass a new one after a
    /// surface reconfiguration.
    pub format: wgpu::TextureFormat,
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
    /// Colour to clear to, or `None` to draw over what is already there —
    /// which is what an overlay on top of a rendered scene wants.
    pub clear: Option<Color>,
}

/// One registered texture and the bind group that samples it.
struct Registered {
    bind_group: wgpu::BindGroup,
}

/// The UI pass.
pub struct UiRenderer {
    module: wgpu::ShaderModule,
    wgsl: String,
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    pipeline: Option<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    sampler: wgpu::Sampler,
    viewport: wgpu::Buffer,
    textures: Vec<Registered>,
    instances: wgpu::Buffer,
    capacity: usize,
}

impl UiRenderer {
    /// How many instances the buffer starts out able to hold.
    const INITIAL_CAPACITY: usize = 4096;

    /// Compile the UI shader from `library` and set up the pass.
    ///
    /// `atlas` is registered as [`TextureId::ATLAS`], so text works with no
    /// further setup. Fails if the library has no `package::wxsl::ui` — the
    /// application supplies the shaders (ADR 0009), and a missing one is
    /// better reported here than as an empty window.
    pub fn new(
        device: &wgpu::Device,
        library: &ShaderLibrary,
        atlas: &Atlas,
    ) -> Result<Self, RenderError> {
        if !library.contains(abi::UI_MODULE) {
            return Err(RenderError::MissingModule {
                module: abi::UI_MODULE.to_string(),
            });
        }
        let extra: [(&str, Cow<'_, str>); 0] = [];
        let wgsl = variants::compile(library, &extra, abi::UI_MODULE, &MacroSet::new())?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wxsl ui"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(wgsl.as_str())),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wxsl ui"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: abi::BINDING_UI_VIEWPORT,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: abi::BINDING_UI_TEXTURE,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: abi::BINDING_UI_SAMPLER,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        // Groups 0..2 are unbound: the UI pass has no camera, no material and
        // no per-frame scene. Everything it needs is a pass resource, which
        // is what group 3 is for (ADR 0010).
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wxsl ui"),
            bind_group_layouts: &[None, None, None, Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wxsl ui"),
            // Linear, and it matters: a distance field is *interpolated*
            // between texels and then thresholded, which is exactly what
            // keeps a glyph's edge smooth at any size. Nearest sampling
            // would give back the aliasing MSDF exists to remove.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let viewport = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wxsl ui viewport"),
            size: core::mem::size_of::<[f32; 4]>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wxsl ui instances"),
            size: (Self::INITIAL_CAPACITY * core::mem::size_of::<UiInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut renderer = UiRenderer {
            module,
            wgsl,
            bind_group_layout,
            pipeline_layout,
            pipeline: None,
            sampler,
            viewport,
            textures: Vec::new(),
            instances,
            capacity: Self::INITIAL_CAPACITY,
        };
        let atlas_id = renderer.register_texture(device, atlas.view());
        debug_assert_eq!(atlas_id, TextureId::ATLAS);
        Ok(renderer)
    }

    /// The WGSL the UI shader compiled to.
    ///
    /// Kept for the same reason a material variant's is: it is the only
    /// readable answer to what the GPU is running, and the editor can show
    /// it next to the material's.
    pub fn wgsl(&self) -> &str {
        &self.wgsl
    }

    /// Register a texture a draw list can sample, returning its id.
    pub fn register_texture(
        &mut self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
    ) -> TextureId {
        let bind_group = self.bind_group(device, view);
        self.textures.push(Registered { bind_group });
        TextureId(self.textures.len() as u32 - 1)
    }

    /// Point an existing id at a different texture.
    ///
    /// What a resized offscreen preview needs: the draw list keeps referring
    /// to the same id, and the bind group behind it is rebuilt.
    ///
    /// # Panics
    ///
    /// Panics on an id this renderer never handed out.
    pub fn replace_texture(
        &mut self,
        device: &wgpu::Device,
        id: TextureId,
        view: &wgpu::TextureView,
    ) {
        let bind_group = self.bind_group(device, view);
        self.textures[id.0 as usize] = Registered { bind_group };
    }

    /// How many textures are registered.
    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }

    fn bind_group(&self, device: &wgpu::Device, view: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wxsl ui"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_UI_VIEWPORT,
                    resource: self.viewport.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_UI_TEXTURE,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_UI_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// The pipeline for `format`, built on first use and after a change.
    fn pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> &wgpu::RenderPipeline {
        if self.pipeline.as_ref().map(|(cached, _)| *cached) != Some(format) {
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("wxsl ui"),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.module,
                    entry_point: Some(abi::UI_VERTEX_ENTRY),
                    // One instance-stepped buffer, and no per-vertex buffer
                    // at all: the corners come from the vertex index.
                    buffers: &[Some(UiInstance::LAYOUT)],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.module,
                    entry_point: Some(abi::UI_FRAGMENT_ENTRY),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        // Straight (non-premultiplied) alpha over: the
                        // shader returns a tint and a coverage, and the UI
                        // is composited in the target's own colour space.
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::SrcAlpha,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    // No culling: a capsule's quad can be wound either way
                    // depending on which direction its axis points.
                    cull_mode: None,
                    ..Default::default()
                },
                // The interface is painted back to front in submission
                // order, so there is nothing for a depth buffer to do.
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
            self.pipeline = Some((format, pipeline));
        }
        &self.pipeline.as_ref().expect("just built").1
    }

    /// Upload `list` and record it into `encoder` as one pass.
    ///
    /// Does nothing but the clear when the list is empty, so a frame with no
    /// interface still leaves a well-defined target.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &UiTarget<'_>,
        list: &DrawList,
    ) -> Result<(), RenderError> {
        let width = target.width.max(1);
        let height = target.height.max(1);
        queue.write_buffer(
            &self.viewport,
            0,
            bytemuck::cast_slice(&[width as f32, height as f32, 0.0, 0.0]),
        );

        let instances = list.instances();
        if instances.len() > self.capacity {
            // Grow in powers of two, so a UI that gets steadily busier does
            // not reallocate every frame.
            self.capacity = instances.len().next_power_of_two();
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wxsl ui instances"),
                size: (self.capacity * core::mem::size_of::<UiInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !instances.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(instances));
        }

        let load = match target.clear {
            Some(color) => wgpu::LoadOp::Clear(wgpu::Color {
                r: f64::from(color.r),
                g: f64::from(color.g),
                b: f64::from(color.b),
                a: f64::from(color.a),
            }),
            None => wgpu::LoadOp::Load,
        };
        // Borrow the pipeline before the pass, so the pass can hold `&self`
        // state without conflicting with the `&mut self` build.
        let format = target.format;
        self.pipeline(device, format);
        let (_, pipeline) = self.pipeline.as_ref().expect("built above");

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("wxsl ui"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if list.is_empty() {
            return Ok(());
        }
        pass.set_pipeline(pipeline);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        for batch in list.batches() {
            let Some(registered) = self.textures.get(batch.texture.0 as usize) else {
                // An id from another renderer, or one never registered.
                // Skipping is the only honest option inside a pass, and it
                // is a programming error rather than a runtime condition.
                debug_assert!(
                    false,
                    "draw list refers to unregistered {:?}",
                    batch.texture
                );
                continue;
            };
            // A scissor rectangle has to be inside the attachment, and
            // integral: a clip of half a pixel is a validation error, not a
            // soft edge.
            let x0 = batch.clip.min.x.floor().clamp(0.0, width as f32) as u32;
            let y0 = batch.clip.min.y.floor().clamp(0.0, height as f32) as u32;
            let x1 = batch.clip.max.x.ceil().clamp(0.0, width as f32) as u32;
            let y1 = batch.clip.max.y.ceil().clamp(0.0, height as f32) as u32;
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
            pass.set_bind_group(abi::GROUP_PASS, &registered.bind_group, &[]);
            pass.draw(0..abi::UI_QUAD_VERTICES, batch.instances.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::draw::Rect;
    use glam::Vec2;

    #[test]
    fn a_scissor_rectangle_is_clamped_to_the_attachment() {
        // The arithmetic `render` does, checked without a device: a clip that
        // hangs off the target is a `wgpu` validation error, not a clipped
        // rectangle, so this cannot be got wrong quietly.
        let clamp = |clip: Rect, width: u32, height: u32| {
            let x0 = clip.min.x.floor().clamp(0.0, width as f32) as u32;
            let y0 = clip.min.y.floor().clamp(0.0, height as f32) as u32;
            let x1 = clip.max.x.ceil().clamp(0.0, width as f32) as u32;
            let y1 = clip.max.y.ceil().clamp(0.0, height as f32) as u32;
            (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
        };

        assert_eq!(
            clamp(Rect::new(10.0, 20.0, 30.0, 40.0), 100, 100),
            (10, 20, 30, 40)
        );
        // Hanging off every side.
        assert_eq!(
            clamp(
                Rect::from_min_max(Vec2::splat(-50.0), Vec2::splat(500.0)),
                100,
                80
            ),
            (0, 0, 100, 80)
        );
        // Entirely outside: an empty rectangle, which `render` skips.
        let (_, _, w, h) = clamp(Rect::new(200.0, 200.0, 10.0, 10.0), 100, 100);
        assert!(w == 0 || h == 0);
        // Sub-pixel clips round outwards, so nothing is lost to rounding.
        assert_eq!(clamp(Rect::new(0.2, 0.2, 0.5, 0.5), 100, 100), (0, 0, 1, 1));
    }

    #[test]
    fn the_instance_buffer_grows_in_powers_of_two() {
        // The rule `render` applies, so a UI that gets busier does not
        // reallocate every frame.
        let grow = |needed: usize, capacity: usize| {
            if needed > capacity {
                needed.next_power_of_two()
            } else {
                capacity
            }
        };
        assert_eq!(
            grow(100, UiRenderer::INITIAL_CAPACITY),
            UiRenderer::INITIAL_CAPACITY
        );
        assert_eq!(grow(5000, 4096), 8192);
        assert_eq!(grow(8193, 8192), 16384);
    }
}
