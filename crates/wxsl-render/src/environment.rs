//! What is being looked *with* — camera, lights, ambient — and the buffers
//! that carry the frame group.
//!
//! [`Environment`] is deliberately not called a scene: a *scene* is meshes
//! and instances, it is pure data, and it lives in `wxsl_core::scene`. What
//! is here is the other half of a frame — where the camera is, what is
//! lighting it — plus [`FrameBindings`], the `wgpu` side of
//! `abi::GROUP_FRAME` ([ADR 0021](../../../docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md)).
//!
//! The `#[repr(C)]` structs here are the host side of the ABI's
//! `shaders/wxsl/bindings.wxsl`: same fields, same order, same padding.
//! WGSL aligns a `vec3f` to 16 bytes, so every one is followed by an explicit
//! scalar rather than relying on the compiler to insert it. A test checks the
//! sizes, which is the part that silently corrupts every frame when it drifts.

use std::collections::BTreeMap;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wxsl_core::abi;
use wxsl_core::resources::BufferLayout;

use crate::pass::PassView;

/// Maximum lights the scene uniform carries.
///
/// The ABI owns the number, because it is the length of an array in a
/// host-shared buffer *and* the layer count of the shadow map array.
pub const MAX_LIGHTS: usize = abi::MAX_LIGHTS;

/// Where the camera is and what it can see.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    /// Camera position in world space.
    pub eye: Vec3,
    /// Point the camera looks at.
    pub target: Vec3,
    /// Approximate up direction.
    pub up: Vec3,
    /// Vertical field of view, radians.
    pub fov_y: f32,
    /// Viewport width over height.
    pub aspect: f32,
    /// Near plane distance.
    pub near: f32,
    /// Far plane distance.
    pub far: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            eye: Vec3::new(2.5, 2.0, 3.5),
            target: Vec3::ZERO,
            up: Vec3::Y,
            fov_y: 45.0_f32.to_radians(),
            aspect: 1.0,
            near: 0.1,
            far: 100.0,
        }
    }
}

impl Camera {
    /// The combined view-projection matrix.
    ///
    /// `perspective_rh` (not `_gl`) because `wgpu`'s clip space has z in
    /// `[0, 1]`.
    pub fn view_proj(&self) -> Mat4 {
        // `directx` is the NDC convention wgpu uses: Z in [0, 1], Y up.
        let projection = glam::camera::rh::proj::directx::perspective(
            self.fov_y,
            self.aspect.max(1e-3),
            self.near,
            self.far,
        );
        projection * glam::camera::rh::view::look_at_mat4(self.eye, self.target, self.up)
    }

    /// The uniform this camera fills in.
    pub fn uniform(&self) -> CameraUniform {
        CameraUniform::new(self.view_proj(), self.eye)
    }
}

/// What kind of light a [`Light`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightKind {
    /// Radiates from a point, with inverse-square falloff.
    Point,
    /// Infinitely far away: one direction, no falloff.
    Directional,
}

/// Where a light renders its shadow map from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowView {
    /// World space to the light's clip space.
    pub view_proj: Mat4,
    /// Where the light's view sits, which is what a graph reading
    /// `view_direction` in a shadow pass sees.
    pub eye: Vec3,
}

/// One light.
#[derive(Clone, Copy, Debug)]
pub struct Light {
    /// Point or directional.
    pub kind: LightKind,
    /// World position for a point light, or the direction *towards* the light
    /// for a directional one.
    pub position_or_direction: Vec3,
    /// Linear-light colour.
    pub color: Vec3,
    /// Radiant intensity multiplier.
    pub intensity: f32,
    /// Whether this light renders a shadow map.
    ///
    /// Only a [`LightKind::Directional`] light can: one slice is one point
    /// of view, and a point light needs six. A point light asking for a
    /// shadow gets none rather than a wrong one, and
    /// [`Light::shadow_view`] is where that is decided.
    pub casts_shadow: bool,
    /// Half the edge of the world-space box a directional light shadows,
    /// centred on the origin.
    ///
    /// Fixed and centred rather than fitted to the camera: cascades are
    /// what make a shadow follow a view without blurring into nothing, and
    /// they are a milestone of their own. Until then this is the knob that
    /// trades area for texels.
    pub shadow_extent: f32,
    /// How deep the directional light's view is, along its own direction.
    pub shadow_distance: f32,
    /// How far along the surface normal a lookup is nudged before it is
    /// compared, in world units. See `shaders/wxsl/shadow.wxsl`.
    pub shadow_normal_bias: f32,
}

impl Light {
    /// The shadow settings every light starts with: none.
    ///
    /// Off by default, because a shadow map is a pass per light and an
    /// application that has not asked for one should not pay for four.
    pub const DEFAULTS: Light = Light {
        kind: LightKind::Directional,
        position_or_direction: Vec3::Y,
        color: Vec3::ONE,
        intensity: 1.0,
        casts_shadow: false,
        shadow_extent: 8.0,
        shadow_distance: 32.0,
        shadow_normal_bias: 0.03,
    };

    /// A point light at `position`.
    pub fn point(position: Vec3, color: Vec3, intensity: f32) -> Self {
        Light {
            kind: LightKind::Point,
            position_or_direction: position,
            color,
            intensity,
            ..Light::DEFAULTS
        }
    }

    /// A directional light shining *from* `direction`.
    pub fn directional(direction: Vec3, color: Vec3, intensity: f32) -> Self {
        Light {
            kind: LightKind::Directional,
            position_or_direction: direction.normalize_or_zero(),
            color,
            intensity,
            ..Light::DEFAULTS
        }
    }

    /// Cast a shadow, over a box `extent` wide centred on the origin.
    pub fn casting_shadow(mut self, extent: f32) -> Self {
        self.casts_shadow = true;
        self.shadow_extent = extent.max(1e-3);
        self
    }

    /// Set the normal-offset bias, in world units.
    pub fn with_normal_bias(mut self, bias: f32) -> Self {
        self.shadow_normal_bias = bias;
        self
    }

    /// The point of view this light renders its shadow map from, or
    /// `None` when it casts none.
    ///
    /// An orthographic box along the light's direction, centred on the
    /// origin and deep enough to contain what is in front of it. `directx`
    /// is the same NDC convention [`Camera::view_proj`] uses, so a depth
    /// written by one and compared by the other means the same thing.
    pub fn shadow_view(&self) -> Option<ShadowView> {
        if !self.casts_shadow || self.kind != LightKind::Directional {
            return None;
        }
        let direction = self.position_or_direction.normalize_or_zero();
        if direction.length_squared() < 1e-6 {
            return None;
        }
        let half = self.shadow_extent.max(1e-3);
        let depth = self.shadow_distance.max(half);
        // `position_or_direction` points *towards* the light, so the eye
        // sits that way from the origin and looks back at it.
        let eye = direction * depth;
        // Any up vector not parallel to the direction; a light straight
        // overhead is the ordinary case and the one that would degenerate.
        let up = if direction.y.abs() > 0.99 {
            Vec3::Z
        } else {
            Vec3::Y
        };
        let projection = glam::camera::rh::proj::directx::orthographic(
            -half,
            half,
            -half,
            half,
            0.0,
            depth * 2.0,
        );
        Some(ShadowView {
            view_proj: projection * glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, up),
            eye,
        })
    }

    fn uniform(&self, shadow_slice: i32, shadow_view_proj: Mat4) -> LightUniform {
        LightUniform {
            position_or_direction: self.position_or_direction.to_array(),
            kind: match self.kind {
                LightKind::Point => 0.0,
                LightKind::Directional => 1.0,
            },
            color: self.color.to_array(),
            intensity: self.intensity,
            shadow_view_proj: shadow_view_proj.to_cols_array_2d(),
            shadow_slice,
            shadow_normal_bias: self.shadow_normal_bias,
            _padding: [0.0; 2],
        }
    }
}

/// What is lighting the frame and where it is seen from.
///
/// Renamed from `Scene` in M1, which is what it always was: a scene is the
/// *document* — meshes, instances, materials — and that now exists, in
/// [`wxsl_core::scene`].
#[derive(Clone, Debug)]
pub struct Environment {
    /// The camera.
    pub camera: Camera,
    /// Lights. Only the first [`MAX_LIGHTS`] reach the shader.
    pub lights: Vec<Light>,
    /// Ambient light from above.
    pub ambient_sky: Vec3,
    /// Ambient light bounced from below.
    pub ambient_ground: Vec3,
    /// Multiplier applied to the shaded colour before tonemapping.
    pub exposure: f32,
    /// Seconds since the renderer started, for animated materials.
    pub time: f32,
    /// What [`Environment::time`] was on the previous frame.
    ///
    /// Only read by a shader compiled with
    /// [`abi::FEATURE_PREVIOUS_FRAME`], which is how a velocity stage gets
    /// last frame's answer out of a time-driven graph.
    /// [`Environment::advance`] keeps it up to date; an application that
    /// has no velocity stage may leave it alone.
    pub previous_time: f32,
}

impl Default for Environment {
    fn default() -> Self {
        Environment {
            camera: Camera::default(),
            lights: Vec::new(),
            ambient_sky: Vec3::new(0.32, 0.40, 0.55),
            ambient_ground: Vec3::new(0.10, 0.08, 0.07),
            exposure: 1.0,
            time: 0.0,
            previous_time: 0.0,
        }
    }
}

impl Environment {
    /// Move the clock to `time`, remembering what it was.
    ///
    /// A method rather than a field an application sets twice, because
    /// "the previous frame" is a fact about the sequence of frames and
    /// getting it out of step is silent: the velocity stage would compute
    /// a motion vector of zero and everything would look almost right.
    pub fn advance(&mut self, time: f32) {
        self.previous_time = self.time;
        self.time = time;
    }

    /// The uniform this environment fills in.
    ///
    /// Lights beyond [`MAX_LIGHTS`] are dropped: the alternative is silently
    /// overrunning a host-shared array.
    pub fn uniform(&self) -> SceneUniform {
        let mut lights = [LightUniform::zeroed(); MAX_LIGHTS];
        let count = self.lights.len().min(MAX_LIGHTS);
        for (index, (slot, light)) in lights.iter_mut().zip(&self.lights[..count]).enumerate() {
            // A light's shadow slice is its own index, so turning shadows
            // off for one does not renumber the others — and the pass that
            // fills slice `i` is the one that was built for light `i`.
            let (shadow_slice, view_proj) = match light.shadow_view() {
                Some(view) => (index as i32, view.view_proj),
                None => (-1, Mat4::IDENTITY),
            };
            *slot = light.uniform(shadow_slice, view_proj);
        }
        SceneUniform {
            lights,
            ambient_sky: self.ambient_sky.to_array(),
            _padding0: 0.0,
            ambient_ground: self.ambient_ground.to_array(),
            _padding1: 0.0,
            light_count: count as u32,
            time: self.time,
            exposure: self.exposure,
            previous_time: self.previous_time,
        }
    }

    /// Every point of view this frame renders from, in [`PassView::slot`]
    /// order: the camera, then one per light.
    ///
    /// Always the full length, so a pass's view slot is a fixed offset
    /// rather than a lookup into whatever lights happen to exist. A light
    /// that casts no shadow gets the camera's own view, which nothing ever
    /// draws against — the pass built for it issues no draws at all.
    pub fn views(&self) -> Vec<CameraUniform> {
        let camera = self.camera.uniform();
        let mut views = vec![camera; PassView::COUNT];
        for (index, light) in self.lights.iter().take(MAX_LIGHTS).enumerate() {
            let Some(view) = light.shadow_view() else {
                continue;
            };
            let slot = PassView::Light {
                index: index as u32,
            }
            .slot();
            views[slot] = CameraUniform::new(view.view_proj, view.eye);
        }
        views
    }

    /// Whether light `index` renders a shadow map this frame.
    pub fn light_casts_shadow(&self, index: u32) -> bool {
        self.lights
            .get(index as usize)
            .filter(|_| (index as usize) < MAX_LIGHTS)
            .is_some_and(|light| light.shadow_view().is_some())
    }
}

/// Host mirror of `Camera` in `shaders/wxsl/bindings.wxsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct CameraUniform {
    /// View-projection matrix, column-major.
    pub view_proj: [[f32; 4]; 4],
    /// Inverse of `view_proj`, for reconstructing world positions from depth.
    pub inverse_view_proj: [[f32; 4]; 4],
    /// Camera position in world space.
    pub position: [f32; 3],
    _padding: f32,
}

impl CameraUniform {
    /// One point of view: its world-to-clip matrix and where it sits.
    ///
    /// Not only the camera's — a shadow pass fills one of these from the
    /// light it renders for, which is what lets the shader keep reading
    /// `camera` and mean whichever view the pass named.
    pub fn new(view_proj: Mat4, position: Vec3) -> Self {
        CameraUniform {
            view_proj: view_proj.to_cols_array_2d(),
            inverse_view_proj: view_proj.inverse().to_cols_array_2d(),
            position: position.to_array(),
            _padding: 0.0,
        }
    }
}

/// Host mirror of `Light` in `shaders/wxsl/bindings.wxsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct LightUniform {
    /// Position (point) or direction towards the light (directional).
    pub position_or_direction: [f32; 3],
    /// 0 for point, 1 for directional.
    pub kind: f32,
    /// Linear-light colour.
    pub color: [f32; 3],
    /// Intensity multiplier.
    pub intensity: f32,
    /// World space to this light's clip space, for the shadow lookup.
    pub shadow_view_proj: [[f32; 4]; 4],
    /// Layer of the shadow map array this light rendered into, or -1 for a
    /// light that casts no shadow.
    pub shadow_slice: i32,
    /// Normal-offset bias, in world units.
    pub shadow_normal_bias: f32,
    _padding: [f32; 2],
}

/// Host mirror of `Scene` in `shaders/wxsl/bindings.wxsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SceneUniform {
    /// Fixed-size light array; only the first `light_count` are meaningful.
    pub lights: [LightUniform; MAX_LIGHTS],
    /// Ambient light from above.
    pub ambient_sky: [f32; 3],
    _padding0: f32,
    /// Ambient light from below.
    pub ambient_ground: [f32; 3],
    _padding1: f32,
    /// How many lights are in use.
    pub light_count: u32,
    /// Seconds since start.
    pub time: f32,
    /// Exposure multiplier.
    pub exposure: f32,
    /// What `time` was last frame.
    pub previous_time: f32,
}

/// Host mirror of one element of `instances` in
/// `shaders/wxsl/bindings.wxsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct InstanceTransform {
    /// Object-to-world matrix, column-major.
    pub model: [[f32; 4]; 4],
    /// Inverse transpose of `model`, as a 4x4 so its layout needs no
    /// per-column padding. Only the upper 3x3 is read.
    pub normal_matrix: [[f32; 4]; 4],
}

impl InstanceTransform {
    /// The transform for an object with the given model matrix.
    ///
    /// The normal matrix is the inverse transpose, so non-uniform scaling
    /// does not shear the normals off the surface.
    pub fn new(model: Mat4) -> Self {
        InstanceTransform {
            model: model.to_cols_array_2d(),
            normal_matrix: model.inverse().transpose().to_cols_array_2d(),
        }
    }
}

impl Default for InstanceTransform {
    fn default() -> Self {
        InstanceTransform::new(Mat4::IDENTITY)
    }
}

/// How many instances the storage buffer starts out able to hold.
///
/// It doubles from here rather than being a hard limit: the point of the
/// storage buffer is that the count is not part of the layout.
const INITIAL_INSTANCE_CAPACITY: usize = 64;

/// The declared per-instance attributes of one frame, grouped by the row
/// shape they are in.
///
/// One frame, and possibly more than one shape: what a material declares
/// is a row of its own design, so two materials in the same draw list can
/// want two different strides (ADR 0024). Each shape gets a buffer at
/// `abi::BINDING_INSTANCE_ATTRIBUTES` and a frame bind group of its own,
/// differing from the base only there. The *transforms* are not in here
/// at all: that array is ABI, one shape, one upload, for the whole frame.
///
/// Every buffer is as long as the whole draw list, and a draw writes at
/// its *global* index. Dense per-shape packing would save memory in a
/// frame that mixes shapes, and it would cost the property that makes the
/// indirect path work unchanged: `@builtin(instance_index)` is the draw's
/// index in the list, and an indirect buffer the application wrote holds
/// those same indices. One shape — which is every frame this repo draws —
/// wastes nothing at all.
#[derive(Clone, Debug, Default)]
pub struct InstanceRows {
    rows: BTreeMap<String, InstanceRowSet>,
}

/// One shape's worth of [`InstanceRows`].
#[derive(Clone, Debug)]
pub struct InstanceRowSet {
    layout: BufferLayout,
    bytes: Vec<u8>,
}

impl InstanceRowSet {
    /// The layout every row in this set has.
    pub fn layout(&self) -> &BufferLayout {
        &self.layout
    }

    /// The rows, packed.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// How many rows.
    pub fn len(&self) -> usize {
        if self.layout.size() == 0 {
            return 0;
        }
        self.bytes.len() / self.layout.size() as usize
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl InstanceRows {
    /// Nothing to draw.
    pub fn new() -> Self {
        InstanceRows::default()
    }

    /// Reserve `count` zeroed rows of `layout`, if that shape has none yet.
    ///
    /// A layout with no fields reserves nothing: a material that declares
    /// no per-instance attributes reads no attribute array, so there is
    /// no buffer for it to be given.
    pub fn reserve(&mut self, layout: &BufferLayout, count: usize) {
        if layout.is_empty() {
            return;
        }
        self.rows
            .entry(layout.signature())
            .or_insert_with(|| InstanceRowSet {
                layout: layout.clone(),
                bytes: vec![0; count * layout.size() as usize],
            });
    }

    /// The mutable bytes of row `index` in `layout`'s set.
    ///
    /// `None` when the shape was never reserved or the index is past its
    /// end, both of which are the caller having miscounted.
    pub fn row_mut(&mut self, layout: &BufferLayout, index: usize) -> Option<&mut [u8]> {
        let set = self.rows.get_mut(&layout.signature())?;
        let stride = set.layout.size() as usize;
        set.bytes.get_mut(index * stride..(index + 1) * stride)
    }

    /// Every shape, by [`BufferLayout::signature`].
    pub fn sets(&self) -> impl Iterator<Item = (&str, &InstanceRowSet)> {
        self.rows.iter().map(|(key, set)| (key.as_str(), set))
    }

    /// Whether nothing was reserved at all.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// The buffers and bind group for the frame group ([`abi::GROUP_FRAME`]).
///
/// One instance is shared by every pass in a frame: the bindings are the
/// same for forward, for the deferred material pass and for the deferred
/// lighting pass, so the layout is created once and reused.
///
/// # Why the transforms are a storage buffer
///
/// ADR 0010 put the object transform in this group as a uniform, to be
/// addressed by a dynamic offset "when a multi-draw scene comes". It came,
/// and a dynamic offset lost: one storage buffer is one binding and one
/// upload for the whole frame, indexed in the shader by
/// `@builtin(instance_index)`, and it is the shape a culling pass can write
/// indices into later. The one capability it costs is
/// `DownlevelFlags::VERTEX_STORAGE`, which the WebGPU baseline satisfies
/// and WebGL does not — and WebGL is not a target
/// ([ADR 0021](../../../docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md)).
pub struct FrameBindings {
    /// Every point of view of the frame, one [`CameraUniform`] each at
    /// [`FrameBindings::view_stride`] apart, addressed by a dynamic
    /// offset. The camera is first; see [`PassView`].
    views: wgpu::Buffer,
    view_stride: u32,
    scene: wgpu::Buffer,
    instances: wgpu::Buffer,
    capacity: usize,
    /// The shadow maps currently bound, and where they came from — the
    /// pool's generation and slot — so that rebinding the same texture is
    /// free. `None` while the placeholder is bound.
    shadow_source: Option<(u64, usize)>,
    shadow_view: wgpu::TextureView,
    /// What [`ShadowMaps::Detached`] binds instead.
    shadow_placeholder: wgpu::TextureView,
    shadow_sampler: wgpu::Sampler,
    /// Kept alive because the views above may be of it.
    _shadow_fallback: wgpu::Texture,
    /// One buffer and bind group per *declared attribute* row shape,
    /// keyed by [`BufferLayout::signature`]. The empty shape — a material
    /// declaring none — is always present and is what a pass with no
    /// material behind it binds.
    groups: BTreeMap<String, InstanceGroup>,
    layout: wgpu::BindGroupLayout,
}

/// One attribute row shape's buffer and frame bind groups.
struct InstanceGroup {
    buffer: wgpu::Buffer,
    /// In rows, not bytes.
    capacity: usize,
    stride: usize,
    bound: wgpu::BindGroup,
    detached: wgpu::BindGroup,
}

/// Whether a frame bind group points at the real shadow maps.
///
/// Two groups per shape rather than one, for a `wgpu` rule with no way
/// around it: a texture may not be sampled and written in the same pass,
/// and a shadow pass has the frame group bound while drawing into the very
/// texture its binding 4 points at. The shader never reads it — a shadow
/// stage compiles no shading — but the usage tracker works on bind groups,
/// not on what the shader does with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowMaps {
    /// The maps this frame rendered. What every pass that shades binds.
    Bound,
    /// A one-texel placeholder. What a pass *writing* the maps binds.
    Detached,
}

impl FrameBindings {
    /// Create the buffers, layout and bind group.
    pub fn new(device: &wgpu::Device) -> Self {
        // One uniform per view, each at an offset the hardware will accept
        // as a dynamic one.
        let alignment = device.limits().min_uniform_buffer_offset_alignment;
        let size = size_of::<CameraUniform>() as u32;
        let view_stride = size.div_ceil(alignment.max(1)) * alignment.max(1);
        let views = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wxsl views"),
            size: u64::from(view_stride) * PassView::COUNT as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let scene = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wxsl scene"),
            size: size_of::<SceneUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let capacity = INITIAL_INSTANCE_CAPACITY;
        let instances = instance_buffer(
            device,
            "wxsl instances",
            size_of::<InstanceTransform>(),
            capacity,
        );

        // A one-texel slice per light, cleared to nothing and never
        // rendered into: a pipeline with no shadow passes still declares
        // the bindings, and an unfilled binding is a validation error
        // rather than a black frame.
        let shadow_fallback = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wxsl shadow fallback"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: abi::MAX_LIGHTS as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let shadow_placeholder = shadow_fallback.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        // Until a pipeline with shadow passes hands its texture over, the
        // placeholder is what everything reads: an empty depth map is a
        // fully lit scene.
        let shadow_view = shadow_placeholder.clone();
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wxsl shadow sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            // `Less`: a fragment nearer than what the light saw is lit, so
            // the comparison returns 1 where the sample survives.
            compare: Some(wgpu::CompareFunction::Less),
            ..Default::default()
        });

        let buffer = |binding: u32, storage: bool, dynamic: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: if storage {
                    wgpu::BufferBindingType::Storage { read_only: true }
                } else {
                    wgpu::BufferBindingType::Uniform
                },
                has_dynamic_offset: dynamic,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wxsl frame bindings"),
            entries: &[
                // Dynamic, because which point of view a pass renders from
                // is the pass's business and the shader's `camera` is
                // whichever one it named. A pipeline with no shadow passes
                // binds offset zero and pays nothing.
                buffer(abi::BINDING_CAMERA, false, true),
                buffer(abi::BINDING_SCENE, false, false),
                buffer(abi::BINDING_INSTANCES, true, false),
                // Always here, whether or not the material being drawn
                // declares anything: a frame group whose shape changed
                // per material would invalidate every pipeline layout
                // built against it.
                buffer(abi::BINDING_INSTANCE_ATTRIBUTES, true, false),
                wgpu::BindGroupLayoutEntry {
                    binding: abi::BINDING_SHADOW_MAPS,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: abi::BINDING_SHADOW_SAMPLER,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let mut bindings = FrameBindings {
            views,
            view_stride,
            scene,
            instances,
            capacity,
            shadow_source: None,
            shadow_view,
            shadow_placeholder,
            shadow_sampler,
            _shadow_fallback: shadow_fallback,
            groups: BTreeMap::new(),
            layout,
        };
        // The shape a material declaring nothing wants: no attributes at
        // all, and a one-row placeholder to fill the binding with.
        bindings.ensure(device, BASE_SHAPE, 0, 1);
        bindings
    }

    /// How many instances fit without reallocating.
    pub fn instance_capacity(&self) -> usize {
        self.capacity
    }

    /// How many distinct *declared* attribute row shapes are held.
    ///
    /// Zero for a frame whose materials declare no per-instance
    /// attributes: the placeholder every such material binds is not a
    /// shape anybody asked for.
    pub fn instance_shapes(&self) -> usize {
        self.groups.keys().filter(|key| *key != BASE_SHAPE).count()
    }

    /// Byte offset of one view in the views buffer — the dynamic offset a
    /// pass rendering from it binds with.
    pub fn view_offset(&self, view: PassView) -> u32 {
        self.view_stride * view.slot() as u32
    }

    /// Bind `texture` as the shadow maps.
    ///
    /// `source` says where the view came from — the resource pool's
    /// generation and slot — and rebinding the same one is free, which
    /// matters because this is called every frame and rebuilding the bind
    /// groups is not.
    pub fn set_shadow_maps(
        &mut self,
        device: &wgpu::Device,
        source: (u64, usize),
        texture: &wgpu::TextureView,
    ) {
        if self.shadow_source == Some(source) {
            return;
        }
        self.shadow_source = Some(source);
        self.shadow_view = texture.clone();
        self.rebuild_groups(device);
    }

    /// Rebuild every frame bind group from the buffers currently held.
    fn rebuild_groups(&mut self, device: &wgpu::Device) {
        let keys: Vec<String> = self.groups.keys().cloned().collect();
        for key in keys {
            let attributes = self.groups[&key].buffer.clone();
            let bound = self.build_group(device, &attributes, ShadowMaps::Bound);
            let detached = self.build_group(device, &attributes, ShadowMaps::Detached);
            if let Some(group) = self.groups.get_mut(&key) {
                group.bound = bound;
                group.detached = detached;
            }
        }
    }

    /// One frame bind group over `attributes`.
    fn build_group(
        &self,
        device: &wgpu::Device,
        attributes: &wgpu::Buffer,
        shadows: ShadowMaps,
    ) -> wgpu::BindGroup {
        let shadow_view = match shadows {
            ShadowMaps::Bound => &self.shadow_view,
            ShadowMaps::Detached => &self.shadow_placeholder,
        };
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wxsl frame bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_CAMERA,
                    // The bound range is one view, not the whole array:
                    // that is what a dynamic offset indexes.
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.views,
                        offset: 0,
                        size: wgpu::BufferSize::new(size_of::<CameraUniform>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_SCENE,
                    resource: self.scene.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_INSTANCES,
                    resource: self.instances.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_INSTANCE_ATTRIBUTES,
                    resource: attributes.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_SHADOW_MAPS,
                    resource: wgpu::BindingResource::TextureView(shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: abi::BINDING_SHADOW_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&self.shadow_sampler),
                },
            ],
        })
    }

    /// Create or grow one shape's buffer, rebuilding its bind group when
    /// the buffer moves.
    fn ensure(&mut self, device: &wgpu::Device, key: &str, stride: usize, rows: usize) {
        let grown = match self.groups.get(key) {
            Some(group) if group.capacity >= rows && group.stride == stride => return,
            // Double until it fits, so a scene that grows by one object
            // per frame does not reallocate every frame.
            Some(group) => {
                let mut capacity = group.capacity.max(1);
                while capacity < rows {
                    capacity *= 2;
                }
                capacity
            }
            None => rows.max(INITIAL_INSTANCE_CAPACITY),
        };
        let buffer = instance_buffer(device, "wxsl instance attributes", stride, grown);
        let bound = self.build_group(device, &buffer, ShadowMaps::Bound);
        let detached = self.build_group(device, &buffer, ShadowMaps::Detached);
        self.groups.insert(
            key.to_string(),
            InstanceGroup {
                buffer,
                capacity: grown,
                stride,
                bound,
                detached,
            },
        );
    }

    /// Grow the transform array and rebuild every bind group that points
    /// at it.
    fn grow_instances(&mut self, device: &wgpu::Device, rows: usize) {
        if rows <= self.capacity {
            return;
        }
        let mut capacity = self.capacity.max(1);
        while capacity < rows {
            capacity *= 2;
        }
        self.capacity = capacity;
        self.instances = instance_buffer(
            device,
            "wxsl instances",
            size_of::<InstanceTransform>(),
            capacity,
        );
        self.rebuild_groups(device);
    }

    /// Upload `environment` and every instance transform of the frame.
    ///
    /// Grows the storage buffer (and rebuilds the bind group) when the frame
    /// has more instances than any before it, which is the only time either
    /// is touched.
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        environment: &Environment,
        transforms: &[InstanceTransform],
        rows: &InstanceRows,
    ) {
        // Every point of view the frame has, camera first: one write per
        // view rather than one buffer per view, so a pass switches between
        // them with a dynamic offset and nothing is rebound.
        for (slot, view) in environment.views().iter().enumerate() {
            queue.write_buffer(
                &self.views,
                u64::from(self.view_stride) * slot as u64,
                bytemuck::bytes_of(view),
            );
        }
        queue.write_buffer(&self.scene, 0, bytemuck::bytes_of(&environment.uniform()));

        self.grow_instances(device, transforms.len());
        if !transforms.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(transforms));
        }

        for (key, set) in rows.sets() {
            self.ensure(device, key, set.layout().size() as usize, set.len());
            if set.is_empty() {
                continue;
            }
            let Some(group) = self.groups.get(key) else {
                continue;
            };
            queue.write_buffer(&group.buffer, 0, set.bytes());
        }
    }

    /// The bind group layout, for building pipeline layouts.
    ///
    /// One layout for every shape: the instance binding declares no
    /// minimum size, so a wider row is the same layout with a longer
    /// buffer behind it.
    pub fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    /// The bind group to set at [`abi::GROUP_FRAME`] for a material whose
    /// declared per-instance attributes have shape `signature`.
    ///
    /// Falls back to the empty shape, which is right for a pass with no
    /// material behind it and harmless for one whose rows were never
    /// uploaded — such a draw has nothing to read.
    pub fn instance_group(&self, signature: &str, shadows: ShadowMaps) -> &wgpu::BindGroup {
        self.groups
            .get(signature)
            .or_else(|| self.groups.get(BASE_SHAPE))
            .map(|group| match shadows {
                ShadowMaps::Bound => &group.bound,
                ShadowMaps::Detached => &group.detached,
            })
            .expect("the empty attribute shape is created up front")
    }

    /// The bind group for a material that declares no per-instance
    /// attributes.
    pub fn bind_group(&self, shadows: ShadowMaps) -> &wgpu::BindGroup {
        self.instance_group(BASE_SHAPE, shadows)
    }
}

/// [`BufferLayout::signature`] of a layout with no fields — what a
/// material declaring no per-instance attributes asks for.
const BASE_SHAPE: &str = "";

fn instance_buffer(
    device: &wgpu::Device,
    label: &str,
    stride: usize,
    capacity: usize,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        // Never zero: a zero-sized binding is a validation error, and an
        // empty frame — or a material that declares no attributes — is a
        // perfectly ordinary thing for an editor to draw.
        size: (capacity.max(1) * stride.max(4)) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxsl_core::node::ValueType;
    use wxsl_core::wxsl::WxslIdent;

    #[test]
    fn the_instance_row_mirrors_the_abi_table_field_for_field() {
        // The one host-shared layout M4 deliberately did *not* compute:
        // the transform array stays ABI, with a `#[repr(C)]` mirror and
        // this test, because `transform_vertex` reads it at that stride
        // and a widened row would move every field out from under it. A
        // material's own per-instance attributes are a separate array
        // (ADR 0024).
        let layout = BufferLayout::storage(
            abi::INSTANCE_BASE_FIELDS
                .iter()
                .map(|field| (WxslIdent::new(field.name).expect("valid"), field.ty)),
            [],
        );
        assert_eq!(layout.size() as usize, size_of::<InstanceTransform>());
        assert_eq!(
            layout.field("model").expect("declared").offset as usize,
            core::mem::offset_of!(InstanceTransform, model)
        );
        assert_eq!(
            layout.field("normal_matrix").expect("declared").offset as usize,
            core::mem::offset_of!(InstanceTransform, normal_matrix)
        );
    }

    #[test]
    fn an_attribute_row_shape_gets_a_buffer_and_the_empty_one_a_placeholder() {
        // Two shapes in one frame is the case the frame group has to
        // survive: the buffers differ, the layout does not, so every
        // pipeline built against it stays valid.
        let rows = InstanceRows::new();
        assert!(rows.is_empty());
        let mut rows = rows;
        rows.reserve(&BufferLayout::default(), 4);
        assert!(rows.is_empty(), "an empty row shape needs no buffer");
        let layout = BufferLayout::storage(
            [],
            [(WxslIdent::new("tint").expect("valid"), ValueType::Vec3)],
        );
        rows.reserve(&layout, 4);
        assert_eq!(rows.sets().count(), 1);
        let set = rows.sets().next().expect("one shape").1;
        assert_eq!(set.len(), 4);
        // 16, not 12: an array element's stride is its size rounded up
        // to its alignment, and a `vec3f` aligns to 16. Getting that
        // wrong is every instance after the first reading the one before.
        assert_eq!(set.layout().size(), 16);
    }

    #[test]
    fn uniform_layouts_match_wgsl_alignment_rules() {
        // vec3f aligns to 16 in WGSL, so every uniform struct is a multiple
        // of 16 bytes and each vec3 is padded to its own 16.
        assert_eq!(size_of::<CameraUniform>(), 64 + 64 + 16);
        // Two vec3+scalar pairs, then a mat4x4f (aligned to 16, so it
        // starts at 32), then the slice, the bias and their padding.
        assert_eq!(size_of::<LightUniform>(), 16 + 16 + 64 + 16);
        assert_eq!(
            size_of::<SceneUniform>(),
            MAX_LIGHTS * 112 + 16 + 16 + 16,
            "scene uniform layout drifted from bindings.wxsl"
        );
        assert_eq!(size_of::<InstanceTransform>(), 128);
        for size in [
            size_of::<CameraUniform>(),
            size_of::<LightUniform>(),
            size_of::<SceneUniform>(),
            size_of::<InstanceTransform>(),
        ] {
            assert_eq!(size % 16, 0);
        }
    }

    #[test]
    fn extra_lights_are_dropped_rather_than_overrunning_the_array() {
        let mut scene = Environment::default();
        for index in 0..MAX_LIGHTS + 3 {
            scene
                .lights
                .push(Light::point(Vec3::splat(index as f32), Vec3::ONE, 1.0));
        }
        let uniform = scene.uniform();
        assert_eq!(uniform.light_count as usize, MAX_LIGHTS);
        assert_eq!(uniform.lights.len(), MAX_LIGHTS);
    }

    #[test]
    fn the_inverse_view_projection_round_trips_a_world_point() {
        let camera = Camera::default();
        let view_proj = camera.view_proj();
        let inverse = Mat4::from_cols_array_2d(&camera.uniform().inverse_view_proj);
        let world = glam::Vec4::new(0.3, -0.2, 0.5, 1.0);
        let clip = view_proj * world;
        let back = inverse * clip;
        let back = back / back.w;
        assert!(
            (back.truncate() - world.truncate()).length() < 1e-4,
            "{back} != {world}"
        );
    }

    #[test]
    fn directional_lights_store_a_unit_direction() {
        let light = Light::directional(Vec3::new(0.0, 3.0, 4.0), Vec3::ONE, 2.0);
        let length = light.position_or_direction.length();
        assert!((length - 1.0).abs() < 1e-6);
        assert_eq!(light.uniform(-1, Mat4::IDENTITY).kind, 1.0);
    }
}
