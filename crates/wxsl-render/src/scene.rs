//! What is being looked at, and the uniform buffers that carry it.
//!
//! The `#[repr(C)]` structs here are the host side of the ABI's
//! `shaders/wxsl/bindings.wxsl`: same fields, same order, same padding.
//! WGSL aligns a `vec3f` to 16 bytes, so every one is followed by an explicit
//! scalar rather than relying on the compiler to insert it. A test checks the
//! sizes, which is the part that silently corrupts every frame when it drifts.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wxsl_core::abi;

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

/// The whole scene: camera, lights, ambient environment, exposure, time.
#[derive(Clone, Debug)]
pub struct Scene {
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

impl Default for Scene {
    fn default() -> Self {
        Scene {
            camera: Camera::default(),
            lights: Vec::new(),
            ambient_sky: Vec3::new(0.32, 0.40, 0.55),
            ambient_ground: Vec3::new(0.10, 0.08, 0.07),
            exposure: 1.0,
            time: 0.0,
        }
    }
}

impl Scene {
    /// The uniform this scene fills in.
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

/// Host mirror of `Object` in `shaders/wxsl/bindings.wxsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ObjectUniform {
    /// Object-to-world matrix, column-major.
    pub model: [[f32; 4]; 4],
    /// Inverse transpose of `model`, as a 4x4 so its uniform layout needs no
    /// per-column padding. Only the upper 3x3 is read.
    pub normal_matrix: [[f32; 4]; 4],
}

impl ObjectUniform {
    /// The uniform for an object with the given model matrix.
    ///
    /// The normal matrix is the inverse transpose, so non-uniform scaling
    /// does not shear the normals off the surface.
    pub fn new(model: Mat4) -> Self {
        ObjectUniform {
            model: model.to_cols_array_2d(),
            normal_matrix: model.inverse().transpose().to_cols_array_2d(),
        }
    }
}

/// The uniform buffers and bind group for the frame group
/// ([`abi::GROUP_FRAME`]).
///
/// One instance is shared by every pipeline: the bindings are the same for
/// forward, for the deferred material pass, and for the deferred lighting
/// pass, so the layout is created once and reused.
///
/// The object transform shares this group even though it changes per draw
/// ([ADR 0010](../../../docs/adr/0010-four-bind-groups-allocated-by-update-frequency.md)).
/// It is still a whole-buffer binding rather than a dynamic offset, which is
/// exactly right for the one-object demo and is where a multi-draw scene
/// would switch `has_dynamic_offset` on without moving the binding.
pub struct SceneBindings {
    camera: wgpu::Buffer,
    scene: wgpu::Buffer,
    object: wgpu::Buffer,
    layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
}

impl SceneBindings {
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
        let object = uniform("wxsl object", size_of::<ObjectUniform>() as u64);

        let entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wxsl scene bindings"),
            entries: &[
                entry(abi::BINDING_CAMERA),
                entry(abi::BINDING_SCENE),
                entry(abi::BINDING_OBJECT),
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wxsl scene bindings"),
            layout: &layout,
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
                    binding: abi::BINDING_OBJECT,
                    resource: object.as_entire_binding(),
                },
            ],
        });

        SceneBindings {
            camera,
            scene,
            object,
            layout,
            bind_group,
        }
    }

    /// Upload `scene` and the object's transform.
    pub fn update(&self, queue: &wgpu::Queue, scene: &Scene, model: glam::Mat4) {
        queue.write_buffer(&self.camera, 0, bytemuck::bytes_of(&scene.camera.uniform()));
        queue.write_buffer(&self.scene, 0, bytemuck::bytes_of(&scene.uniform()));
        queue.write_buffer(
            &self.object,
            0,
            bytemuck::bytes_of(&ObjectUniform::new(model)),
        );
    }

    /// The bind group layout, for building pipeline layouts.
    pub fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    /// The bind group to set at index 0.
    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(size_of::<ObjectUniform>(), 128);
        for size in [
            size_of::<CameraUniform>(),
            size_of::<SceneUniform>(),
            size_of::<ObjectUniform>(),
        ] {
            assert_eq!(size % 16, 0);
        }
    }

    #[test]
    fn extra_lights_are_dropped_rather_than_overrunning_the_array() {
        let mut scene = Scene::default();
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
