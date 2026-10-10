//! Device-free graph validation, scheduling and resource allocation (ADR 0048).
//!
//! History reads create no ordering edge. Transient lifetimes may share a
//! physical slot; persistent resources rotate through a ring. The wgpu
//! backend consumes this plan to allocate and record a frame.

use std::collections::HashMap;
use std::fmt;

#[cfg(test)]
use crate::pass::DepthAttachment;
use crate::pass::{
    Dimension, DrawSource, Extent, PassDesc, PassKind, Persistence, Policy, ResourceDesc,
    ResourceId, ResourceShape,
};
#[cfg(test)]
use crate::types::Color;
use crate::types::{BufferUsages, TextureFormat, TextureUsages};
use wxsl_core::abi;

/// A pipeline, as a list of passes over a set of resources.
#[derive(Clone, Debug, serde::Serialize)]
pub struct RenderGraph {
    resources: Vec<ResourceDesc>,
    passes: Vec<PassDesc>,
    shadow_maps: Option<ResourceId>,
    environment_maps: Option<[ResourceId; 2]>,
    environment_scale: f32,
    /// The G-buffer layout this graph's `gbuffer`-stage passes are built
    /// for: what the scheduler checks a material pass's attachment count
    /// against. The base targets unless the graph says otherwise — see
    /// `RenderGraph::with_gbuffer_layout`, which a pipeline built from a
    /// lighting-model set calls
    /// (`wxsl_core::lighting::LightingSet::gbuffer_layout`).
    gbuffer_layout: Vec<abi::GBufferTarget>,
}

impl RenderGraph {
    /// The frame's own target, which every graph has and nobody allocates.
    pub const TARGET: ResourceId = ResourceId(0);

    /// An empty graph whose target is in `format`.
    pub fn new(format: TextureFormat) -> Self {
        RenderGraph {
            resources: vec![ResourceDesc::imported("target", format)],
            passes: Vec::new(),
            shadow_maps: None,
            environment_maps: None,
            environment_scale: 1.0,
            gbuffer_layout: abi::GBUFFER_BASE_TARGETS.to_vec(),
        }
    }

    /// Look up a resource description.
    pub fn resource_desc(&self, id: ResourceId) -> Option<&ResourceDesc> {
        self.resources.get(id.index())
    }

    /// Make `resource` stable storage: persistent, keeping no history.
    ///
    /// What a pass whose policy is not `per frame` needs its target to be
    /// (plan2 P10) — the scheduler rejects anything less, because a
    /// transient's slot is reused within a frame and an off-frame read of
    /// a rotating ring is of some other frame's bake. The pipeline
    /// compiler calls this when a policy'd pass writes a chain's colour
    /// target: documents carry less than pass lists on purpose, and this
    /// is a derivation, not a new knob.
    pub fn make_stable_storage(&mut self, resource: ResourceId) {
        if let Some(desc) = self.resources.get_mut(resource.index()) {
            desc.persistence = Persistence::Persistent { history: 0 };
        }
    }
    ///
    /// Without this, a `gbuffer`-stage pass is checked against the ABI's
    /// base targets; with it, against exactly what the enabled set
    /// requests — which is what keeps the scheduler's target-count check
    /// exact once that count stops being fixed.
    pub fn with_gbuffer_layout(mut self, layout: Vec<abi::GBufferTarget>) -> Self {
        self.gbuffer_layout = layout;
        self
    }

    /// The G-buffer layout this graph was built for.
    pub fn gbuffer_layout(&self) -> &[abi::GBufferTarget] {
        &self.gbuffer_layout
    }

    /// Declare a resource, returning the id passes refer to it by.
    pub fn resource(&mut self, desc: ResourceDesc) -> ResourceId {
        self.resources.push(desc);
        ResourceId(self.resources.len() as u32 - 1)
    }

    /// Add a pass. Declaration order is only a tie-break: the schedule
    /// orders passes by what they read and write.
    pub fn pass(&mut self, desc: PassDesc) -> &mut Self {
        self.passes.push(desc);
        self
    }

    /// The declared resources, indexed by [`ResourceId::index`].
    pub fn resources(&self) -> &[ResourceDesc] {
        &self.resources
    }

    /// The resource labelled `label`, if one is declared.
    ///
    /// How the renderer matches a host-supplied view to the imported
    /// resource it is a view *of* — by name, the same name the scene's
    /// bake declaration and the pipeline's `resource.color` agree on.
    pub fn resource_by_label(&self, label: &str) -> Option<ResourceId> {
        self.resources
            .iter()
            .position(|desc| desc.label == label)
            .map(|index| ResourceId(index as u32))
    }

    /// The passes, in declaration order.
    pub fn passes(&self) -> &[PassDesc] {
        &self.passes
    }

    /// Edit a declared pass before rescheduling the graph.
    pub fn pass_mut(&mut self, index: usize) -> Option<&mut PassDesc> {
        self.passes.get_mut(index)
    }

    /// Declare `desc` as the graph's shadow maps: the depth atlas
    /// the renderer binds into the frame group.
    ///
    /// Named rather than inferred, because this is the one resource read
    /// from *outside* the graph. Every other read is a
    /// [`crate::pass::Read`] and therefore an ordering edge and a usage
    /// flag; this one is bound beside the camera and the lights, where a
    /// pass list has no say (`abi::BINDING_SHADOW_MAPS`). The shadow passes
    /// order through their write chain; scheduling adds implicit reads to
    /// shading passes without changing their pass-group bindings.
    pub fn declare_shadow_maps(&mut self, desc: ResourceDesc) -> ResourceId {
        let id = self.resource(desc);
        self.shadow_maps = Some(id);
        id
    }

    /// The shadow maps, if this pipeline has any.
    pub fn shadow_maps(&self) -> Option<ResourceId> {
        self.shadow_maps
    }

    /// Bind Lambert-ready diffuse and GGX roughness-mip cubes in the frame
    /// group. Scheduling accounts for these reads without pass-group bindings.
    pub fn declare_environment_maps(&mut self, diffuse: ResourceId, specular: ResourceId) {
        self.environment_maps = Some([diffuse, specular]);
    }

    /// Diffuse and specular resources, in frame binding order.
    pub fn environment_maps(&self) -> Option<[ResourceId; 2]> {
        self.environment_maps
    }

    /// Restore the HDR source's radiance scale after its float16 convolution.
    pub fn set_environment_scale(&mut self, scale: f32) {
        self.environment_scale = scale;
    }

    /// Linear multiplier applied to sampled environment radiance.
    pub fn environment_scale(&self) -> f32 {
        self.environment_scale
    }

    /// Order the passes, validate them, and decide which physical texture
    /// serves each resource.
    ///
    /// Pure: no device, no allocation of anything but memory. Everything
    /// that can be wrong with a pass list is wrong here rather than as a
    /// `wgpu` validation error three layers down.
    pub fn schedule(&self) -> Result<Schedule, GraphError> {
        if let Some(maps) = self.shadow_maps {
            let mut expanded = self.clone();
            expanded.shadow_maps = None;
            for pass in &mut expanded.passes {
                let shades = matches!(&pass.kind,
                    PassKind::Geometry { stage, .. } if matches!(stage.output(), abi::StageOutput::Color | abi::StageOutput::PeelResolve))
                    || matches!(&pass.kind, PassKind::Screen { effect } if effect == "wxsl.deferred_lighting");
                if shades {
                    pass.reads.push(crate::pass::Read::current(maps));
                }
            }
            return expanded.schedule();
        }
        if let Some(maps) = self.environment_maps {
            if !self.environment_scale.is_finite() || self.environment_scale <= 0.0 {
                return Err(GraphError::InvalidTexture {
                    resource: "environment scale".into(),
                    reason: "must be finite and positive".into(),
                });
            }
            for id in maps {
                let Some(desc) = self.resource_desc(id) else {
                    return Err(GraphError::InvalidTexture {
                        resource: format!("environment {}", id.index()),
                        reason: "unknown resource".into(),
                    });
                };
                if desc.imported
                    || !matches!(
                        desc.shape,
                        ResourceShape::Texture {
                            dimension: Dimension::Cube,
                            format: TextureFormat::Rgba16Float,
                            ..
                        }
                    )
                    || !matches!(desc.persistence, Persistence::Persistent { history: 0 })
                {
                    return Err(GraphError::InvalidTexture {
                        resource: desc.label.clone(),
                        reason: "environment maps require pooled stable rgba16float cubes".into(),
                    });
                }
            }
            let mut expanded = self.clone();
            expanded.environment_maps = None;
            for pass in &mut expanded.passes {
                let shades = matches!(&pass.kind,
                    PassKind::Geometry { stage, .. } if matches!(stage.output(), abi::StageOutput::Color | abi::StageOutput::PeelResolve))
                    || matches!(&pass.kind, PassKind::Screen { effect } if effect == "wxsl.deferred_lighting");
                if shades {
                    pass.reads.extend(maps.map(crate::pass::Read::current));
                }
            }
            return expanded.schedule();
        }
        self.validate()?;
        let order = self.topological_order()?;
        Ok(self.allocate(order))
    }

    /// Everything checkable about a pass list before a device sees it.
    fn validate(&self) -> Result<(), GraphError> {
        for desc in &self.resources {
            if let ResourceShape::Texture {
                extent,
                dimension,
                layers,
                mip_levels,
                ..
            } = desc.shape
            {
                let invalid = |reason: &str| GraphError::InvalidTexture {
                    resource: desc.label.clone(),
                    reason: reason.into(),
                };
                if layers == 0 || mip_levels == 0 {
                    return Err(invalid("layer and mip counts must be nonzero"));
                }
                if dimension == Dimension::D2 && layers != 1
                    || dimension == Dimension::Cube && layers != 6
                {
                    return Err(invalid("a 2D texture has one layer; a cube has six faces"));
                }
                if dimension == Dimension::Cube
                    && !matches!(extent, Extent::Fixed { width, height } if width > 0 && width == height)
                {
                    return Err(invalid("cube textures require a fixed, square extent"));
                }
                if mip_levels > 1 {
                    let Extent::Fixed { width, height } = extent else {
                        return Err(invalid("multi-mip textures require a fixed extent"));
                    };
                    let depth = if dimension == Dimension::D3 {
                        layers
                    } else {
                        1
                    };
                    let largest = width.max(height).max(depth).max(1);
                    let maximum = u32::BITS - largest.leading_zeros();
                    if mip_levels > maximum {
                        return Err(invalid(&format!(
                            "{mip_levels} mip levels exceed the extent's maximum {maximum}"
                        )));
                    }
                }
            }
        }
        for pass in &self.passes {
            if pass.viewport.is_some() && pass.color.is_empty() && pass.depth.is_none() {
                return Err(GraphError::InvalidTextureUse {
                    pass: pass.label.clone(),
                    resource: "raster rectangle".into(),
                    reason: "requires a render attachment".into(),
                });
            }
            for id in pass
                .written()
                .chain(pass.reads.iter().map(|r| r.resource))
                .chain(pass.indirect_buffer())
            {
                if id.index() >= self.resources.len() {
                    return Err(GraphError::UnknownResource {
                        pass: pass.label.clone(),
                        resource: id,
                    });
                }
            }

            // Attachments are texture writes: a buffer in a colour or
            // depth slot is a shape mistake, and naming it here beats a
            // bind-group complaint three layers down (plan2 P11).
            for id in pass
                .color
                .iter()
                .map(|attachment| attachment.resource)
                .chain(pass.depth.iter().map(|depth| depth.resource))
            {
                if self
                    .resources
                    .get(id.index())
                    .is_some_and(|desc| desc.texture().is_none())
                {
                    return Err(GraphError::AttachmentNotATexture {
                        pass: pass.label.clone(),
                        resource: self.resources[id.index()].label.clone(),
                    });
                }
            }

            // Attachment views select one layer and one mip. Imports are
            // already-selected host views, never reinterpreted textures.
            let mut fixed_attachment_extent = None;
            for (id, layer, mip) in pass
                .color
                .iter()
                .map(|a| (a.resource, a.layer, a.mip))
                .chain(pass.depth.iter().map(|a| (a.resource, a.layer, a.mip)))
            {
                let desc = &self.resources[id.index()];
                if let Some(rect) = pass.viewport {
                    let valid = match desc.shape {
                        ResourceShape::Texture {
                            extent: extent @ Extent::Fixed { .. },
                            ..
                        } => {
                            let (width, height) = extent.resolve_mip(1, 1, mip);
                            rect.width > 0
                                && rect.height > 0
                                && rect
                                    .x
                                    .checked_add(rect.width)
                                    .is_some_and(|end| end <= width)
                                && rect
                                    .y
                                    .checked_add(rect.height)
                                    .is_some_and(|end| end <= height)
                        }
                        _ => false,
                    };
                    if !valid {
                        return Err(GraphError::InvalidTextureUse {
                            pass: pass.label.clone(), resource: desc.label.clone(),
                            reason: "raster rectangle requires a fixed attachment extent and must be nonempty and inside it".into(),
                        });
                    }
                }
                if let ResourceShape::Texture {
                    dimension,
                    extent,
                    layers,
                    mip_levels,
                    ..
                } = desc.shape
                {
                    let reason = if dimension == Dimension::D3 {
                        Some("3D attachments need a depth-slice view, which is not supported")
                    } else if layer >= layers || mip >= mip_levels {
                        Some("attachment layer or mip is outside the allocated texture")
                    } else if desc.imported && (layer != 0 || mip != 0) {
                        Some("an imported view is already selected by the host; nonzero selectors are not supported")
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        return Err(GraphError::InvalidTextureUse {
                            pass: pass.label.clone(),
                            resource: desc.label.clone(),
                            reason: reason.into(),
                        });
                    }
                    // Fixed mip extents are known without a frame target.
                    // Viewport/import sizes still belong to the host/device.
                    if !desc.imported && matches!(extent, Extent::Fixed { .. }) {
                        let size = extent.resolve_mip(1, 1, mip);
                        if fixed_attachment_extent.is_some_and(|expected| expected != size) {
                            return Err(GraphError::InvalidTextureUse {
                                pass: pass.label.clone(), resource: desc.label.clone(),
                                reason: "attachment mip extent differs from the pass's other attachments".into(),
                            });
                        }
                        fixed_attachment_extent = Some(size);
                    }
                }
            }
            for id in &pass.writes {
                let desc = &self.resources[id.index()];
                if matches!(
                    desc.shape,
                    ResourceShape::Texture {
                        dimension: Dimension::Cube,
                        ..
                    } | ResourceShape::Texture {
                        mip_levels: 2..,
                        ..
                    }
                ) {
                    return Err(GraphError::InvalidTextureUse {
                        pass: pass.label.clone(), resource: desc.label.clone(),
                        reason: "storage writes require a single-mip, non-cube view; use render-face attachments instead".into(),
                    });
                }
            }

            // A pass writing three colour targets needs a shader that
            // returns three. This is the one mismatch `wgpu` reports as an
            // entry-point signature error with no mention of the pass.
            if let Some(expected) = expected_color_targets(&pass.kind, self) {
                if pass.color.len() != expected {
                    return Err(GraphError::WrongColorTargetCount {
                        pass: pass.label.clone(),
                        expected,
                        found: pass.color.len(),
                    });
                }
            }

            // The depth format is in the pipeline state, so a pass whose
            // attachment disagrees with it builds a pipeline that cannot be
            // used with its own render pass.
            let attached = pass
                .depth
                .and_then(|depth| self.resources.get(depth.resource.index()))
                .map(|desc| {
                    desc.texture().map(|shape| {
                        let ResourceShape::Texture { format, .. } = shape else {
                            unreachable!("texture() answers only textures")
                        };
                        *format
                    })
                });
            if attached.flatten() != pass.state.depth_format {
                return Err(GraphError::DepthFormatMismatch {
                    pass: pass.label.clone(),
                    attached: attached.flatten(),
                    state: pass.state.depth_format,
                });
            }

            if let PassKind::Geometry {
                source:
                    DrawSource::Indirect {
                        buffer,
                        offset,
                        count,
                        ..
                    },
                ..
            } = &pass.kind
            {
                let desc = &self.resources[buffer.index()];
                let reason = match desc.shape {
                    ResourceShape::Texture { .. } => Some("not a buffer"),
                    ResourceShape::Buffer { .. } if pass.written().any(|id| id == *buffer) => {
                        Some("written by the same pass")
                    }
                    ResourceShape::Buffer { size, .. } if *count > 0 => {
                        if offset % 4 != 0 {
                            Some("offset must be a multiple of 4 bytes")
                        } else if offset
                            .checked_add(u64::from(*count) * 20)
                            .is_none_or(|end| end > size)
                        {
                            Some("indexed draw records exceed the buffer size")
                        } else {
                            None
                        }
                    }
                    ResourceShape::Buffer { .. } => None,
                };
                if let Some(reason) = reason {
                    return Err(GraphError::InvalidIndirectBuffer {
                        pass: pass.label.clone(),
                        resource: desc.label.clone(),
                        reason: reason.to_string(),
                    });
                }
            }

            for read in &pass.reads {
                let desc = &self.resources[read.resource.index()];
                // Sampling a texture the same pass is drawing into is a
                // read-write hazard, and `wgpu` reports it as a bind group
                // error with no mention of the attachment. Loading an
                // attachment is the legitimate way to read what is already
                // there; this is not.
                if read.history == 0 && pass.written().any(|id| id == read.resource) {
                    return Err(GraphError::ReadsWhatItWrites {
                        pass: pass.label.clone(),
                        resource: desc.label.clone(),
                    });
                }
                let depth = read.history as usize;
                if depth >= desc.persistence.ring_length() {
                    return Err(GraphError::NoSuchHistory {
                        pass: pass.label.clone(),
                        resource: desc.label.clone(),
                        history: read.history,
                        available: desc.persistence.ring_length() as u32 - 1,
                    });
                }
            }

            // A pass that does not run every frame must write only stable
            // storage: a transient's texture is free for reuse within the
            // frame, so its contents would be undefined on every frame the
            // pass skips — and the frame's own target is re-presented every
            // frame by definition. With this, *skipping* is safe: whatever
            // the pass last wrote is exactly what a later pass reads.
            if pass.policy != Policy::PerFrame {
                for id in pass.written() {
                    let desc = &self.resources[id.index()];
                    // An imported resource *other than the frame's own
                    // target* is stable in the strongest sense the rule is
                    // about: nobody here can reallocate it. The host owns
                    // the texture (a bake table most of all), so a skipped
                    // pass leaves its last write on a texture that cannot
                    // have moved — the rule's purpose holds by ownership
                    // rather than by the pool's ring. The target is
                    // imported too, but it is re-presented every frame by
                    // definition and is the frame's, not the host's.
                    let stable = (desc.imported && id != RenderGraph::TARGET)
                        || matches!(desc.persistence, Persistence::Persistent { history: 0 });
                    if !stable {
                        return Err(GraphError::PolicyNeedsStableStorage {
                            pass: pass.label.clone(),
                            resource: desc.label.clone(),
                            policy: pass.policy,
                            imported: desc.imported,
                        });
                    }
                }
            }
        }

        // Every resource read this frame must have been written this frame,
        // unless it is imported (someone else wrote it) or persistent (the
        // ring holds what an earlier frame wrote).
        let mut written = vec![false; self.resources.len()];
        for pass in &self.passes {
            for id in pass.written() {
                written[id.index()] = true;
            }
        }
        for pass in &self.passes {
            for id in pass.read_this_frame() {
                let desc = &self.resources[id.index()];
                if !written[id.index()] && !desc.imported {
                    return Err(GraphError::NeverWritten {
                        pass: pass.label.clone(),
                        resource: desc.label.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Kahn's algorithm over write-before-read edges, with declaration
    /// order as the tie-break so a graph that needs no reordering keeps the
    /// order it was written in.
    ///
    /// The edges come from the resources, not from the order the passes
    /// were declared in: a pass list written back to front schedules the
    /// same as one written front to back, which is the entire point of
    /// describing a pipeline rather than sequencing it.
    fn topological_order(&self) -> Result<Vec<usize>, GraphError> {
        let count = self.passes.len();
        let mut edges: Vec<Vec<usize>> = vec![Vec::new(); count];
        let mut incoming = vec![0usize; count];
        let mut connect = |from: usize, to: usize, edges: &mut Vec<Vec<usize>>| {
            if from != to && !edges[from].contains(&to) {
                edges[from].push(to);
                incoming[to] += 1;
            }
        };

        let mut writers: HashMap<usize, Vec<usize>> = HashMap::new();
        for (index, pass) in self.passes.iter().enumerate() {
            for id in pass.written() {
                writers.entry(id.index()).or_default().push(index);
            }
        }
        // A reader runs after every writer of what it reads.
        for (index, pass) in self.passes.iter().enumerate() {
            for id in pass.read_this_frame() {
                for writer in writers.get(&id.index()).into_iter().flatten() {
                    connect(*writer, index, &mut edges);
                }
            }
        }
        // Two passes writing the same resource keep their declared order,
        // which is what makes "clear it, then draw more into it" mean what
        // it looks like.
        for chain in writers.values() {
            for pair in chain.windows(2) {
                connect(pair[0], pair[1], &mut edges);
            }
        }

        let mut ready: Vec<usize> = (0..count).filter(|index| incoming[*index] == 0).collect();
        let mut order = Vec::with_capacity(count);
        while !ready.is_empty() {
            // Lowest declaration index first, so the order is deterministic.
            let position = ready
                .iter()
                .enumerate()
                .min_by_key(|(_, pass)| **pass)
                .map(|(position, _)| position)
                .expect("ready is not empty");
            let pass = ready.remove(position);
            order.push(pass);
            for &next in &edges[pass] {
                incoming[next] -= 1;
                if incoming[next] == 0 {
                    ready.push(next);
                }
            }
        }

        if order.len() != count {
            let stuck = (0..count)
                .find(|index| !order.contains(index))
                .expect("a pass is missing");
            return Err(GraphError::Cycle {
                pass: self.passes[stuck].label.clone(),
            });
        }
        Ok(order)
    }

    /// Assign a physical slot to every resource, reusing a transient's
    /// texture once its last reader has run.
    fn allocate(&self, order: Vec<usize>) -> Schedule {
        // Position in `order` of each pass, so a lifetime is an interval.
        let mut position = vec![0usize; self.passes.len()];
        for (step, pass) in order.iter().enumerate() {
            position[*pass] = step;
        }

        let mut usage = vec![TextureUsages::empty(); self.resources.len()];
        let mut buffer_usage = vec![BufferUsages::empty(); self.resources.len()];
        let mut first = vec![usize::MAX; self.resources.len()];
        let mut last = vec![0usize; self.resources.len()];
        let mut used = vec![false; self.resources.len()];
        let touch = |resource: ResourceId,
                     step: usize,
                     used: &mut Vec<bool>,
                     first: &mut Vec<usize>,
                     last: &mut Vec<usize>| {
            let index = resource.index();
            if !used[index] {
                used[index] = true;
                first[index] = step;
            }
            first[index] = first[index].min(step);
            last[index] = last[index].max(step);
        };

        // Which usage a touch infers depends on what the resource is: a
        // read of a texture binds it as a sampled texture, a read of a
        // buffer as storage — the pass group decides at record time.
        for (pass_index, pass) in self.passes.iter().enumerate() {
            let step = position[pass_index];
            for attachment in &pass.color {
                usage[attachment.resource.index()] |= TextureUsages::RENDER_ATTACHMENT;
                touch(attachment.resource, step, &mut used, &mut first, &mut last);
            }
            if let Some(depth) = pass.depth {
                usage[depth.resource.index()] |= TextureUsages::RENDER_ATTACHMENT;
                touch(depth.resource, step, &mut used, &mut first, &mut last);
            }
            for read in &pass.reads {
                match self.resources[read.resource.index()].shape {
                    ResourceShape::Texture { .. } => {
                        usage[read.resource.index()] |= TextureUsages::TEXTURE_BINDING;
                    }
                    ResourceShape::Buffer { .. } => {
                        buffer_usage[read.resource.index()] |= BufferUsages::STORAGE;
                    }
                }
                touch(read.resource, step, &mut used, &mut first, &mut last);
            }
            if let Some(buffer) = pass.indirect_buffer() {
                buffer_usage[buffer.index()] |= BufferUsages::INDIRECT;
                touch(buffer, step, &mut used, &mut first, &mut last);
            }
            for write in &pass.writes {
                match self.resources[write.index()].shape {
                    ResourceShape::Texture { .. } => {
                        usage[write.index()] |= TextureUsages::STORAGE_BINDING;
                    }
                    ResourceShape::Buffer { .. } => {
                        buffer_usage[write.index()] |= BufferUsages::STORAGE;
                    }
                }
                touch(*write, step, &mut used, &mut first, &mut last);
            }
        }

        let mut slots: Vec<SlotDesc> = Vec::new();
        let mut slot_free_after: Vec<usize> = Vec::new();
        let mut allocations = vec![Allocation::Imported; self.resources.len()];

        // Resources in order of first use, so a greedy assignment sees a
        // slot's whole lifetime before deciding whether to reuse it.
        let mut candidates: Vec<usize> = (0..self.resources.len())
            .filter(|index| used[*index] && !self.resources[*index].imported)
            .collect();
        candidates.sort_by_key(|index| (first[*index], *index));

        for index in candidates {
            let desc = &self.resources[index];
            let slot_desc = SlotDesc {
                label: desc.label.clone(),
                shape: match desc.shape {
                    ResourceShape::Texture {
                        extent,
                        dimension,
                        layers,
                        mip_levels,
                        format,
                        usage: extra,
                    } => SlotShape::Texture {
                        extent,
                        dimension,
                        layers,
                        mip_levels,
                        format,
                        usage: usage[index] | extra,
                    },
                    ResourceShape::Buffer { size, usage: extra } => SlotShape::Buffer {
                        size,
                        usage: buffer_usage[index] | extra,
                    },
                },
            };
            let length = desc.persistence.ring_length();
            if desc.persistence == Persistence::Transient && desc.texture().is_some() {
                // Reuse a compatible slot whose last reader has already run.
                let reusable = slots.iter().enumerate().position(|(slot, existing)| {
                    slot_free_after[slot] < first[index] && existing.aliasable_with(&slot_desc)
                });
                if let Some(slot) = reusable {
                    slot_free_after[slot] = last[index];
                    // The union of usages, so a texture reused as a sampled
                    // target is created with both flags.
                    if let (
                        SlotShape::Texture {
                            usage: existing, ..
                        },
                        SlotShape::Texture { usage: extra, .. },
                    ) = (&mut slots[slot].shape, &slot_desc.shape)
                    {
                        *existing |= *extra;
                    }
                    allocations[index] = Allocation::Ring {
                        base: slot,
                        length: 1,
                    };
                    continue;
                }
            }
            let base = slots.len();
            for ring in 0..length {
                let mut entry = slot_desc.clone();
                if length > 1 {
                    entry.label = format!("{} [{ring}]", slot_desc.label);
                }
                slots.push(entry);
                // A persistent slot is never free for anything else — a
                // ring of one included. `Persistent { history: 0 }` is how
                // a resource says "somebody outside the pass list holds a
                // view of me", and handing its texture to a later transient
                // would rebind that view to someone else's contents.
                let transient = desc.persistence == Persistence::Transient;
                slot_free_after.push(if transient { last[index] } else { usize::MAX });
            }
            allocations[index] = Allocation::Ring { base, length };
        }

        Schedule {
            order,
            allocations,
            slots,
        }
    }
}

/// How many colour targets a pass of this kind must have, or `None` when
/// the kind does not constrain it.
///
/// For a geometry pass this comes straight from the stage table: a stage
/// that returns a G-buffer needs one attachment per G-buffer target, and a
/// depth-only stage needs none.
fn expected_color_targets(kind: &PassKind, graph: &RenderGraph) -> Option<usize> {
    match kind {
        PassKind::Geometry { stage, .. } => Some(match stage.output() {
            // A G-buffer pass writes what the *enabled set* requested, not
            // the fixed base count the stage's own row names.
            abi::StageOutput::GBuffer => graph.gbuffer_layout.len(),
            _ => stage.color_targets(),
        }),
        PassKind::Screen { .. } => Some(1),
        PassKind::Compute { .. } => Some(0),
    }
}

/// Where a resource's contents actually live.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Allocation {
    /// Supplied by the caller each frame; the graph allocates nothing.
    Imported,
    /// `length` physical slots starting at `base`, rotated once per frame.
    /// A transient is the degenerate case, `length == 1`, and may share its
    /// slot with any other transient whose lifetime does not overlap.
    Ring {
        /// Index of the first slot.
        base: usize,
        /// How many slots the ring holds.
        length: usize,
    },
}

/// One physical resource the pool has to create.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct SlotDesc {
    /// Label for the `wgpu` texture or buffer.
    pub label: String,
    /// What it is made of, with every usage any resource in the slot
    /// needs.
    pub shape: SlotShape,
}

/// The make-up of one physical slot: the pool's mirror of
/// [`ResourceShape`], with the inferred and declared usages unioned.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotShape {
    /// A texture.
    Texture {
        /// Size.
        extent: Extent,
        /// Shape.
        dimension: Dimension,
        /// Layers, faces or depth.
        layers: u32,
        /// Number of allocated mip levels.
        mip_levels: u32,
        /// Texel format.
        format: TextureFormat,
        /// Every usage any resource in this slot needs.
        usage: TextureUsages,
    },
    /// A storage buffer.
    Buffer {
        /// Size in bytes.
        size: u64,
        /// Every usage any resource in this slot needs.
        usage: BufferUsages,
    },
}

impl SlotDesc {
    fn aliasable_with(&self, other: &SlotDesc) -> bool {
        match (&self.shape, &other.shape) {
            (
                SlotShape::Texture {
                    extent: a,
                    dimension: ad,
                    layers: al,
                    mip_levels: am,
                    format: af,
                    ..
                },
                SlotShape::Texture {
                    extent: b,
                    dimension: bd,
                    layers: bl,
                    mip_levels: bm,
                    format: bf,
                    ..
                },
            ) => a == b && ad == bd && al == bl && am == bm && af == bf,
            _ => false,
        }
    }
}

/// A validated, ordered, allocated plan for one graph.
///
/// Computed once per graph rather than once per frame: nothing in it
/// depends on the frame number or the target size, which is exactly why it
/// can be tested with no device.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Schedule {
    order: Vec<usize>,
    allocations: Vec<Allocation>,
    slots: Vec<SlotDesc>,
}

impl Schedule {
    /// The passes, in the order they will be recorded.
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// The physical textures the pool must create.
    pub fn slots(&self) -> &[SlotDesc] {
        &self.slots
    }

    /// Where a resource lives.
    pub fn allocation(&self, resource: ResourceId) -> Allocation {
        self.allocations
            .get(resource.index())
            .copied()
            .unwrap_or(Allocation::Imported)
    }

    /// Which physical slot serves `resource` on `frame`, reading `history`
    /// frames back.
    ///
    /// The rotation is the whole of the temporal story: the write goes to
    /// `frame % length` and a read of `h` frames ago to `(frame - h) %
    /// length`, so with a ring of three, frame 5 writes slot 2 while
    /// reading slots 1 and 0.
    pub fn slot(&self, resource: ResourceId, frame: u64, history: u32) -> Option<usize> {
        match self.allocation(resource) {
            Allocation::Imported => None,
            Allocation::Ring { base, length } => {
                let length = length as u64;
                let back = u64::from(history) % length;
                Some(base + ((frame + length - back) % length) as usize)
            }
        }
    }
}

/// What is wrong with a pass list.
#[derive(Clone, Debug, PartialEq)]
pub enum GraphError {
    /// A texture descriptor cannot be allocated as described.
    InvalidTexture {
        /// Resource label.
        resource: String,
        /// Descriptor invariant that failed.
        reason: String,
    },
    /// A pass asks for an unsupported or out-of-range texture view.
    InvalidTextureUse {
        /// Pass label.
        pass: String,
        /// Resource label.
        resource: String,
        /// View invariant that failed.
        reason: String,
    },
    /// Indirect arguments must be a buffer not written by the same pass.
    InvalidIndirectBuffer {
        /// Pass label.
        pass: String,
        /// Resource label.
        resource: String,
        /// Why the argument range or resource is invalid.
        reason: String,
    },
    /// A pass names a resource that was never declared.
    UnknownResource {
        /// The pass's label.
        pass: String,
        /// The id it named.
        resource: ResourceId,
    },
    /// The passes cannot be ordered: something reads what it writes, or two
    /// passes wait on each other.
    Cycle {
        /// One pass in the cycle.
        pass: String,
    },
    /// A pass reads a resource nothing writes.
    NeverWritten {
        /// The pass's label.
        pass: String,
        /// The resource's label.
        resource: String,
    },
    /// A pass has a number of colour attachments its shader cannot return.
    WrongColorTargetCount {
        /// The pass's label.
        pass: String,
        /// What the pass kind requires.
        expected: usize,
        /// What the pass declares.
        found: usize,
    },
    /// The depth attachment's format is not the one the pipeline state says.
    DepthFormatMismatch {
        /// The pass's label.
        pass: String,
        /// Format of the attached resource, if there is one.
        attached: Option<TextureFormat>,
        /// Format the pipeline state expects.
        state: Option<TextureFormat>,
    },
    /// A pass samples a resource it is also drawing into.
    ReadsWhatItWrites {
        /// The pass's label.
        pass: String,
        /// The resource's label.
        resource: String,
    },
    /// A pass reads further back than the resource's history goes.
    NoSuchHistory {
        /// The pass's label.
        pass: String,
        /// The resource's label.
        resource: String,
        /// Frames back the pass asked for.
        history: u32,
        /// Frames back the resource keeps.
        available: u32,
    },
    /// A pass whose [`Policy`] is not `per frame` writes a resource whose
    /// contents do not survive frames. On every frame the pass skips,
    /// whatever reads it would read undefined memory — make the target
    /// persistent (no history) instead.
    PolicyNeedsStableStorage {
        /// The pass's label.
        pass: String,
        /// The resource's label.
        resource: String,
        /// The policy the pass runs under.
        policy: Policy,
        /// Whether the resource was imported (the frame's own target) —
        /// which gets its own sentence, because "the target" is the
        /// likeliest way to arrive here.
        imported: bool,
    },
    /// A colour or depth attachment names a buffer. Attachments are
    /// texture writes; a buffer is read and written as storage, through
    /// the pass group.
    AttachmentNotATexture {
        /// The pass's label.
        pass: String,
        /// The buffer's label.
        resource: String,
    },
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphError::InvalidTexture { resource, reason } => {
                write!(f, "texture `{resource}` is invalid: {reason}")
            }
            GraphError::InvalidTextureUse {
                pass,
                resource,
                reason,
            } => write!(f, "pass `{pass}` cannot use texture `{resource}`: {reason}"),
            GraphError::InvalidIndirectBuffer {
                pass,
                resource,
                reason,
            } => write!(
                f,
                "pass `{pass}` cannot use indirect buffer `{resource}`: {reason}"
            ),
            GraphError::UnknownResource { pass, resource } => write!(
                f,
                "pass `{pass}` names resource {}, which this graph does not have",
                resource.index()
            ),
            GraphError::Cycle { pass } => write!(
                f,
                "the passes cannot be ordered: `{pass}` is in a cycle (a pass reading what it \
                 writes wants `Read::previous`, not `Read::current`)"
            ),
            GraphError::NeverWritten { pass, resource } => write!(
                f,
                "pass `{pass}` reads `{resource}`, which no pass writes and nothing imports"
            ),
            GraphError::WrongColorTargetCount {
                pass,
                expected,
                found,
            } => write!(
                f,
                "pass `{pass}` has {found} colour attachments but its shader writes {expected}"
            ),
            GraphError::DepthFormatMismatch {
                pass,
                attached,
                state,
            } => write!(
                f,
                "pass `{pass}` attaches a {attached:?} depth target but its state says {state:?}"
            ),
            GraphError::ReadsWhatItWrites { pass, resource } => write!(
                f,
                "pass `{pass}` samples `{resource}` and draws into it in the same pass; to read \
                 what is already there, load the attachment instead"
            ),
            GraphError::NoSuchHistory {
                pass,
                resource,
                history,
                available,
            } => write!(
                f,
                "pass `{pass}` reads `{resource}` {history} frames back, but it keeps {available}"
            ),
            GraphError::PolicyNeedsStableStorage {
                pass,
                resource,
                policy,
                imported,
            } => {
                write!(
                    f,
                    "pass `{pass}` runs {policy}, but it writes `{resource}`, whose contents do \
                     not survive frames"
                )?;
                if *imported {
                    f.write_str(
                        " — that is the frame's own target, which is presented every frame; \
                         write a persistent resource instead and present that",
                    )
                } else {
                    f.write_str(
                        " — make it persistent (with no history) so the pass's last \
                     output is what a later frame reads",
                    )
                }
            }
            GraphError::AttachmentNotATexture { pass, resource } => write!(
                f,
                "pass `{pass}` attaches `{resource}`, which is a buffer — attachments are \
                 texture writes; bind it through the pass group instead"
            ),
        }
    }
}

impl std::error::Error for GraphError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_rectangles_validate_mip_bounds_and_loading_writers_keep_order() {
        use crate::pass::{ViewportRect, DEPTH_FORMAT};
        let mut graph = RenderGraph::new(TextureFormat::Rgba8Unorm);
        let depth = graph.declare_shadow_maps(
            ResourceDesc::color("atlas", DEPTH_FORMAT)
                .with_extent(Extent::Fixed {
                    width: 64,
                    height: 64,
                })
                .with_mip_levels(2),
        );
        for index in 0..2 {
            graph.pass(
                PassDesc::geometry(
                    format!("tile {index}"),
                    DrawSource::Scene(TagExpr::Always),
                    abi::MaterialStage::SHADOW,
                )
                .with_depth(
                    if index == 0 {
                        DepthAttachment::clear(depth, 1.0)
                    } else {
                        DepthAttachment::load(depth)
                    }
                    .with_mip(1),
                )
                .with_viewport(ViewportRect::square(index * 16, 0, 16)),
            );
        }
        assert!(
            graph.schedule().is_ok(),
            "loaded depth tiles form a write chain, not a cycle"
        );
        for rect in [
            ViewportRect::square(0, 0, 0),
            ViewportRect::square(17, 0, 16),
            ViewportRect::square(u32::MAX, 0, 16),
        ] {
            graph.pass_mut(1).unwrap().viewport = Some(rect);
            let error = graph.schedule().unwrap_err();
            assert!(matches!(error, GraphError::InvalidTextureUse { .. }));
            assert!(error.to_string().contains("tile 1"));
        }
    }
    use crate::pass::{Attachment, DrawSource, PassState, Policy, Read, DEPTH_FORMAT};
    use wxsl_core::abi::{self, MaterialStage};
    use wxsl_core::scene::TagExpr;

    const COLOR: TextureFormat = TextureFormat::Rgba8Unorm;

    fn mip_texture() -> ResourceDesc {
        ResourceDesc::color("mip texture", COLOR)
            .with_extent(Extent::Fixed {
                width: 8,
                height: 8,
            })
            .with_mip_levels(4)
    }

    #[test]
    fn mip_counts_survive_allocation_and_prevent_incompatible_aliasing() {
        let mut graph = RenderGraph::new(COLOR);
        let base_desc = mip_texture().with_mip_levels(1);
        assert!(!base_desc.aliasable_with(&mip_texture()));
        let base = graph.resource(base_desc);
        let mips = graph.resource(mip_texture());
        graph.pass(
            PassDesc::screen("base", "test.clear")
                .with_color(Attachment::clear(base, Color::BLACK)),
        );
        graph.pass(
            PassDesc::screen("mips", "test.clear")
                .with_color(Attachment::clear(mips, Color::BLACK).with_mip(3)),
        );
        let schedule = graph.schedule().unwrap();
        assert_ne!(
            schedule.slot(base, 0, 0),
            schedule.slot(mips, 0, 0),
            "non-overlapping lifetimes still need different mip counts"
        );
        assert!(matches!(
            schedule.slots()[schedule.slot(mips, 0, 0).unwrap()].shape,
            SlotShape::Texture { mip_levels: 4, .. }
        ));
    }

    #[test]
    fn invalid_mip_and_cube_descriptors_are_named_before_allocation() {
        for desc in [
            mip_texture().with_mip_levels(0),
            mip_texture().with_mip_levels(5),
            mip_texture().with_extent(Extent::default()),
            mip_texture().with_dimension(Dimension::Cube, 5),
            mip_texture()
                .with_dimension(Dimension::Cube, 6)
                .with_extent(Extent::Fixed {
                    width: 8,
                    height: 4,
                }),
            mip_texture().with_dimension(Dimension::D2, 2),
        ] {
            let mut graph = RenderGraph::new(COLOR);
            graph.resource(desc);
            assert!(
                matches!(graph.schedule(), Err(GraphError::InvalidTexture { resource, .. }) if resource == "mip texture")
            );
        }
    }

    #[test]
    fn out_of_range_and_unsupported_attachment_views_are_named() {
        for (desc, layer, mip) in [
            (mip_texture(), 0, 4),
            (mip_texture(), 1, 0),
            (mip_texture().with_dimension(Dimension::Cube, 6), 6, 0),
            (mip_texture().with_dimension(Dimension::D3, 8), 0, 0),
            (
                ResourceDesc {
                    imported: true,
                    ..mip_texture()
                },
                0,
                1,
            ),
        ] {
            let mut graph = RenderGraph::new(COLOR);
            let texture = graph.resource(desc);
            graph.pass(
                PassDesc::screen("invalid view", "test.clear").with_color(
                    Attachment::clear(texture, Color::BLACK)
                        .with_layer(layer)
                        .with_mip(mip),
                ),
            );
            assert!(
                matches!(graph.schedule(), Err(GraphError::InvalidTextureUse { pass, resource, .. })
                if pass == "invalid view" && resource == "mip texture")
            );
        }
    }

    #[test]
    fn depth_mips_are_validated_and_whole_resource_hazards_stay_conservative() {
        let mut graph = RenderGraph::new(COLOR);
        let depth = graph.resource(
            ResourceDesc::color("depth mips", DEPTH_FORMAT)
                .with_extent(Extent::Fixed {
                    width: 8,
                    height: 8,
                })
                .with_mip_levels(4),
        );
        graph.pass(
            PassDesc::geometry(
                "depth view",
                DrawSource::Scene(TagExpr::Never),
                MaterialStage::DEPTH_ONLY,
            )
            .with_depth(DepthAttachment::clear(depth, 1.0).with_mip(4)),
        );
        assert!(
            matches!(graph.schedule(), Err(GraphError::InvalidTextureUse { resource, .. }) if resource == "depth mips")
        );
        let mut graph = RenderGraph::new(COLOR);
        let image = graph.resource(mip_texture());
        graph.pass(
            PassDesc::screen("same texture", "test.clear")
                .with_reads([Read::current(image)])
                .with_color(Attachment::clear(image, Color::BLACK).with_mip(1)),
        );
        assert!(matches!(
            graph.schedule(),
            Err(GraphError::ReadsWhatItWrites { .. })
        ));
    }

    #[test]
    fn storage_writes_do_not_silently_bind_whole_mip_chains_or_cube_views() {
        for desc in [
            mip_texture(),
            mip_texture()
                .with_dimension(Dimension::Cube, 6)
                .with_mip_levels(1),
        ] {
            let mut graph = RenderGraph::new(COLOR);
            let texture = graph.resource(desc);
            graph.pass(PassDesc::compute("storage", "test.fill").with_write(texture));
            assert!(
                matches!(graph.schedule(), Err(GraphError::InvalidTextureUse { pass, .. }) if pass == "storage")
            );
        }
    }

    #[test]
    fn mismatched_colour_and_depth_mip_extents_are_named() {
        let mut graph = RenderGraph::new(COLOR);
        let colour = graph.resource(mip_texture());
        let depth = graph.resource(
            ResourceDesc::color("depth", DEPTH_FORMAT)
                .with_extent(Extent::Fixed {
                    width: 8,
                    height: 8,
                })
                .with_mip_levels(4),
        );
        graph.pass(
            PassDesc::geometry(
                "mismatch",
                DrawSource::Scene(TagExpr::Never),
                MaterialStage::FORWARD_LIT,
            )
            .with_state(PassState::FULLSCREEN.with_depth_format(Some(DEPTH_FORMAT)))
            .with_color(Attachment::clear(colour, Color::BLACK))
            .with_depth(DepthAttachment::clear(depth, 1.0).with_mip(1)),
        );
        assert!(
            matches!(graph.schedule(), Err(GraphError::InvalidTextureUse { resource, .. }) if resource == "depth")
        );
    }

    #[test]
    fn indirect_arguments_order_their_writer_and_infer_buffer_usage() {
        let mut graph = RenderGraph::new(COLOR);
        let args = graph.resource(ResourceDesc::buffer("arguments", 20));
        graph.pass(
            PassDesc::geometry(
                "draw",
                DrawSource::Indirect {
                    buffer: args,
                    offset: 0,
                    count: 1,
                    draw: 0,
                },
                MaterialStage::FORWARD_LIT,
            )
            .with_state(PassState::FULLSCREEN)
            .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK)),
        );
        graph.pass(PassDesc::compute("fill", "test.arguments").with_write(args));
        let schedule = graph
            .schedule()
            .expect("indirect arguments are a dependency");
        assert_eq!(schedule.order(), &[1, 0]);
        let SlotShape::Buffer { usage, .. } = schedule.slots()[0].shape else {
            panic!("buffer")
        };
        assert!(usage.contains(BufferUsages::STORAGE | BufferUsages::INDIRECT));
    }

    #[test]
    fn an_indirect_texture_is_refused_by_resource_name() {
        let mut graph = RenderGraph::new(COLOR);
        let args = graph.resource(ResourceDesc::color("not arguments", COLOR));
        graph.pass(
            PassDesc::geometry(
                "draw",
                DrawSource::Indirect {
                    buffer: args,
                    offset: 0,
                    count: 1,
                    draw: 0,
                },
                MaterialStage::FORWARD_LIT,
            )
            .with_state(PassState::FULLSCREEN)
            .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK)),
        );
        assert!(
            matches!(graph.schedule(), Err(GraphError::InvalidIndirectBuffer { resource, .. }) if resource == "not arguments")
        );
    }

    #[test]
    fn invalid_indirect_ranges_are_refused_before_recording() {
        for (offset, count) in [(2, 1), (0, 2), (u64::MAX - 3, 1)] {
            let mut graph = RenderGraph::new(COLOR);
            let args = graph.resource(ResourceDesc::buffer("arguments", 20));
            graph.pass(
                PassDesc::geometry(
                    "draw",
                    DrawSource::Indirect {
                        buffer: args,
                        offset,
                        count,
                        draw: 0,
                    },
                    MaterialStage::FORWARD_LIT,
                )
                .with_state(PassState::FULLSCREEN)
                .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK)),
            );
            assert!(
                matches!(graph.schedule(), Err(GraphError::InvalidIndirectBuffer { resource, .. }) if resource == "arguments")
            );
        }
    }

    fn draw_all() -> DrawSource {
        DrawSource::Scene(TagExpr::Always)
    }

    /// A pass that writes `target`, clearing it, with no depth.
    fn writer(label: &str, target: ResourceId) -> PassDesc {
        PassDesc::screen(label, "deferred_lighting")
            .with_color(Attachment::clear(target, Color::BLACK))
    }

    #[test]
    fn the_transient_allocator_reuses_one_texture_across_two_lifetimes() {
        // `a` is written and consumed before `b` is born, so the two never
        // coexist and one texture serves both. Without this, every
        // intermediate in a postprocess chain costs its own target.
        let mut graph = RenderGraph::new(COLOR);
        let a = graph.resource(ResourceDesc::color("a", COLOR));
        let b = graph.resource(ResourceDesc::color("b", COLOR));
        graph.pass(writer("write a", a));
        graph.pass(writer("a to target", RenderGraph::TARGET).with_reads([Read::current(a)]));
        graph.pass(writer("write b", b));
        graph.pass(
            PassDesc::screen("b to target", "deferred_lighting")
                .with_color(Attachment::load(RenderGraph::TARGET))
                .with_reads([Read::current(b)]),
        );

        let schedule = graph.schedule().expect("schedules");
        assert_eq!(schedule.order(), &[0, 1, 2, 3]);
        assert_eq!(
            schedule.slots().len(),
            1,
            "two non-overlapping transients should share one texture: {:?}",
            schedule.slots()
        );
        assert_eq!(schedule.slot(a, 0, 0), schedule.slot(b, 0, 0));
        assert_eq!(
            schedule.allocation(RenderGraph::TARGET),
            Allocation::Imported
        );
    }

    #[test]
    fn overlapping_lifetimes_get_a_texture_each() {
        // `a` is still alive when `b` is born — the pass that writes `b`
        // is sampling `a` at that moment — so sharing would corrupt it.
        let mut graph = RenderGraph::new(COLOR);
        let a = graph.resource(ResourceDesc::color("a", COLOR));
        let b = graph.resource(ResourceDesc::color("b", COLOR));
        graph.pass(writer("write a", a));
        graph.pass(writer("a to b", b).with_reads([Read::current(a)]));
        graph.pass(writer("b to target", RenderGraph::TARGET).with_reads([Read::current(b)]));

        let schedule = graph.schedule().expect("schedules");
        assert_eq!(schedule.slots().len(), 2);
        assert_ne!(schedule.slot(a, 0, 0), schedule.slot(b, 0, 0));
    }

    #[test]
    fn a_persistent_resource_hands_a_pass_the_previous_frames_contents() {
        // The temporal case: what this frame writes must not be what the
        // next frame reads as "one frame ago".
        let mut graph = RenderGraph::new(COLOR);
        let history = graph.resource(ResourceDesc::color("history", COLOR).persistent(2));
        graph.pass(writer("accumulate", history).with_reads([Read::previous(history, 1)]));
        graph.pass(writer("present", RenderGraph::TARGET).with_reads([Read::current(history)]));

        let schedule = graph.schedule().expect("schedules");
        assert_eq!(schedule.slots().len(), 3, "history: 2 is a ring of three");

        let mut written = Vec::new();
        for frame in 0..6u64 {
            let now = schedule.slot(history, frame, 0).expect("allocated");
            let one_ago = schedule.slot(history, frame, 1).expect("allocated");
            let two_ago = schedule.slot(history, frame, 2).expect("allocated");
            if frame >= 1 {
                assert_eq!(one_ago, written[frame as usize - 1], "frame {frame}");
            }
            if frame >= 2 {
                assert_eq!(two_ago, written[frame as usize - 2], "frame {frame}");
            }
            assert_ne!(now, one_ago);
            assert_ne!(now, two_ago);
            written.push(now);
        }
        // And it is a ring, not a leak: three textures serve every frame.
        assert!(written.iter().all(|slot| *slot < 3));
    }

    #[test]
    fn passes_are_ordered_by_what_they_read() {
        // Declared backwards on purpose: the schedule must not need the
        // author to have got the order right.
        let mut graph = RenderGraph::new(COLOR);
        let middle = graph.resource(ResourceDesc::color("middle", COLOR));
        graph.pass(writer("second", RenderGraph::TARGET).with_reads([Read::current(middle)]));
        graph.pass(writer("first", middle));

        let schedule = graph.schedule().expect("schedules");
        assert_eq!(schedule.order(), &[1, 0]);
    }

    #[test]
    fn a_pass_sampling_its_own_attachment_is_reported() {
        let mut graph = RenderGraph::new(COLOR);
        let loop_back = graph.resource(ResourceDesc::color("loop", COLOR));
        graph.pass(writer("self", loop_back).with_reads([Read::current(loop_back)]));
        match graph.schedule() {
            Err(GraphError::ReadsWhatItWrites { pass, resource }) => {
                assert_eq!((pass.as_str(), resource.as_str()), ("self", "loop"));
            }
            other => panic!("expected a read-write hazard, got {other:?}"),
        }
    }

    #[test]
    fn two_passes_waiting_on_each_other_are_a_cycle() {
        let mut graph = RenderGraph::new(COLOR);
        let left = graph.resource(ResourceDesc::color("left", COLOR));
        let right = graph.resource(ResourceDesc::color("right", COLOR));
        graph.pass(writer("writes left", left).with_reads([Read::current(right)]));
        graph.pass(writer("writes right", right).with_reads([Read::current(left)]));
        assert!(matches!(graph.schedule(), Err(GraphError::Cycle { .. })));
    }

    #[test]
    fn reading_further_back_than_the_ring_goes_is_reported() {
        let mut graph = RenderGraph::new(COLOR);
        let once = graph.resource(ResourceDesc::color("once", COLOR).persistent(1));
        graph.pass(writer("too far", once).with_reads([Read::previous(once, 2)]));
        assert!(matches!(
            graph.schedule(),
            Err(GraphError::NoSuchHistory { history: 2, .. })
        ));
    }

    #[test]
    fn a_geometry_pass_must_have_as_many_targets_as_its_stage_writes() {
        let mut graph = RenderGraph::new(COLOR);
        graph.pass(
            PassDesc::geometry("gbuffer", draw_all(), MaterialStage::GBUFFER)
                .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK))
                .with_state(PassState::FULLSCREEN),
        );
        match graph.schedule() {
            Err(GraphError::WrongColorTargetCount {
                expected, found, ..
            }) => {
                assert_eq!((expected, found), (abi::GBUFFER_BASE_TARGETS.len(), 1));
            }
            other => panic!("expected a target-count error, got {other:?}"),
        }
    }

    #[test]
    fn a_depth_attachment_must_match_the_states_depth_format() {
        let mut graph = RenderGraph::new(COLOR);
        let depth = graph.resource(ResourceDesc::color("depth", DEPTH_FORMAT));
        graph.pass(
            PassDesc::geometry("forward", draw_all(), MaterialStage::FORWARD_LIT)
                .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK))
                .with_depth(DepthAttachment::clear(depth, 1.0))
                .with_state(
                    PassState::OPAQUE.with_depth_format(Some(TextureFormat::Depth24PlusStencil8)),
                ),
        );
        assert!(matches!(
            graph.schedule(),
            Err(GraphError::DepthFormatMismatch { .. })
        ));
    }

    #[test]
    fn reading_a_resource_nobody_writes_is_reported_not_drawn_black() {
        let mut graph = RenderGraph::new(COLOR);
        let orphan = graph.resource(ResourceDesc::color("orphan", COLOR));
        graph.pass(writer("present", RenderGraph::TARGET).with_reads([Read::current(orphan)]));
        match graph.schedule() {
            Err(GraphError::NeverWritten { resource, .. }) => assert_eq!(resource, "orphan"),
            other => panic!("expected a never-written error, got {other:?}"),
        }
    }

    #[test]
    fn a_slot_is_created_with_every_usage_its_resources_need() {
        // Drawn into, then sampled: a texture created with only one of the
        // two flags is a validation error at the first frame.
        let mut graph = RenderGraph::new(COLOR);
        let a = graph.resource(ResourceDesc::color("a", COLOR));
        graph.pass(writer("write a", a));
        graph.pass(writer("present", RenderGraph::TARGET).with_reads([Read::current(a)]));

        let schedule = graph.schedule().expect("schedules");
        let SlotShape::Texture { usage, .. } = schedule.slots()[0].shape else {
            panic!("a texture");
        };
        assert!(usage.contains(TextureUsages::RENDER_ATTACHMENT));
        assert!(usage.contains(TextureUsages::TEXTURE_BINDING));
    }

    #[test]
    fn a_pass_that_skips_frames_may_only_write_stable_storage() {
        // The once-baked LUT is the design case: written once, read every
        // frame — sound only because the target survives frames.
        let mut graph = RenderGraph::new(COLOR);
        let lut = graph.resource(ResourceDesc::color("brdf lut", COLOR).persistent(0));
        graph.pass(
            PassDesc::compute("bake", "brdf_lut")
                .with_write(lut)
                .with_policy(Policy::Once),
        );
        graph.pass(writer("present", RenderGraph::TARGET).with_reads([Read::current(lut)]));
        let schedule = graph.schedule().expect("the LUT graph schedules");
        assert_eq!(schedule.order(), &[0, 1]);
        // Storage writes get their usage flag, like any other write.
        let SlotShape::Texture { usage, .. } = schedule.slots()[0].shape else {
            panic!("a texture");
        };
        assert!(usage.contains(TextureUsages::STORAGE_BINDING));

        // The same bake into a transient is the mistake the rule exists
        // for: the slot is reused within the frame, so the second frame's
        // read would be of whatever rented the texture meanwhile.
        let mut graph = RenderGraph::new(COLOR);
        let scratch = graph.resource(ResourceDesc::color("scratch", COLOR));
        graph.pass(
            PassDesc::compute("bake", "brdf_lut")
                .with_write(scratch)
                .with_policy(Policy::Once),
        );
        graph.pass(writer("present", RenderGraph::TARGET).with_reads([Read::current(scratch)]));
        match graph.schedule() {
            Err(GraphError::PolicyNeedsStableStorage {
                resource, policy, ..
            }) => {
                assert_eq!(resource, "scratch");
                assert_eq!(policy, Policy::Once);
            }
            other => panic!("expected a stable-storage error, got {other:?}"),
        }

        // And writing the frame's own target under a policy is the same
        // mistake wearing the target's face.
        let mut graph = RenderGraph::new(COLOR);
        graph.pass(
            PassDesc::screen("backdrop", "lut_view")
                .with_policy(Policy::Once)
                .with_color(Attachment::clear(RenderGraph::TARGET, Color::BLACK)),
        );
        let error = graph.schedule().expect_err("the target is not stable");
        assert!(error.to_string().contains("frame's own target"), "{error}");
    }

    #[test]
    fn buffers_participate_in_ordering_and_never_share_a_slot() {
        // A compute pass fills a buffer; a screen pass reads it as
        // storage. The read is an ordering edge, exactly as a texture
        // read is — which is the whole of P11's point (plan2 P11).
        let mut graph = RenderGraph::new(COLOR);
        let ramp = graph.resource(ResourceDesc::buffer("ramp", 256));
        graph.pass(writer("show", RenderGraph::TARGET).with_reads([Read::current(ramp)]));
        graph.pass(PassDesc::compute("fill", "ramp_fill").with_write(ramp));
        // Declared second on purpose: the schedule must not need the
        // author to have got the order right.
        let schedule = graph.schedule().expect("schedules");
        assert_eq!(schedule.order(), &[1, 0]);
        // Storage usage is inferred for a buffer any pass touches.
        let SlotShape::Buffer { usage, .. } = schedule.slots()[0].shape else {
            panic!("a buffer");
        };
        assert!(usage.contains(BufferUsages::STORAGE));

        // And two buffers never share, however neatly their lifetimes
        // would fit: the aliasing rule is "never" for now, because a
        // buffer aliasing bug corrupts a whole block.
        let mut graph = RenderGraph::new(COLOR);
        let a = graph.resource(ResourceDesc::buffer("a", 64));
        let b = graph.resource(ResourceDesc::buffer("b", 64));
        graph.pass(PassDesc::compute("write a", "ramp_fill").with_write(a));
        graph.pass(writer("a to target", RenderGraph::TARGET).with_reads([Read::current(a)]));
        graph.pass(PassDesc::compute("write b", "ramp_fill").with_write(b));
        graph.pass(writer("b to target", RenderGraph::TARGET).with_reads([Read::current(b)]));
        let schedule = graph.schedule().expect("schedules");
        assert_ne!(schedule.slot(a, 0, 0), schedule.slot(b, 0, 0));
        assert_eq!(schedule.slots().len(), 2, "one slot per buffer");
    }

    #[test]
    fn a_buffer_in_an_attachment_slot_is_named() {
        // Attachments are texture writes; a buffer belongs to the pass
        // group. The mistake is a named error, not a bind-group complaint.
        let mut graph = RenderGraph::new(COLOR);
        let scratch = graph.resource(ResourceDesc::buffer("scratch", 64));
        graph.pass(writer("write", scratch));
        match graph.schedule() {
            Err(GraphError::AttachmentNotATexture { resource, .. }) => {
                assert_eq!(resource, "scratch")
            }
            other => panic!("expected an attachment-shape error, got {other:?}"),
        }
    }
}
