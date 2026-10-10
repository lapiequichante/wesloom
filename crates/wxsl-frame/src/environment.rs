//! Camera, lights and frame values, without a device (ADR 0048).
//! Fixed host layouts are generated from `wxsl-core::host` (ADR 0050):
//! matrices are column-major, shadow slice -1 means no shadow, and the
//! previous camera/time defaults mean no motion.

use std::collections::BTreeMap;

use bytemuck::Zeroable;
use glam::{Mat4, Vec2, Vec3};
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
    /// Sub-pixel offset applied to the projection, in NDC units — the
    /// jitter a temporal technique (TAA above all) moves the camera by
    /// each frame so that geometric edges land between samples instead of
    /// on them. Zero is no jitter, which is every frame this repo drew
    /// before the field existed. A sequence of small values (Halton(2, 3)
    /// scaled to the pixel size is the usual) is what turns a velocity
    /// buffer plus a history into antialiasing; the same values fed to
    /// `previous_camera` are what keep the velocity buffer honest about
    /// it, because the jitter cancels in the difference of the two
    /// frames' clips.
    pub jitter: Vec2,
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
            jitter: Vec2::ZERO,
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
        let mut projection = glam::camera::rh::proj::directx::perspective(
            self.fov_y,
            self.aspect.max(1e-3),
            self.near,
            self.far,
        );
        // The jitter rides the projection's z column, so a clip position
        // moves by `jitter * z` — the standard cheap sub-pixel shift,
        // which is exact at every depth the way a post-translate would
        // not be.
        projection.z_axis.x += self.jitter.x;
        projection.z_axis.y += self.jitter.y;
        projection * glam::camera::rh::view::look_at_mat4(self.eye, self.target, self.up)
    }

    /// The uniform this camera fills in, answering for no previous frame.
    pub fn uniform(&self) -> CameraUniform {
        CameraUniform::new(self.view_proj(), self.eye)
    }

    /// The uniform this camera fills in when `previous` is where it was
    /// last frame. `None` — the camera did not move, or nobody is keeping
    /// score — carries this frame's own matrix and position there, which
    /// is zero motion and the behaviour every frame had before the field
    /// existed.
    pub fn uniform_with(&self, previous: Option<&Camera>) -> CameraUniform {
        match previous {
            Some(previous) => CameraUniform::new_with_previous(
                self.view_proj(),
                previous.view_proj(),
                self.eye,
                previous.eye,
            ),
            None => self.uniform(),
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
    /// Where the camera was on the previous frame.
    ///
    /// The velocity stage transforms every vertex against both frames'
    /// cameras; without this it would see object motion only, and a
    /// moving camera would drag a wrong motion vector across a still
    /// scene. `None` — the default — says the camera did not move, which
    /// is the behaviour every frame had before the field existed. An
    /// application that moves the camera and renders velocity fills it
    /// with what the camera was, exactly as [`Environment::advance`]
    /// fills `previous_time` with what the clock was: "the previous
    /// frame" is a fact about the sequence of frames, and getting it out
    /// of step is silent.
    pub previous_camera: Option<Camera>,
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
            previous_camera: None,
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
        let camera = self.camera.uniform_with(self.previous_camera.as_ref());
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

include!(concat!(env!("OUT_DIR"), "/host.rs"));

impl CameraUniform {
    /// One point of view: its world-to-clip matrix and where it sits.
    ///
    /// Not only the camera's — a shadow pass fills one of these from the
    /// light it renders for, which is what lets the shader keep reading
    /// `camera` and mean whichever view the pass named. The previous
    /// frame's halves answer "this view did not move".
    pub fn new(view_proj: Mat4, position: Vec3) -> Self {
        CameraUniform::new_with_previous(view_proj, view_proj, position, position)
    }

    /// One point of view, with what it was last frame stated explicitly.
    pub fn new_with_previous(
        view_proj: Mat4,
        previous_view_proj: Mat4,
        position: Vec3,
        previous_position: Vec3,
    ) -> Self {
        CameraUniform {
            view_proj: view_proj.to_cols_array_2d(),
            inverse_view_proj: view_proj.inverse().to_cols_array_2d(),
            position: position.to_array(),
            _padding: 0.0,
            previous_view_proj: previous_view_proj.to_cols_array_2d(),
            previous_position: previous_position.to_array(),
            _padding1: 0.0,
        }
    }
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

/// Camera-space depth of a point along the view direction — the sort key
/// a pass's draw order hangs on (plan5 D3). Positive in front of the
/// camera, larger is farther. Pure, device-free, and shared by every
/// sorting pass so two passes cannot disagree about "nearest".
pub fn view_depth(origin: Vec3, eye: Vec3, forward: Vec3) -> f32 {
    (origin - eye).dot(forward)
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
        assert_eq!(size_of::<CameraUniform>(), 64 + 64 + 16 + 64 + 16);
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

    #[test]
    fn view_depth_orders_points_along_the_view() {
        // Looking down -z: nearer points have smaller depth, off-axis
        // points sort by their component along the view.
        let eye = Vec3::ZERO;
        let forward = Vec3::new(0.0, 0.0, -1.0);
        let near = view_depth(Vec3::new(0.3, 0.0, -2.0), eye, forward);
        let far = view_depth(Vec3::new(0.0, 2.0, -8.0), eye, forward);
        let behind = view_depth(Vec3::new(0.0, 0.0, 3.0), eye, forward);
        assert!(near < far);
        assert!(behind < near, "behind the camera is nearer than anything");
    }
}
