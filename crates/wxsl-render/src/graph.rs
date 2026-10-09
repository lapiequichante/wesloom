//! wgpu resource allocation and recording for shared frame plans (ADR 0048).

use crate::error::RenderError;
use crate::pass::{
    Attachment, DepthAttachment, Load, PassDesc, PassKind, ResourceId, ResourceShape,
};
use crate::pipeline::TargetConfig;
use crate::types::{texture_dimension, view_dimension, WgpuType};
use std::collections::HashMap;
use wxsl_core::abi;

pub use wxsl_frame::graph::{Allocation, GraphError, Schedule, SlotDesc, SlotShape};

/// A shared frame plan with wgpu recording operations.
#[derive(Clone, Debug)]
pub struct RenderGraph(wxsl_frame::graph::RenderGraph);

impl RenderGraph {
    /// The caller-supplied frame target.
    pub const TARGET: ResourceId = wxsl_frame::graph::RenderGraph::TARGET;

    /// An empty graph using the native target format.
    pub fn new(format: wgpu::TextureFormat) -> Self {
        Self(wxsl_frame::graph::RenderGraph::new(
            wxsl_frame::types::TextureFormat::from_wgpu(format),
        ))
    }

    /// Set the layout the geometry pass is checked against.
    pub fn with_gbuffer_layout(self, layout: Vec<abi::GBufferTarget>) -> Self {
        Self(self.0.with_gbuffer_layout(layout))
    }
}

impl From<wxsl_frame::graph::RenderGraph> for RenderGraph {
    fn from(graph: wxsl_frame::graph::RenderGraph) -> Self {
        Self(graph)
    }
}

impl From<RenderGraph> for wxsl_frame::graph::RenderGraph {
    fn from(graph: RenderGraph) -> Self {
        graph.0
    }
}

impl std::ops::Deref for RenderGraph {
    type Target = wxsl_frame::graph::RenderGraph;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for RenderGraph {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// The textures a [`Schedule`] asked for.
///
/// Reallocated when the target's size changes or the schedule does, and not
/// otherwise: resizing a window must not leak a texture per frame.
pub struct ResourcePool {
    slots: Vec<Slot>,
    layout: Vec<SlotDesc>,
    target: Option<TargetConfig>,
    // Keyed on what the bind group *is* — the shapes, the physical slots
    // and, when the pass has effect parameters, its label's hash — rather
    // than on which pass asked for it: two passes that agree on all of it
    // would build the identical bind group, and a pass index would go
    // stale the moment the pass list changed.
    #[allow(clippy::type_complexity)]
    bind_groups: HashMap<(Vec<PassBinding>, Vec<usize>, Option<u64>), wgpu::BindGroup>,
    bind_layouts: HashMap<Vec<PassBinding>, wgpu::BindGroupLayout>,
    frame: u64,
    generation: u64,
}

enum Slot {
    Texture {
        texture: wgpu::Texture,
        view: wgpu::TextureView,
    },
    Buffer {
        buffer: wgpu::Buffer,
    },
}

/// The shape of one entry of a pass bind group.
///
/// Two passes whose reads and writes have the same shapes can share a
/// bind group *layout*, and therefore a pipeline layout — which is why
/// this is what the caches are keyed on rather than "does this pass have
/// a pass group", a question two passes can answer the same way while
/// wanting different layouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PassBinding {
    /// A texture read — sampled or loaded, whole-resource.
    Texture {
        /// How the texture is sampled.
        sample_type: wgpu::TextureSampleType,
        /// The view's dimension.
        view_dimension: wgpu::TextureViewDimension,
    },
    /// A storage texture the pass writes — a compute effect's output,
    /// write-only (plan2 P10).
    StorageTexture {
        /// The texel format, which the bind group layout has to name.
        format: wgpu::TextureFormat,
        /// The view's dimension.
        view_dimension: wgpu::TextureViewDimension,
    },
    /// A storage buffer read (`read_only`) or written by the pass (plan2
    /// P11). The size rides along because the bind group layout's
    /// `min_binding_size` is part of the layout's identity.
    Buffer {
        /// Whether the pass only reads it.
        read_only: bool,
        /// The buffer's size in bytes.
        size: u64,
    },
    /// A uniform block of the pass's effect parameters (plan3 N4), bound
    /// after the reads and the writes — the next binding the effect's
    /// shader reaches. The buffer behind it is not a pooled resource: its
    /// *contents* are host state that changes without the pass list
    /// changing, so the caller hands the actual buffer in and it joins
    /// this shape only as a size.
    Uniform {
        /// The block's size in bytes, already a multiple of 16.
        size: u32,
    },
}

impl Default for ResourcePool {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourcePool {
    /// An empty pool.
    pub fn new() -> Self {
        ResourcePool {
            slots: Vec::new(),
            layout: Vec::new(),
            target: None,
            bind_groups: HashMap::new(),
            bind_layouts: HashMap::new(),
            frame: 0,
            generation: 0,
        }
    }

    /// How many times the pool has actually reallocated.
    ///
    /// What anything holding a view of a slot watches: the views a caller
    /// took out are stale exactly when this has moved on, and no more
    /// often — [`ResourcePool::configure`] runs every frame and usually
    /// does nothing.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The whole-resource view of one slot, for binding it as a texture.
    pub fn slot_view(&self, slot: usize) -> Option<&wgpu::TextureView> {
        match self.slots.get(slot) {
            Some(Slot::Texture { view, .. }) => Some(view),
            _ => None,
        }
    }

    /// The frame counter the ring rotation is taken modulo.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Create whatever `schedule` needs at `target`'s size, if it is not
    /// already there. Cheap and idempotent, so it is safe every frame.
    pub fn configure(&mut self, device: &wgpu::Device, schedule: &Schedule, target: TargetConfig) {
        let same_size = self.target.is_some_and(|current| {
            (current.width, current.height) == (target.width, target.height)
        });
        if same_size && self.layout == schedule.slots() {
            self.target = Some(target);
            return;
        }

        self.slots.clear();
        self.bind_groups.clear();
        for desc in schedule.slots() {
            let slot = match desc.shape {
                SlotShape::Texture {
                    extent,
                    dimension,
                    layers,
                    format,
                    usage,
                } => {
                    let (width, height) = extent.resolve(target.width, target.height);
                    let texture = device.create_texture(&wgpu::TextureDescriptor {
                        label: Some(&format!("wxsl {}", desc.label)),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: layers,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: texture_dimension(dimension),
                        format: format.to_wgpu(),
                        usage: usage.to_wgpu(),
                        view_formats: &[],
                    });
                    let view = texture.create_view(&wgpu::TextureViewDescriptor {
                        dimension: Some(view_dimension(dimension)),
                        ..Default::default()
                    });
                    Slot::Texture { texture, view }
                }
                SlotShape::Buffer { size, usage } => {
                    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some(&format!("wxsl {}", desc.label)),
                        size,
                        usage: usage.to_wgpu() | wgpu::BufferUsages::STORAGE,
                        mapped_at_creation: false,
                    });
                    Slot::Buffer { buffer }
                }
            };
            self.slots.push(slot);
        }
        self.layout = schedule.slots().to_vec();
        self.target = Some(target);
        self.generation += 1;
    }

    /// The texture serving one slot, if the pool has been configured and
    /// the slot is a texture.
    ///
    /// The escape hatch for anything the graph does not do itself: reading
    /// a target back, or handing it to another system.
    pub fn texture(&self, slot: usize) -> Option<&wgpu::Texture> {
        match self.slots.get(slot) {
            Some(Slot::Texture { texture, .. }) => Some(texture),
            _ => None,
        }
    }

    /// The buffer serving one slot, if the slot is a buffer — the same
    /// escape hatch, for reading a compute pass's output back.
    pub fn buffer(&self, slot: usize) -> Option<&wgpu::Buffer> {
        match self.slots.get(slot) {
            Some(Slot::Buffer { buffer }) => Some(buffer),
            _ => None,
        }
    }

    /// The view of one slot, for a whole-resource binding.
    fn view(&self, slot: usize) -> &wgpu::TextureView {
        match &self.slots[slot] {
            Slot::Texture { view, .. } => view,
            Slot::Buffer { .. } => panic!("a buffer has no texture view"),
        }
    }

    /// The buffer of one slot, for a pass-group binding.
    fn slot_buffer(&self, slot: usize) -> &wgpu::Buffer {
        match &self.slots[slot] {
            Slot::Buffer { buffer } => buffer,
            Slot::Texture { .. } => panic!("a texture has no buffer"),
        }
    }

    /// A view of one layer of a slot, for an attachment.
    ///
    /// A cube face or a shadow cascade is rendered into one layer at a
    /// time; a plain 2D target's layer 0 is the whole texture, and reuses
    /// the view that already exists.
    fn attachment_view(&self, slot: usize, layer: u32) -> wgpu::TextureView {
        let Slot::Texture { texture, view } = &self.slots[slot] else {
            panic!("attachments are textures; the scheduler checks")
        };
        if layer == 0 && texture.depth_or_array_layers() == 1 {
            return view.clone();
        }
        texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: layer,
            array_layer_count: Some(1),
            ..Default::default()
        })
    }
}

/// The pass currently being recorded, and what the caller needs to build a
/// pipeline for it.
pub struct RecordedPass<'a> {
    /// Resolved argument buffer for an indirect geometry pass.
    pub indirect_buffer: Option<wgpu::Buffer>,
    /// The description this pass came from.
    pub desc: &'a PassDesc,
    /// Its index in the graph's declaration order, which is how a caller
    /// looks up whatever it precomputed per pass.
    pub index: usize,
    /// Colour target formats, in attachment order — the other half of a
    /// pipeline's identity, alongside [`crate::pass::PassState`].
    pub color_formats: Vec<Option<wgpu::ColorTargetState>>,
    /// Layout of the pass bind group, or `None` when the pass reads nothing.
    pub pass_layout: Option<wgpu::BindGroupLayout>,
    /// The shape of that layout, for keying a pipeline built against it.
    pub pass_bindings: Vec<PassBinding>,
    /// The pass bind group itself, already built from this frame's slots.
    pub pass_bind_group: Option<wgpu::BindGroup>,
}

/// Whichever kind of `wgpu` pass the graph opened.
pub enum PassEncoder<'a, 'b> {
    /// A render pass, for [`PassKind::Geometry`] and [`PassKind::Screen`].
    Render(&'a mut wgpu::RenderPass<'b>),
    /// A compute pass, for [`PassKind::Compute`].
    Compute(&'a mut wgpu::ComputePass<'b>),
}

impl RenderGraph {
    /// Record every pass in `schedule` order into `encoder`.
    ///
    /// `imports` supplies a view for each imported resource — at minimum
    /// [`RenderGraph::TARGET`]. `run` decides, per pass (by declaration
    /// index), whether the pass runs this frame at all — the execution
    /// policies' hook (plan2 P10): a pass it returns `false` for is not
    /// recorded and leaves whatever its last run wrote behind, which the
    /// scheduler's stable-storage rule makes safe. `params` hands over the
    /// pass's effect-parameter block — the uniform buffer and its size —
    /// for a pass whose effect declares parameters; the graph knows
    /// neither effects nor their buffers, so the caller resolves both.
    /// `body` issues the actual work: the graph has opened the pass,
    /// resolved its attachments and bound nothing, because which bind
    /// groups a draw needs is the caller's business, not the scheduler's.
    #[allow(clippy::too_many_arguments)]
    pub fn record<'s, F>(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        schedule: &Schedule,
        pool: &mut ResourcePool,
        imports: &[(ResourceId, &wgpu::TextureView)],
        run: &dyn Fn(usize) -> bool,
        // The buffer a pass's parameters live in borrows from the
        // *caller's* state, so its lifetime rides on this call's borrow of
        // `params`, not on the pass description's.
        params: &(dyn Fn(&PassDesc) -> Option<(&'s wgpu::Buffer, u32)> + 's),
        mut body: F,
    ) -> Result<(), RenderError>
    where
        F: FnMut(&RecordedPass<'_>, PassEncoder<'_, '_>) -> Result<(), RenderError>,
    {
        let frame = pool.frame;
        let import = |id: ResourceId| -> Result<wgpu::TextureView, RenderError> {
            imports
                .iter()
                .find(|(resource, _)| *resource == id)
                .map(|(_, view)| (*view).clone())
                .ok_or_else(|| RenderError::MissingImport {
                    resource: self.resources()[id.index()].label.clone(),
                })
        };
        for &index in schedule.order() {
            if !run(index) {
                continue;
            }
            let pass = &self.passes()[index];

            // The pass bind group: the resources it reads, in order, at
            // binding 0..n of `abi::GROUP_PASS`. The G-buffer the deferred
            // lighting pass samples is exactly this, and so is a screen
            // effect's input; there is nothing pass-specific left to write.
            let (pass_bindings, pass_layout, pass_bind_group) =
                self.pass_group(device, pool, schedule, pass, params(pass), imports)?;

            let color_formats: Vec<Option<wgpu::ColorTargetState>> = pass
                .color
                .iter()
                .map(|attachment| {
                    let format = match self.resources()[attachment.resource.index()].shape {
                        ResourceShape::Texture { format, .. } => format,
                        ResourceShape::Buffer { .. } => {
                            return Err(RenderError::Graph(GraphError::AttachmentNotATexture {
                                pass: pass.label.clone(),
                                resource: self.resources()[attachment.resource.index()]
                                    .label
                                    .clone(),
                            }));
                        }
                    };
                    Ok(Some(wgpu::ColorTargetState {
                        format: format.to_wgpu(),
                        blend: attachment.blend.or(pass.state.blend).map(WgpuType::to_wgpu),
                        write_mask: wgpu::ColorWrites::ALL,
                    }))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let indirect_buffer = pass
                .indirect_buffer()
                .map(|id| {
                    schedule
                        .slot(id, frame, 0)
                        .and_then(|slot| pool.buffer(slot))
                        .cloned()
                        .ok_or_else(|| RenderError::MissingImport {
                            resource: self.resources()[id.index()].label.clone(),
                        })
                })
                .transpose()?;
            let recorded = RecordedPass {
                indirect_buffer,
                desc: pass,
                index,
                pass_bindings,
                color_formats,
                pass_layout,
                pass_bind_group,
            };

            match &pass.kind {
                PassKind::Compute { .. } => {
                    let mut compute = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some(&pass.label),
                        timestamp_writes: None,
                    });
                    body(&recorded, PassEncoder::Compute(&mut compute))?;
                }
                PassKind::Geometry { .. } | PassKind::Screen { .. } => {
                    let color_views = pass
                        .color
                        .iter()
                        .map(|attachment| {
                            self.attachment_view(attachment, pool, schedule, frame, &import)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let depth_view = match pass.depth {
                        Some(depth) => {
                            Some(self.depth_view(depth, pool, schedule, frame, &import)?)
                        }
                        None => None,
                    };
                    let color_attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = pass
                        .color
                        .iter()
                        .zip(&color_views)
                        .map(|(attachment, view)| {
                            Some(wgpu::RenderPassColorAttachment {
                                view,
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: match attachment.load {
                                        Load::Clear(color) => wgpu::LoadOp::Clear(color.to_wgpu()),
                                        Load::Load => wgpu::LoadOp::Load,
                                    },
                                    store: store_op(attachment.store),
                                },
                            })
                        })
                        .collect();
                    let depth_attachment =
                        pass.depth.zip(depth_view.as_ref()).map(|(depth, view)| {
                            wgpu::RenderPassDepthStencilAttachment {
                                view,
                                depth_ops: Some(wgpu::Operations {
                                    load: match depth.clear {
                                        Some(value) => wgpu::LoadOp::Clear(value),
                                        None => wgpu::LoadOp::Load,
                                    },
                                    store: store_op(depth.store),
                                }),
                                stencil_ops: None,
                            }
                        });
                    let mut render = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some(&pass.label),
                        color_attachments: &color_attachments,
                        depth_stencil_attachment: depth_attachment,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    body(&recorded, PassEncoder::Render(&mut render))?;
                }
            }
        }
        pool.frame = pool.frame.wrapping_add(1);
        Ok(())
    }

    fn attachment_view(
        &self,
        attachment: &Attachment,
        pool: &ResourcePool,
        schedule: &Schedule,
        frame: u64,
        import: &impl Fn(ResourceId) -> Result<wgpu::TextureView, RenderError>,
    ) -> Result<wgpu::TextureView, RenderError> {
        match schedule.slot(attachment.resource, frame, 0) {
            Some(slot) => Ok(pool.attachment_view(slot, attachment.layer)),
            None => import(attachment.resource),
        }
    }

    fn depth_view(
        &self,
        depth: DepthAttachment,
        pool: &ResourcePool,
        schedule: &Schedule,
        frame: u64,
        import: &impl Fn(ResourceId) -> Result<wgpu::TextureView, RenderError>,
    ) -> Result<wgpu::TextureView, RenderError> {
        match schedule.slot(depth.resource, frame, 0) {
            Some(slot) => Ok(pool.attachment_view(slot, depth.layer)),
            None => import(depth.resource),
        }
    }

    /// The pass group's binding shapes, in binding order: every read, then
    /// every non-attachment write, then — when the pass's effect declares
    /// parameters — the uniform block at the next binding. Device-free,
    /// because a shape is data: this is what the layout, the bind group
    /// and the pipeline caches are all keyed on.
    pub fn pass_binding_kinds(&self, pass: &PassDesc, params: Option<u32>) -> Vec<PassBinding> {
        let mut kinds: Vec<PassBinding> = Vec::with_capacity(
            pass.reads.len() + pass.writes.len() + usize::from(params.is_some()),
        );
        for read in &pass.reads {
            kinds.push(match self.resources()[read.resource.index()].shape {
                ResourceShape::Texture {
                    format, dimension, ..
                } => PassBinding::Texture {
                    sample_type: format
                        .to_wgpu()
                        .sample_type(None, None)
                        .unwrap_or(wgpu::TextureSampleType::Float { filterable: true }),
                    view_dimension: view_dimension(dimension),
                },
                ResourceShape::Buffer { size, .. } => PassBinding::Buffer {
                    read_only: true,
                    size,
                },
            });
        }
        for write in &pass.writes {
            kinds.push(match self.resources()[write.index()].shape {
                ResourceShape::Texture {
                    format, dimension, ..
                } => PassBinding::StorageTexture {
                    format: format.to_wgpu(),
                    view_dimension: view_dimension(dimension),
                },
                ResourceShape::Buffer { size, .. } => PassBinding::Buffer {
                    read_only: false,
                    size,
                },
            });
        }
        if let Some(size) = params {
            kinds.push(PassBinding::Uniform { size });
        }
        kinds
    }

    /// Build (or reuse) the pass group's layout and bind group.
    ///
    /// The group is the pass's declared contract in binding order: every
    /// read (a texture or, later, a buffer), then every non-attachment
    /// write (a compute effect's storage target), then the pass's effect
    /// parameters as one uniform block when it declares any. Effects
    /// declare their inputs, outputs and parameters in the same order,
    /// which is what makes the shader and the group meet without either
    /// knowing the other (plan2 P4/P10, plan3 N4).
    #[allow(clippy::type_complexity)]
    fn pass_group(
        &self,
        device: &wgpu::Device,
        pool: &mut ResourcePool,
        schedule: &Schedule,
        pass: &PassDesc,
        params: Option<(&wgpu::Buffer, u32)>,
        imports: &[(ResourceId, &wgpu::TextureView)],
    ) -> Result<
        (
            Vec<PassBinding>,
            Option<wgpu::BindGroupLayout>,
            Option<wgpu::BindGroup>,
        ),
        RenderError,
    > {
        let params_key = params.map(|(_, _)| wxsl_core::wxsl::stable_hash(pass.label.as_bytes()));
        if pass.reads.is_empty() && pass.writes.is_empty() && params.is_none() {
            return Ok((Vec::new(), None, None));
        }
        let kinds = self.pass_binding_kinds(pass, params.map(|(_, size)| size));

        let layout = pool
            .bind_layouts
            .entry(kinds.clone())
            .or_insert_with(|| {
                let entries: Vec<wgpu::BindGroupLayoutEntry> = kinds
                    .iter()
                    .enumerate()
                    .map(|(binding, kind)| {
                        // Reads are visible to everything a pass can run;
                        // writes are compute-only, because a render pass
                        // writing storage mid-draw is not expressible.
                        let (visibility, ty) = match *kind {
                            PassBinding::Texture {
                                sample_type,
                                view_dimension,
                            } => (
                                wgpu::ShaderStages::VERTEX_FRAGMENT | wgpu::ShaderStages::COMPUTE,
                                wgpu::BindingType::Texture {
                                    sample_type,
                                    view_dimension,
                                    multisampled: false,
                                },
                            ),
                            PassBinding::StorageTexture {
                                format,
                                view_dimension,
                            } => (
                                wgpu::ShaderStages::COMPUTE,
                                wgpu::BindingType::StorageTexture {
                                    access: wgpu::StorageTextureAccess::WriteOnly,
                                    format,
                                    view_dimension,
                                },
                            ),
                            PassBinding::Buffer { read_only, size } => (
                                if read_only {
                                    wgpu::ShaderStages::VERTEX_FRAGMENT
                                        | wgpu::ShaderStages::COMPUTE
                                } else {
                                    wgpu::ShaderStages::COMPUTE
                                },
                                wgpu::BindingType::Buffer {
                                    ty: wgpu::BufferBindingType::Storage { read_only },
                                    has_dynamic_offset: false,
                                    min_binding_size: wgpu::BufferSize::new(size),
                                },
                            ),
                            // The effect parameters: written by the host
                            // between frames, read by whatever stage the
                            // effect runs — so visible to both, unlike a
                            // storage write which only compute may do.
                            PassBinding::Uniform { size } => (
                                wgpu::ShaderStages::VERTEX_FRAGMENT | wgpu::ShaderStages::COMPUTE,
                                wgpu::BindingType::Buffer {
                                    ty: wgpu::BufferBindingType::Uniform,
                                    has_dynamic_offset: false,
                                    min_binding_size: wgpu::BufferSize::new(u64::from(size)),
                                },
                            ),
                        };
                        wgpu::BindGroupLayoutEntry {
                            binding: binding as u32,
                            visibility,
                            ty,
                            count: None,
                        }
                    })
                    .collect();
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("wxsl pass"),
                    entries: &entries,
                })
            })
            .clone();

        // Reads go to their ring slot at their history; writes to this
        // frame's slot of what they write — unless the resource is
        // imported, which has no slot because nobody here allocated it:
        // it binds the view the host handed over (a bake table above all,
        // ADR 0045). The parameter block has no slot either — its buffer
        // came in with the pass.
        // Reads go to their ring slot at their history; writes to this
        // frame's slot of what they write — unless the resource is
        // imported, which has no slot because nobody here allocated it: it
        // binds the view the host handed over (a bake table above all,
        // ADR 0045). The parameter block has no slot either — its buffer
        // came in with the pass.
        let imported_view = |resource: ResourceId| -> Option<&wgpu::TextureView> {
            self.resources()[resource.index()]
                .imported
                .then(|| {
                    imports
                        .iter()
                        .find(|(imported, _)| *imported == resource)
                        .map(|(_, view)| *view)
                })
                .flatten()
        };
        let mut sources: Vec<Result<usize, &wgpu::TextureView>> =
            Vec::with_capacity(pass.reads.len() + pass.writes.len());
        let mut any_imported = false;
        for read in &pass.reads {
            match imported_view(read.resource) {
                Some(view) => {
                    any_imported = true;
                    sources.push(Err(view));
                }
                None => match schedule.slot(read.resource, pool.frame, read.history) {
                    Some(slot) => sources.push(Ok(slot)),
                    None => {
                        return Err(RenderError::MissingImport {
                            resource: pass.label.clone(),
                        })
                    }
                },
            }
        }
        for write in &pass.writes {
            match imported_view(*write) {
                Some(view) => {
                    any_imported = true;
                    sources.push(Err(view));
                }
                None => match schedule.slot(*write, pool.frame, 0) {
                    Some(slot) => sources.push(Ok(slot)),
                    None => {
                        return Err(RenderError::MissingImport {
                            resource: pass.label.clone(),
                        })
                    }
                },
            }
        }
        // The cache key carries slots only, and a slot cannot name a view
        // the host owns — so a group holding an import is built fresh
        // whenever it is due. That is cheap by construction: the group is
        // only built when the pass runs, and a bake runs once or on
        // demand. Every other pass is keyed as before: two passes with
        // the same shapes and the same slots but different parameter
        // buffers must not share a group, and the label — unique per pass,
        // the name its author knows — is what tells them apart.
        let slots: Vec<usize> = sources
            .iter()
            .map(|source| source.as_ref().copied().unwrap_or(usize::MAX))
            .collect();
        let key = (kinds.clone(), slots, params_key);
        if !any_imported {
            if let Some(existing) = pool.bind_groups.get(&key) {
                return Ok((kinds, Some(layout), Some(existing.clone())));
            }
        }
        let mut entries: Vec<wgpu::BindGroupEntry> = Vec::with_capacity(kinds.len());
        let mut sources = sources.into_iter();
        for (binding, kind) in kinds.iter().enumerate() {
            // The binding's kind decides how the entry is bound — and the
            // kind is where the pass declared it, so the two cannot
            // disagree. Only the parameter block binds something that did
            // not come from a slot or an import.
            let resource = match *kind {
                PassBinding::Texture { .. } | PassBinding::StorageTexture { .. } => {
                    match sources.next().expect("a source per texture binding") {
                        Ok(slot) => wgpu::BindingResource::TextureView(pool.view(slot)),
                        Err(view) => wgpu::BindingResource::TextureView(view),
                    }
                }
                PassBinding::Buffer { .. } => wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: pool.slot_buffer(
                        sources
                            .next()
                            .expect("a source per buffer binding")
                            .expect("a buffer source is a slot"),
                    ),
                    offset: 0,
                    size: None,
                }),
                PassBinding::Uniform { .. } => {
                    let (buffer, _) = params.expect("a buffer per uniform binding");
                    wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer,
                        offset: 0,
                        size: None,
                    })
                }
            };
            entries.push(wgpu::BindGroupEntry {
                binding: binding as u32,
                resource,
            });
        }
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&pass.label),
            layout: &layout,
            entries: &entries,
        });
        pool.bind_groups.insert(key, bind_group.clone());
        Ok((kinds, Some(layout), Some(bind_group)))
    }
}

fn store_op(store: bool) -> wgpu::StoreOp {
    if store {
        wgpu::StoreOp::Store
    } else {
        wgpu::StoreOp::Discard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::Read;

    const COLOR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

    #[test]
    fn an_effect_parameter_block_joins_the_group_after_the_reads_and_writes() {
        // The descriptor's order — inputs, outputs, parameters — is the
        // binding order, so the uniform block lands at the next binding
        // after whatever the pass reads and writes, and its size is the
        // layout's (plan3 N4).
        let mut graph = RenderGraph::new(COLOR);
        let image = graph.resource(crate::pass::ResourceDesc::color(
            "image",
            wxsl_frame::types::TextureFormat::from_wgpu(COLOR),
        ));
        let pass = PassDesc::screen("bloom", "bloom").with_reads([Read::current(image)]);

        let plain = graph.pass_binding_kinds(&pass, None);
        assert_eq!(plain.len(), 1, "no parameters, no block");
        assert!(matches!(plain[0], PassBinding::Texture { .. }));

        let tuned = graph.pass_binding_kinds(&pass, Some(16));
        assert_eq!(tuned[0], plain[0], "the block must not renumber the reads");
        assert_eq!(tuned[1], PassBinding::Uniform { size: 16 });
    }
}
