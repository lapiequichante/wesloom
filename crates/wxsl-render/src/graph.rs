//! The render graph: a list of [`PassDesc`]s in, a recorded frame out.
//!
//! A pipeline is no longer a Rust struct that writes `begin_render_pass` by
//! hand — it is a [`RenderGraph`], and this module is the engine that runs
//! one ([ADR 0021](../../../docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md)).
//! It has exactly three jobs:
//!
//! 1. **Order the passes.** A pass that reads what another wrote runs after
//!    it. Reading *history* creates no edge, which is what lets a temporal
//!    pass read last frame's output without a cycle.
//! 2. **Allocate the resources.** A transient target is created at its first
//!    write and its texture is free for reuse after its last read; a
//!    persistent one gets a ring so this frame's write does not destroy what
//!    last frame's read needs.
//! 3. **Record.** Resolve attachments to views, build the pass bind group,
//!    begin the pass, and hand it to the caller to issue draws into.
//!
//! The first two are pure functions over data — [`RenderGraph::schedule`]
//! needs no device and is tested without one, which matters because CI has
//! no GPU.
//!
//! ```text
//!   RenderGraph ──schedule──> Schedule ──configure──> ResourcePool
//!   (passes and                (order,                 (the textures)
//!    resource descs)            slot per resource)          │
//!                                     └────── record ───────┘
//! ```

use std::collections::HashMap;
use std::fmt;

use crate::error::RenderError;
use crate::pass::{
    Attachment, DepthAttachment, Dimension, Extent, Load, PassDesc, PassKind, Persistence,
    ResourceDesc, ResourceId,
};
use crate::pipeline::TargetConfig;

/// A pipeline, as a list of passes over a set of resources.
#[derive(Clone, Debug)]
pub struct RenderGraph {
    resources: Vec<ResourceDesc>,
    passes: Vec<PassDesc>,
}

impl RenderGraph {
    /// The frame's own target, which every graph has and nobody allocates.
    pub const TARGET: ResourceId = ResourceId(0);

    /// An empty graph whose target is in `format`.
    pub fn new(format: wgpu::TextureFormat) -> Self {
        RenderGraph {
            resources: vec![ResourceDesc::imported("target", format)],
            passes: Vec::new(),
        }
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

    /// The passes, in declaration order.
    pub fn passes(&self) -> &[PassDesc] {
        &self.passes
    }

    /// Look up a resource description.
    pub fn resource_desc(&self, id: ResourceId) -> Option<&ResourceDesc> {
        self.resources.get(id.index())
    }

    /// Order the passes, validate them, and decide which physical texture
    /// serves each resource.
    ///
    /// Pure: no device, no allocation of anything but memory. Everything
    /// that can be wrong with a pass list is wrong here rather than as a
    /// `wgpu` validation error three layers down.
    pub fn schedule(&self) -> Result<Schedule, GraphError> {
        self.validate()?;
        let order = self.topological_order()?;
        Ok(self.allocate(order))
    }

    /// Everything checkable about a pass list before a device sees it.
    fn validate(&self) -> Result<(), GraphError> {
        for pass in &self.passes {
            for id in pass.written().chain(pass.reads.iter().map(|r| r.resource)) {
                if id.index() >= self.resources.len() {
                    return Err(GraphError::UnknownResource {
                        pass: pass.label.clone(),
                        resource: id,
                    });
                }
            }

            // A pass writing three colour targets needs a shader that
            // returns three. This is the one mismatch `wgpu` reports as an
            // entry-point signature error with no mention of the pass.
            if let Some(expected) = expected_color_targets(&pass.kind) {
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
                .map(|desc| desc.format);
            if attached != pass.state.depth_format {
                return Err(GraphError::DepthFormatMismatch {
                    pass: pass.label.clone(),
                    attached,
                    state: pass.state.depth_format,
                });
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

        let mut usage = vec![wgpu::TextureUsages::empty(); self.resources.len()];
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

        for (pass_index, pass) in self.passes.iter().enumerate() {
            let step = position[pass_index];
            for attachment in &pass.color {
                usage[attachment.resource.index()] |= wgpu::TextureUsages::RENDER_ATTACHMENT;
                touch(attachment.resource, step, &mut used, &mut first, &mut last);
            }
            if let Some(depth) = pass.depth {
                usage[depth.resource.index()] |= wgpu::TextureUsages::RENDER_ATTACHMENT;
                touch(depth.resource, step, &mut used, &mut first, &mut last);
            }
            for read in &pass.reads {
                usage[read.resource.index()] |= wgpu::TextureUsages::TEXTURE_BINDING;
                touch(read.resource, step, &mut used, &mut first, &mut last);
            }
            for write in &pass.writes {
                usage[write.index()] |= wgpu::TextureUsages::STORAGE_BINDING;
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
                extent: desc.extent,
                dimension: desc.dimension,
                layers: desc.layers,
                format: desc.format,
                usage: usage[index] | desc.usage,
            };
            let length = desc.persistence.ring_length();
            if desc.persistence == Persistence::Transient {
                // Reuse a compatible slot whose last reader has already run.
                let reusable = slots.iter().enumerate().position(|(slot, existing)| {
                    slot_free_after[slot] < first[index] && existing.aliasable_with(&slot_desc)
                });
                if let Some(slot) = reusable {
                    slot_free_after[slot] = last[index];
                    // The union of usages, so a texture reused as a sampled
                    // target is created with both flags.
                    slots[slot].usage |= slot_desc.usage;
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
                // A persistent slot is never free for anything else.
                slot_free_after.push(if length > 1 { usize::MAX } else { last[index] });
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
fn expected_color_targets(kind: &PassKind) -> Option<usize> {
    match kind {
        PassKind::Geometry { stage, .. } => Some(stage.color_targets()),
        PassKind::Screen { .. } => Some(1),
        PassKind::Compute { .. } => Some(0),
    }
}

/// Where a resource's contents actually live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// One physical texture the pool has to create.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotDesc {
    /// Label for the `wgpu` texture.
    pub label: String,
    /// Size.
    pub extent: Extent,
    /// Shape.
    pub dimension: Dimension,
    /// Layers, faces or depth.
    pub layers: u32,
    /// Format.
    pub format: wgpu::TextureFormat,
    /// Every usage any resource in this slot needs.
    pub usage: wgpu::TextureUsages,
}

impl SlotDesc {
    fn aliasable_with(&self, other: &SlotDesc) -> bool {
        self.extent == other.extent
            && self.dimension == other.dimension
            && self.layers == other.layers
            && self.format == other.format
    }
}

/// A validated, ordered, allocated plan for one graph.
///
/// Computed once per graph rather than once per frame: nothing in it
/// depends on the frame number or the target size, which is exactly why it
/// can be tested with no device.
#[derive(Clone, Debug)]
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
        attached: Option<wgpu::TextureFormat>,
        /// Format the pipeline state expects.
        state: Option<wgpu::TextureFormat>,
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
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
        }
    }
}

impl std::error::Error for GraphError {}

/// The textures a [`Schedule`] asked for.
///
/// Reallocated when the target's size changes or the schedule does, and not
/// otherwise: resizing a window must not leak a texture per frame.
pub struct ResourcePool {
    slots: Vec<Slot>,
    layout: Vec<SlotDesc>,
    target: Option<TargetConfig>,
    // Keyed on what the bind group *is* — the shapes and the physical
    // slots — rather than on which pass asked for it: two passes that agree
    // on both would build the identical bind group, and a pass index would
    // go stale the moment the pass list changed.
    bind_groups: HashMap<(Vec<PassBinding>, Vec<usize>), wgpu::BindGroup>,
    bind_layouts: HashMap<Vec<PassBinding>, wgpu::BindGroupLayout>,
    frame: u64,
}

struct Slot {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// The shape of one entry of a pass bind group.
///
/// Two passes whose reads have the same shapes can share a bind group
/// *layout*, and therefore a pipeline layout — which is why this is what
/// the caches are keyed on rather than "does this pass have a pass group",
/// a question two passes can answer the same way while wanting different
/// layouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PassBinding {
    /// How the texture is sampled.
    pub sample_type: wgpu::TextureSampleType,
    /// The view's dimension.
    pub view_dimension: wgpu::TextureViewDimension,
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
            let (width, height) = desc.extent.resolve(target.width, target.height);
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&format!("wxsl {}", desc.label)),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: desc.layers,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: desc.dimension.texture_dimension(),
                format: desc.format,
                usage: desc.usage,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(desc.dimension.view_dimension()),
                ..Default::default()
            });
            self.slots.push(Slot { texture, view });
        }
        self.layout = schedule.slots().to_vec();
        self.target = Some(target);
    }

    /// The texture serving one slot, if the pool has been configured.
    ///
    /// The escape hatch for anything the graph does not do itself: reading
    /// a target back, or handing it to another system.
    pub fn texture(&self, slot: usize) -> Option<&wgpu::Texture> {
        self.slots.get(slot).map(|slot| &slot.texture)
    }

    /// The view of one slot, for a whole-resource binding.
    fn view(&self, slot: usize) -> &wgpu::TextureView {
        &self.slots[slot].view
    }

    /// A view of one layer of a slot, for an attachment.
    ///
    /// A cube face or a shadow cascade is rendered into one layer at a
    /// time; a plain 2D target's layer 0 is the whole texture, and reuses
    /// the view that already exists.
    fn attachment_view(&self, slot: usize, layer: u32) -> wgpu::TextureView {
        let entry = &self.slots[slot];
        if layer == 0 && entry.texture.depth_or_array_layers() == 1 {
            return entry.view.clone();
        }
        entry.texture.create_view(&wgpu::TextureViewDescriptor {
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
    /// [`RenderGraph::TARGET`]. `body` issues the actual work: the graph has
    /// opened the pass, resolved its attachments and bound nothing, because
    /// which bind groups a draw needs is the caller's business, not the
    /// scheduler's.
    pub fn record<F>(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        schedule: &Schedule,
        pool: &mut ResourcePool,
        imports: &[(ResourceId, &wgpu::TextureView)],
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
                    resource: self.resources[id.index()].label.clone(),
                })
        };
        for &index in schedule.order() {
            let pass = &self.passes[index];

            // The pass bind group: the resources it reads, in order, at
            // binding 0..n of `abi::GROUP_PASS`. The G-buffer the deferred
            // lighting pass samples is exactly this, and so is a screen
            // effect's input; there is nothing pass-specific left to write.
            let (pass_bindings, pass_layout, pass_bind_group) =
                self.pass_group(device, pool, schedule, pass)?;

            let color_formats: Vec<Option<wgpu::ColorTargetState>> = pass
                .color
                .iter()
                .map(|attachment| {
                    Some(wgpu::ColorTargetState {
                        format: self.resources[attachment.resource.index()].format,
                        blend: pass.state.blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })
                })
                .collect();
            let recorded = RecordedPass {
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
                                        Load::Clear(color) => wgpu::LoadOp::Clear(color),
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

    /// Build (or reuse) the pass group's layout and bind group.
    #[allow(clippy::type_complexity)]
    fn pass_group(
        &self,
        device: &wgpu::Device,
        pool: &mut ResourcePool,
        schedule: &Schedule,
        pass: &PassDesc,
    ) -> Result<
        (
            Vec<PassBinding>,
            Option<wgpu::BindGroupLayout>,
            Option<wgpu::BindGroup>,
        ),
        RenderError,
    > {
        if pass.reads.is_empty() {
            return Ok((Vec::new(), None, None));
        }
        let kinds: Vec<PassBinding> = pass
            .reads
            .iter()
            .map(|read| {
                let desc = &self.resources[read.resource.index()];
                PassBinding {
                    sample_type: desc
                        .format
                        .sample_type(None, None)
                        .unwrap_or(wgpu::TextureSampleType::Float { filterable: true }),
                    view_dimension: desc.dimension.view_dimension(),
                }
            })
            .collect();

        let layout = pool
            .bind_layouts
            .entry(kinds.clone())
            .or_insert_with(|| {
                let entries: Vec<wgpu::BindGroupLayoutEntry> = kinds
                    .iter()
                    .enumerate()
                    .map(|(binding, kind)| wgpu::BindGroupLayoutEntry {
                        binding: binding as u32,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT
                            | wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: kind.sample_type,
                            view_dimension: kind.view_dimension,
                            multisampled: false,
                        },
                        count: None,
                    })
                    .collect();
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("wxsl pass"),
                    entries: &entries,
                })
            })
            .clone();

        let slots: Vec<usize> = pass
            .reads
            .iter()
            .map(|read| schedule.slot(read.resource, pool.frame, read.history))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| RenderError::MissingImport {
                resource: pass.label.clone(),
            })?;
        let key = (kinds.clone(), slots.clone());
        if let Some(existing) = pool.bind_groups.get(&key) {
            return Ok((kinds, Some(layout), Some(existing.clone())));
        }
        let views: Vec<wgpu::TextureView> =
            slots.iter().map(|slot| pool.view(*slot).clone()).collect();
        let entries: Vec<wgpu::BindGroupEntry> = views
            .iter()
            .enumerate()
            .map(|(binding, view)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: wgpu::BindingResource::TextureView(view),
            })
            .collect();
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
    use crate::pass::{Attachment, DrawSource, PassState, Read, ScreenShader, DEPTH_FORMAT};
    use wxsl_core::abi::{self, MaterialStage};
    use wxsl_core::scene::TagExpr;

    const COLOR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

    fn draw_all() -> DrawSource {
        DrawSource::Scene(TagExpr::Always)
    }

    /// A pass that writes `target`, clearing it, with no depth.
    fn writer(label: &str, target: ResourceId) -> PassDesc {
        PassDesc::screen(label, ScreenShader::DeferredLighting)
            .with_color(Attachment::clear(target, wgpu::Color::BLACK))
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
            PassDesc::screen("b to target", ScreenShader::DeferredLighting)
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
                .with_color(Attachment::clear(RenderGraph::TARGET, wgpu::Color::BLACK))
                .with_state(PassState::FULLSCREEN),
        );
        match graph.schedule() {
            Err(GraphError::WrongColorTargetCount {
                expected, found, ..
            }) => {
                assert_eq!((expected, found), (abi::GBUFFER_TARGETS.len(), 1));
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
                .with_color(Attachment::clear(RenderGraph::TARGET, wgpu::Color::BLACK))
                .with_depth(DepthAttachment::clear(depth, 1.0))
                .with_state(
                    PassState::OPAQUE
                        .with_depth_format(Some(wgpu::TextureFormat::Depth24PlusStencil8)),
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
        let slot = &schedule.slots()[0];
        assert!(slot.usage.contains(wgpu::TextureUsages::RENDER_ATTACHMENT));
        assert!(slot.usage.contains(wgpu::TextureUsages::TEXTURE_BINDING));
    }
}
