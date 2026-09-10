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

/// Maximum lights the scene uniform carries.
///
/// Fixed rather than a macro variable: it is the length of an array in a
/// host-shared buffer, so it cannot vary per shader variant without the Rust
/// and WXSL sides disagreeing about the layout. Must match
/// `WXSL_MAX_LIGHTS` in `shaders/wxsl/bindings.wxsl`.
pub const MAX_LIGHTS: usize = 4;

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
        let view_proj = self.view_proj();
        CameraUniform {
            view_proj: view_proj.to_cols_array_2d(),
            inverse_view_proj: view_proj.inverse().to_cols_array_2d(),
            position: self.eye.to_array(),
            _padding: 0.0,
        }
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
}

impl Light {
    /// A point light at `position`.
    pub fn point(position: Vec3, color: Vec3, intensity: f32) -> Self {
        Light {
            kind: LightKind::Point,
            position_or_direction: position,
            color,
            intensity,
        }
    }

    /// A directional light shining *from* `direction`.
    pub fn directional(direction: Vec3, color: Vec3, intensity: f32) -> Self {
        Light {
            kind: LightKind::Directional,
            position_or_direction: direction.normalize_or_zero(),
            color,
            intensity,
        }
    }

    fn uniform(&self) -> LightUniform {
        LightUniform {
            position_or_direction: self.position_or_direction.to_array(),
            kind: match self.kind {
                LightKind::Point => 0.0,
                LightKind::Directional => 1.0,
            },
            color: self.color.to_array(),
            intensity: self.intensity,
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
        }
    }
}

impl Environment {
    /// The uniform this environment fills in.
    ///
    /// Lights beyond [`MAX_LIGHTS`] are dropped: the alternative is silently
    /// overrunning a host-shared array.
    pub fn uniform(&self) -> SceneUniform {
        let mut lights = [LightUniform::zeroed(); MAX_LIGHTS];
        let count = self.lights.len().min(MAX_LIGHTS);
        for (slot, light) in lights.iter_mut().zip(&self.lights[..count]) {
            *slot = light.uniform();
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
            _padding2: 0.0,
        }
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
    _padding2: f32,
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
    camera: wgpu::Buffer,
    scene: wgpu::Buffer,
    instances: wgpu::Buffer,
    capacity: usize,
    /// One buffer and bind group per *declared attribute* row shape,
    /// keyed by [`BufferLayout::signature`]. The empty shape — a material
    /// declaring none — is always present and is what a pass with no
    /// material behind it binds.
    groups: BTreeMap<String, InstanceGroup>,
    layout: wgpu::BindGroupLayout,
}

/// One attribute row shape's buffer and frame bind group.
struct InstanceGroup {
    buffer: wgpu::Buffer,
    /// In rows, not bytes.
    capacity: usize,
    stride: usize,
    bind_group: wgpu::BindGroup,
}

impl FrameBindings {
    /// Create the buffers, layout and bind group.
    pub fn new(device: &wgpu::Device) -> Self {
        let uniform = |label: &str, size: u64| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let camera = uniform("wxsl camera", size_of::<CameraUniform>() as u64);
        let scene = uniform("wxsl scene", size_of::<SceneUniform>() as u64);
        let capacity = INITIAL_INSTANCE_CAPACITY;
        let instances = instance_buffer(
            device,
            "wxsl instances",
            size_of::<InstanceTransform>(),
            capacity,
        );

        let buffer = |binding: u32, storage: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: if storage {
                    wgpu::BufferBindingType::Storage { read_only: true }
                } else {
                    wgpu::BufferBindingType::Uniform
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wxsl frame bindings"),
            entries: &[
                buffer(abi::BINDING_CAMERA, false),
                buffer(abi::BINDING_SCENE, false),
                buffer(abi::BINDING_INSTANCES, true),
                // Always here, whether or not the material being drawn
                // declares anything: a frame group whose shape changed
                // per material would invalidate every pipeline layout
                // built against it.
                buffer(abi::BINDING_INSTANCE_ATTRIBUTES, true),
            ],
        });
        let mut bindings = FrameBindings {
            camera,
            scene,
            instances,
            capacity,
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
        let bind_group = frame_bind_group(
            device,
            &self.layout,
            &self.camera,
            &self.scene,
            &self.instances,
            &buffer,
        );
        self.groups.insert(
            key.to_string(),
            InstanceGroup {
                buffer,
                capacity: grown,
                stride,
                bind_group,
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
        for group in self.groups.values_mut() {
            group.bind_group = frame_bind_group(
                device,
                &self.layout,
                &self.camera,
                &self.scene,
                &self.instances,
                &group.buffer,
            );
        }
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
        queue.write_buffer(
            &self.camera,
            0,
            bytemuck::bytes_of(&environment.camera.uniform()),
        );
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
    pub fn instance_group(&self, signature: &str) -> &wgpu::BindGroup {
        self.groups
            .get(signature)
            .or_else(|| self.groups.get(BASE_SHAPE))
            .map(|group| &group.bind_group)
            .expect("the empty attribute shape is created up front")
    }

    /// The bind group for a material that declares no per-instance
    /// attributes.
    pub fn bind_group(&self) -> &wgpu::BindGroup {
        self.instance_group(BASE_SHAPE)
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

#[allow(clippy::too_many_arguments)]
fn frame_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    camera: &wgpu::Buffer,
    scene: &wgpu::Buffer,
    instances: &wgpu::Buffer,
    attributes: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("wxsl frame bindings"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: abi::BINDING_CAMERA,
                resource: camera.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: abi::BINDING_SCENE,
                resource: scene.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: abi::BINDING_INSTANCES,
                resource: instances.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: abi::BINDING_INSTANCE_ATTRIBUTES,
                resource: attributes.as_entire_binding(),
            },
        ],
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
        assert_eq!(size_of::<LightUniform>(), 32);
        assert_eq!(
            size_of::<SceneUniform>(),
            MAX_LIGHTS * 32 + 16 + 16 + 16,
            "scene uniform layout drifted from bindings.wxsl"
        );
        assert_eq!(size_of::<InstanceTransform>(), 128);
        for size in [
            size_of::<CameraUniform>(),
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
        assert_eq!(light.uniform().kind, 1.0);
    }
}
