//! What a pass *is*, as data: [`PassDesc`], the resources it reads and
//! writes, and the pipeline state it draws with.
//!
//! Nothing here records anything — [`crate::graph`] does that. A pass
//! description is inert, comparable and cheap to build, which is what lets a
//! pipeline be a list of them rather than a struct with a hand-written
//! `record()`, and what will let a pipeline *graph* produce them later
//! ([ADR 0021](../../../docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md)).
//!
//! # Resources are described, not allocated
//!
//! A [`ResourceDesc`] says what a target must be — extent, format,
//! dimension, how long it has to live — and the graph decides which physical
//! texture serves it. Two things in that description carry their weight
//! immediately even though M1 uses neither:
//!
//! * [`Dimension`], because cascaded shadows want an array, reflection
//!   probes want a cube and froxel volumetrics want a 3D texture, and an
//!   allocator that only knows about screen-sized 2D targets has to be
//!   rewritten rather than extended to learn them.
//! * [`Persistence::Persistent`], because every temporal technique — TAA,
//!   stabilised SSR, auto-exposure, trailing bloom — needs last frame's
//!   texture, and "which texture do I write this frame" is a question the
//!   allocator answers or nobody does.

use std::sync::Arc;

use wxsl_core::scene::TagExpr;

use crate::path::RenderPath;

/// A resource in a [`crate::graph::RenderGraph`], by index.
///
/// Opaque and `Copy`: a pass holds ids, never textures, so the same pass
/// list can be scheduled against a 512-pixel preview and a 4K window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId(pub(crate) u32);

impl ResourceId {
    /// The index this id stands for.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// The shape of a texture resource.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Dimension {
    /// A plain 2D texture.
    #[default]
    D2,
    /// An array of 2D layers — cascaded shadow maps, texture atlases with a
    /// layer per light.
    D2Array,
    /// Six faces, sampled as a cube — reflection and irradiance probes.
    Cube,
    /// A volume — froxel volumetrics, 3D LUTs.
    D3,
}

impl Dimension {
    /// The `wgpu` texture dimension this is stored as.
    ///
    /// A cube and an array are both `D2` textures with layers; only the
    /// *view* distinguishes them, which is exactly why the two are separate
    /// here and identical there.
    pub fn texture_dimension(self) -> wgpu::TextureDimension {
        match self {
            Dimension::D2 | Dimension::D2Array | Dimension::Cube => wgpu::TextureDimension::D2,
            Dimension::D3 => wgpu::TextureDimension::D3,
        }
    }

    /// The default view dimension for a whole resource of this shape.
    pub fn view_dimension(self) -> wgpu::TextureViewDimension {
        match self {
            Dimension::D2 => wgpu::TextureViewDimension::D2,
            Dimension::D2Array => wgpu::TextureViewDimension::D2Array,
            Dimension::Cube => wgpu::TextureViewDimension::Cube,
            Dimension::D3 => wgpu::TextureViewDimension::D3,
        }
    }
}

/// How big a resource is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Extent {
    /// A fraction of the frame's target: 1.0 for full resolution, 0.5 for a
    /// half-res bloom chain. Rounded up, and never zero.
    Viewport {
        /// Multiplier on the target's width and height.
        scale: f32,
    },
    /// A fixed size in pixels, whatever the window is doing — a shadow
    /// atlas, a LUT.
    Fixed {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
}

impl Extent {
    /// The pixel size of this extent when the frame's target is
    /// `target_width` x `target_height`.
    pub fn resolve(self, target_width: u32, target_height: u32) -> (u32, u32) {
        match self {
            Extent::Viewport { scale } => {
                let scaled = |value: u32| ((value as f32 * scale).ceil() as u32).max(1);
                (scaled(target_width), scaled(target_height))
            }
            Extent::Fixed { width, height } => (width.max(1), height.max(1)),
        }
    }
}

impl Default for Extent {
    fn default() -> Self {
        Extent::Viewport { scale: 1.0 }
    }
}

/// How long a resource's contents have to live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Persistence {
    /// Created at first write, dead after its last read, and its memory
    /// reusable by any later resource of the same shape.
    #[default]
    Transient,
    /// Survives the frame, kept as a ring so that this frame's write does
    /// not destroy what last frame wrote.
    Persistent {
        /// How many *previous* frames are readable. `1` is a classic
        /// ping-pong (two textures); `n` costs `n + 1`.
        history: u32,
    },
}

impl Persistence {
    /// How many physical textures this persistence needs.
    pub fn ring_length(self) -> usize {
        match self {
            Persistence::Transient => 1,
            Persistence::Persistent { history } => history as usize + 1,
        }
    }
}

/// Everything the graph needs in order to allocate a resource.
///
/// `Imported` is the escape hatch and the reason the frame's own target is
/// expressible: the graph is handed the view rather than creating it.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceDesc {
    /// Label, used for the `wgpu` texture and in diagnostics.
    pub label: String,
    /// Size.
    pub extent: Extent,
    /// Shape.
    pub dimension: Dimension,
    /// Array layers, cube faces (6) or volume depth. Always 1 for
    /// [`Dimension::D2`].
    pub layers: u32,
    /// Texel format.
    pub format: wgpu::TextureFormat,
    /// Usages beyond the ones the graph infers from how the passes use it.
    pub usage: wgpu::TextureUsages,
    /// Whether the contents outlive the frame.
    pub persistence: Persistence,
    /// Whether the graph owns the texture, or is handed one per frame.
    pub imported: bool,
}

impl ResourceDesc {
    /// A screen-sized colour target in `format`.
    pub fn color(label: impl Into<String>, format: wgpu::TextureFormat) -> Self {
        ResourceDesc {
            label: label.into(),
            extent: Extent::default(),
            dimension: Dimension::D2,
            layers: 1,
            format,
            usage: wgpu::TextureUsages::empty(),
            persistence: Persistence::Transient,
            imported: false,
        }
    }

    /// A resource the caller supplies a view for every frame — the window's
    /// swapchain texture, or a target another system owns.
    pub fn imported(label: impl Into<String>, format: wgpu::TextureFormat) -> Self {
        ResourceDesc {
            imported: true,
            ..ResourceDesc::color(label, format)
        }
    }

    /// Set the extent.
    pub fn with_extent(mut self, extent: Extent) -> Self {
        self.extent = extent;
        self
    }

    /// Set the shape and layer count.
    pub fn with_dimension(mut self, dimension: Dimension, layers: u32) -> Self {
        self.dimension = dimension;
        self.layers = layers.max(1);
        self
    }

    /// Add usages on top of the ones the graph infers.
    pub fn with_usage(mut self, usage: wgpu::TextureUsages) -> Self {
        self.usage |= usage;
        self
    }

    /// Keep the contents across frames, with `history` previous frames
    /// readable.
    pub fn persistent(mut self, history: u32) -> Self {
        self.persistence = Persistence::Persistent { history };
        self
    }

    /// Whether two resources could share one physical texture.
    ///
    /// Everything the `wgpu` descriptor is built from has to match; the
    /// label does not, because a shared texture ends up labelled after
    /// whichever resource claimed the slot first.
    pub fn aliasable_with(&self, other: &ResourceDesc) -> bool {
        !self.imported
            && !other.imported
            && self.persistence == Persistence::Transient
            && other.persistence == Persistence::Transient
            && self.extent == other.extent
            && self.dimension == other.dimension
            && self.layers == other.layers
            && self.format == other.format
    }
}

/// What happens to a colour attachment's existing contents.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Load {
    /// Overwrite with a constant. Cheaper than loading on tiled hardware.
    Clear(wgpu::Color),
    /// Keep what is there, which makes the pass a *read* of the resource as
    /// well as a write.
    Load,
}

/// One colour attachment of a render pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Attachment {
    /// What is being written.
    pub resource: ResourceId,
    /// What happens to what was already there.
    pub load: Load,
    /// Whether the result is kept. `false` for a depth buffer nothing reads
    /// back, which lets a tiler skip the write-out entirely.
    pub store: bool,
    /// Which array layer or cube face, for a layered resource.
    pub layer: u32,
}

impl Attachment {
    /// Clear `resource` to `color` and keep the result.
    pub fn clear(resource: ResourceId, color: wgpu::Color) -> Self {
        Attachment {
            resource,
            load: Load::Clear(color),
            store: true,
            layer: 0,
        }
    }

    /// Draw on top of what is already in `resource`.
    pub fn load(resource: ResourceId) -> Self {
        Attachment {
            resource,
            load: Load::Load,
            store: true,
            layer: 0,
        }
    }

    /// Target a single layer of an array or cube resource.
    pub fn with_layer(mut self, layer: u32) -> Self {
        self.layer = layer;
        self
    }
}

/// The depth-stencil attachment of a render pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthAttachment {
    /// The depth resource.
    pub resource: ResourceId,
    /// Clear value, or `None` to keep what a previous pass left — which is
    /// how a depth prepass hands its buffer to the pass that uses it.
    pub clear: Option<f32>,
    /// Whether the depth result is kept.
    pub store: bool,
    /// Which array layer, for a shadow cascade.
    pub layer: u32,
}

impl DepthAttachment {
    /// Clear to `depth` (1.0 is the far plane) and keep the result.
    pub fn clear(resource: ResourceId, depth: f32) -> Self {
        DepthAttachment {
            resource,
            clear: Some(depth),
            store: true,
            layer: 0,
        }
    }

    /// Test against what is already there, without clearing.
    pub fn load(resource: ResourceId) -> Self {
        DepthAttachment {
            resource,
            clear: None,
            store: true,
            layer: 0,
        }
    }
}

/// The pipeline state a pass draws with.
///
/// Part of the pipeline cache key rather than baked into one hardcoded
/// descriptor, which is what lets a shadow pass cull front faces, a
/// transparent pass blend, and a peel pass use a different depth format —
/// all with the same material shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PassState {
    /// Which face is culled, if any.
    pub cull_mode: Option<wgpu::Face>,
    /// Depth test. `Always` with `depth_write: false` and no depth
    /// attachment is a fullscreen pass.
    pub depth_compare: wgpu::CompareFunction,
    /// Whether the depth test's result is written back.
    pub depth_write: bool,
    /// Format of the depth attachment, or `None` for a pass with none.
    ///
    /// Per pass, not a crate constant: `Depth32Float` forecloses every
    /// stencil technique — portal masking, outline masks, shadow volumes —
    /// and a pass that wants a stencil should be able to ask for one
    /// without every other pass paying for it.
    pub depth_format: Option<wgpu::TextureFormat>,
    /// Blend state of every colour target, or `None` for opaque writes.
    pub blend: Option<wgpu::BlendState>,
}

impl PassState {
    /// Opaque geometry: cull back faces, depth test and write, no blending.
    pub const OPAQUE: PassState = PassState {
        cull_mode: Some(wgpu::Face::Back),
        depth_compare: wgpu::CompareFunction::Less,
        depth_write: true,
        depth_format: Some(DEPTH_FORMAT),
        blend: None,
    };

    /// A fullscreen pass: no culling, no depth attachment at all.
    pub const FULLSCREEN: PassState = PassState {
        cull_mode: None,
        depth_compare: wgpu::CompareFunction::Always,
        depth_write: false,
        depth_format: None,
        blend: None,
    };

    /// The same state with a different depth format.
    pub fn with_depth_format(mut self, format: Option<wgpu::TextureFormat>) -> Self {
        self.depth_format = format;
        self
    }

    /// The same state with blending.
    pub fn with_blend(mut self, blend: wgpu::BlendState) -> Self {
        self.blend = Some(blend);
        self
    }

    /// The same state with a different cull mode.
    pub fn with_cull_mode(mut self, cull_mode: Option<wgpu::Face>) -> Self {
        self.cull_mode = cull_mode;
        self
    }

    /// The `wgpu` depth-stencil state, or `None` when the pass has no depth
    /// attachment.
    pub fn depth_stencil(&self) -> Option<wgpu::DepthStencilState> {
        self.depth_format.map(|format| wgpu::DepthStencilState {
            format,
            depth_write_enabled: Some(self.depth_write),
            depth_compare: Some(self.depth_compare),
            stencil: Default::default(),
            bias: Default::default(),
        })
    }
}

/// The depth format a pass gets unless it asks for another.
///
/// `Depth32Float` because the deferred lighting pass samples depth to
/// reconstruct world position and wants the precision. It is a *default*,
/// not a law: [`PassState::depth_format`] is per pass.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Where a geometry pass's draws come from.
#[derive(Clone, Debug)]
pub enum DrawSource {
    /// The frame's draw list, filtered by a tag expression. The material
    /// says what it is, the pass says what it draws.
    Scene(TagExpr),
    /// An indirect buffer some earlier pass wrote — GPU-driven culling, GPU
    /// particles. The draw path is the same one either way, which is the
    /// point of having it here from the start rather than as a later
    /// redesign of how draws are issued.
    Indirect {
        /// Buffer of `wgpu::util::DrawIndexedIndirectArgs` records.
        buffer: Arc<wgpu::Buffer>,
        /// Byte offset of the first record.
        offset: u64,
        /// How many records to issue.
        count: u32,
        /// Which entry of the frame's draw list supplies the geometry and
        /// the material. An indirect draw still needs a mesh and a pipeline
        /// bound; what it does not need is a draw call per object.
        draw: usize,
    },
}

/// The one screen-space shader the renderer knows how to run.
///
/// A single variant for now, because the deferred lighting pass is the only
/// fullscreen shader that exists. M7 replaces it with a screen-domain
/// *graph* id, at which point this becomes the second variant of an enum
/// rather than a new pass kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScreenShader {
    /// Shade the G-buffer: `abi::LIGHTING_PASS_MODULE`.
    DeferredLighting,
}

/// A compute dispatch's workgroup count.
#[derive(Clone, Debug)]
pub enum Dispatch {
    /// A fixed number of workgroups.
    Direct {
        /// Workgroups in x.
        x: u32,
        /// Workgroups in y.
        y: u32,
        /// Workgroups in z.
        z: u32,
    },
    /// A count another pass wrote.
    Indirect {
        /// Buffer holding a `wgpu::util::DispatchIndirectArgs`.
        buffer: Arc<wgpu::Buffer>,
        /// Byte offset of the record.
        offset: u64,
    },
}

/// What kind of work a pass does.
#[derive(Clone, Debug)]
pub enum PassKind {
    /// Draw geometry, with each material compiled for `path`.
    Geometry {
        /// Where the draws come from.
        source: DrawSource,
        /// Which compiled variant of each material to draw with. M2 widens
        /// this from a two-valued path into a material *stage*.
        path: RenderPath,
    },
    /// One fullscreen triangle running a screen-space shader.
    Screen {
        /// Which shader.
        shader: ScreenShader,
    },
    /// A compute dispatch.
    ///
    /// Present from the start rather than bolted on in M7, and nearly free
    /// because the repo already runs a compute pass: MSDF glyph generation
    /// (ADR 0014) has already solved compute pipelines, their bind groups
    /// and their variant compilation.
    Compute {
        /// Entry point in the module.
        entry: String,
        /// How many workgroups.
        workgroups: Dispatch,
    },
}

/// One resource a pass reads, and from which frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Read {
    /// The resource.
    pub resource: ResourceId,
    /// How many frames back. `0` is this frame's contents and orders this
    /// pass after whoever wrote them; anything else reads a
    /// [`Persistence::Persistent`] resource's history and creates no
    /// ordering edge at all, which is precisely what makes a temporal
    /// technique schedulable.
    pub history: u32,
}

impl Read {
    /// This frame's contents of `resource`.
    pub fn current(resource: ResourceId) -> Self {
        Read {
            resource,
            history: 0,
        }
    }

    /// The contents `resource` had `history` frames ago.
    pub fn previous(resource: ResourceId, history: u32) -> Self {
        Read { resource, history }
    }
}

/// One pass, as data.
#[derive(Clone, Debug)]
pub struct PassDesc {
    /// Label, used for the `wgpu` pass and in diagnostics.
    pub label: String,
    /// What the pass does.
    pub kind: PassKind,
    /// Colour attachments, in `@location` order.
    pub color: Vec<Attachment>,
    /// Depth attachment, if any.
    pub depth: Option<DepthAttachment>,
    /// Pipeline state.
    pub state: PassState,
    /// Resources bound as the pass group (`abi::GROUP_PASS`), in binding
    /// order.
    pub reads: Vec<Read>,
    /// Resources the pass writes without attaching them — a compute pass's
    /// storage textures. Attachments are writes too and need not be listed.
    pub writes: Vec<ResourceId>,
}

impl PassDesc {
    /// A geometry pass drawing `source` with materials compiled for `path`.
    pub fn geometry(label: impl Into<String>, source: DrawSource, path: RenderPath) -> Self {
        PassDesc {
            label: label.into(),
            kind: PassKind::Geometry { source, path },
            color: Vec::new(),
            depth: None,
            state: PassState::OPAQUE,
            reads: Vec::new(),
            writes: Vec::new(),
        }
    }

    /// A fullscreen pass running `shader`.
    pub fn screen(label: impl Into<String>, shader: ScreenShader) -> Self {
        PassDesc {
            label: label.into(),
            kind: PassKind::Screen { shader },
            color: Vec::new(),
            depth: None,
            state: PassState::FULLSCREEN,
            reads: Vec::new(),
            writes: Vec::new(),
        }
    }

    /// A compute pass.
    pub fn compute(
        label: impl Into<String>,
        entry: impl Into<String>,
        workgroups: Dispatch,
    ) -> Self {
        PassDesc {
            label: label.into(),
            kind: PassKind::Compute {
                entry: entry.into(),
                workgroups,
            },
            color: Vec::new(),
            depth: None,
            state: PassState::FULLSCREEN,
            reads: Vec::new(),
            writes: Vec::new(),
        }
    }

    /// Add a colour attachment.
    pub fn with_color(mut self, attachment: Attachment) -> Self {
        self.color.push(attachment);
        self
    }

    /// Add every colour attachment in `attachments`.
    pub fn with_colors(mut self, attachments: impl IntoIterator<Item = Attachment>) -> Self {
        self.color.extend(attachments);
        self
    }

    /// Set the depth attachment.
    pub fn with_depth(mut self, depth: DepthAttachment) -> Self {
        self.depth = Some(depth);
        self
    }

    /// Set the pipeline state.
    pub fn with_state(mut self, state: PassState) -> Self {
        self.state = state;
        self
    }

    /// Bind `reads` as the pass group, in this order.
    pub fn with_reads(mut self, reads: impl IntoIterator<Item = Read>) -> Self {
        self.reads.extend(reads);
        self
    }

    /// Declare a write that is not an attachment.
    pub fn with_write(mut self, resource: ResourceId) -> Self {
        self.writes.push(resource);
        self
    }

    /// Every resource this pass writes, attachments included.
    pub fn written(&self) -> impl Iterator<Item = ResourceId> + '_ {
        self.color
            .iter()
            .map(|attachment| attachment.resource)
            .chain(self.depth.iter().map(|depth| depth.resource))
            .chain(self.writes.iter().copied())
    }

    /// Every resource this pass reads *this frame*, which is what orders it
    /// after the pass that wrote them.
    ///
    /// A `Load` attachment counts: drawing on top of something is a read of
    /// what is already there.
    pub fn read_this_frame(&self) -> impl Iterator<Item = ResourceId> + '_ {
        self.reads
            .iter()
            .filter(|read| read.history == 0)
            .map(|read| read.resource)
            .chain(
                self.color
                    .iter()
                    .filter(|attachment| attachment.load == Load::Load)
                    .map(|attachment| attachment.resource),
            )
            .chain(
                self.depth
                    .iter()
                    .filter(|depth| depth.clear.is_none())
                    .map(|depth| depth.resource),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_viewport_extent_scales_with_the_target_and_never_reaches_zero() {
        let half = Extent::Viewport { scale: 0.5 };
        assert_eq!(half.resolve(1280, 720), (640, 360));
        // Rounded up, so a half-res chain over an odd size still covers it.
        assert_eq!(half.resolve(1281, 721), (641, 361));
        // A bloom chain's last level must not be a zero-sized texture.
        assert_eq!(Extent::Viewport { scale: 0.01 }.resolve(4, 4), (1, 1));
        assert_eq!(
            Extent::Fixed {
                width: 0,
                height: 8
            }
            .resolve(1, 1),
            (1, 8)
        );
    }

    #[test]
    fn history_costs_one_texture_more_than_it_promises() {
        assert_eq!(Persistence::Transient.ring_length(), 1);
        // Ping-pong: this frame's write must not destroy last frame's read.
        assert_eq!(Persistence::Persistent { history: 1 }.ring_length(), 2);
        assert_eq!(Persistence::Persistent { history: 2 }.ring_length(), 3);
    }

    #[test]
    fn only_matching_transients_may_share_a_texture() {
        let base = ResourceDesc::color("a", wgpu::TextureFormat::Rgba8Unorm);
        assert!(base.aliasable_with(&ResourceDesc::color("b", wgpu::TextureFormat::Rgba8Unorm)));
        assert!(!base.aliasable_with(&ResourceDesc::color("b", wgpu::TextureFormat::Rgba16Float)));
        assert!(!base.aliasable_with(
            &ResourceDesc::color("b", wgpu::TextureFormat::Rgba8Unorm)
                .with_extent(Extent::Viewport { scale: 0.5 })
        ));
        // A persistent resource is the whole point of not being reused.
        assert!(!base.aliasable_with(
            &ResourceDesc::color("b", wgpu::TextureFormat::Rgba8Unorm).persistent(1)
        ));
        // And an imported one is not ours to hand out.
        assert!(!base.aliasable_with(&ResourceDesc::imported(
            "target",
            wgpu::TextureFormat::Rgba8Unorm
        )));
    }

    #[test]
    fn loading_an_attachment_counts_as_reading_it() {
        // Otherwise a pass that draws on top of another's output could be
        // scheduled before it.
        let first = ResourceId(1);
        let second = ResourceId(2);
        let pass = PassDesc::geometry(
            "overlay",
            DrawSource::Scene(TagExpr::Always),
            RenderPath::Forward,
        )
        .with_color(Attachment::load(first))
        .with_depth(DepthAttachment::load(second));
        assert_eq!(
            pass.read_this_frame().collect::<Vec<_>>(),
            vec![first, second]
        );
        assert_eq!(pass.written().collect::<Vec<_>>(), vec![first, second]);

        let clearing = PassDesc::geometry(
            "first",
            DrawSource::Scene(TagExpr::Always),
            RenderPath::Forward,
        )
        .with_color(Attachment::clear(first, wgpu::Color::BLACK))
        .with_depth(DepthAttachment::clear(second, 1.0));
        assert_eq!(clearing.read_this_frame().count(), 0);
    }

    #[test]
    fn reading_history_is_not_an_ordering_edge() {
        // A pass reading last frame's result must not be ordered after the
        // pass that writes this frame's; that is a cycle, and the whole
        // reason temporal techniques need history in the first place.
        let taa = ResourceId(4);
        let pass = PassDesc::screen("taa", ScreenShader::DeferredLighting)
            .with_reads([Read::previous(taa, 1)])
            .with_color(Attachment::clear(taa, wgpu::Color::BLACK));
        assert_eq!(pass.read_this_frame().count(), 0);
    }

    #[test]
    fn a_fullscreen_pass_has_no_depth_state_at_all() {
        // Attaching a depth texture and sampling it in the same pass is a
        // validation error, which is what the deferred lighting pass would
        // hit if it inherited the geometry state.
        assert!(PassState::FULLSCREEN.depth_stencil().is_none());
        let opaque = PassState::OPAQUE
            .depth_stencil()
            .expect("geometry has depth");
        assert_eq!(opaque.format, DEPTH_FORMAT);
        assert_eq!(opaque.depth_write_enabled, Some(true));
    }
}
