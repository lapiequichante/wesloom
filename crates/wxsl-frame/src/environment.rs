//! Camera, lights and frame values, without a device (ADR 0048).
//! Fixed host layouts are generated from `wxsl-core::host` (ADR 0050):
//! matrices are column-major, a zero shadow rectangle means no shadow, and the
//! previous camera/time defaults mean no motion.

use std::collections::BTreeMap;

use bytemuck::Zeroable;
use glam::{DMat4, DVec3, Mat4, Vec2, Vec3};
use wxsl_core::abi;
use wxsl_core::resources::BufferLayout;
use wxsl_core::scene::TagExpr;

use crate::light_grid::{GridMapping, LightGrid};
use crate::pass::PassView;

/// Maximum lights the scene uniform carries.
///
/// The ABI owns the number, because it is the length of an array in a
/// host-shared buffer.
pub const MAX_LIGHTS: usize = abi::MAX_LIGHTS;

/// Most cascades a directional light may split its shadow into.
///
/// A cascade is one atlas tile, so this is bounded by the tile budget and
/// by sense: past four, the outer cascades are blurred beyond what the
/// normal-offset bias can hide from the inner ones.
pub const MAX_SHADOW_CASCADES: u32 = 4;

// Keep in sync with WXSL_SHADOW_CASCADE_BLEND in shadow.wxsl.
const SHADOW_CASCADE_BLEND: f32 = 0.1;

/// One atlas slot this frame: who owns it, and where it looks.
///
/// A light claims as many of these as its shadow kind needs — one per
/// cascade, six per point light — so the shadow pass list (one pass per
/// slot, always) and the sampling side (`scene.shadow_tiles`, addressed by
/// the light's `tile_first` + the kind's selection) agree by construction:
/// both read this vector, in this order.
#[derive(Clone, Copy, Debug)]
pub struct ShadowTileView {
    /// Index of the light that claimed this slot.
    pub light: u32,
    /// World-to-clip for the shadow pass and the fragment's lookup alike.
    pub view_proj: Mat4,
    /// Where the tile's view sits — the cube face's centre or the cascade
    /// box's anchor.
    pub eye: Vec3,
    /// The tile's split distance — see [`ShadowView::split`].
    pub split: f32,
}

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

/// Which side of the hard edge a soft shadow's penumbra lies on
/// (ADR 0105).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Penumbra {
    /// Straddling the edge, as a source that size casts: half of it
    /// lights the shadow's inside.
    #[default]
    Inside,
    /// Wholly outside the shadow, whose inside stays dark; a contact
    /// shadow inside a far caster's penumbra keeps its own width.
    Outside,
}

/// What kind of light a [`Light`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightKind {
    /// Radiates from a point, with inverse-square falloff.
    Point,
    /// One-sided rectangular emitter: sixteen shadowed surface points
    /// (ADR 0072), or, casting no shadow, samples that follow the specular
    /// lobe (ADR 0095).
    Rectangle,
    /// Finite cone with smooth falloff and an optional projected cookie.
    Spot,
    /// Infinitely far away: one direction, no falloff.
    Directional,
    /// A glowing cylinder, sampled along its axis around the specular lobe;
    /// never shadowed (ADR 0095).
    Tube,
    /// A one-sided disc, or ellipse, integrated like an unshadowed
    /// rectangle; never shadowed (ADR 0104).
    Disc,
}

/// Where a light renders one shadow tile from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowView {
    /// World space to the light's clip space.
    pub view_proj: Mat4,
    /// Where the light's view sits, which is what a graph reading
    /// `view_direction` in a shadow pass sees.
    pub eye: Vec3,
    /// How far from the camera this tile's coverage reaches — the split
    /// that sends a fragment to it. A cascade's far distance; zero on
    /// every tile that needs no selection (a single box, a cube face).
    pub split: f32,
}

/// One light.
///
/// `Clone` and not `Copy`, since [`Light::shadow_casters`] is a tag
/// expression (ADR 0101).
#[derive(Clone, Debug)]
pub struct Light {
    /// Point or directional.
    pub kind: LightKind,
    /// World position for a point light, or the direction *towards* the light
    /// for a directional one.
    pub position_or_direction: Vec3,
    /// Linear-light colour.
    pub color: Vec3,
    /// Radiant intensity multiplier, or emitted radiance for a rectangle,
    /// a tube or a disc.
    pub intensity: f32,
    /// Rectangle half-edge vectors, or a disc's semi-axes; cross(u, v)
    /// points towards its emitting side.
    pub area_u: Vec3,
    /// Second perpendicular half-edge of a rectangle, or semi-axis of a
    /// disc; zero for other lights.
    pub area_v: Vec3,
    /// A tube's radius (its half-axis is `area_u`), or the source radius
    /// a point, spot or directional light's shadow softens from: world
    /// units for a point or spot, the tangent of the angular radius for a
    /// directional light. Zero keeps the fixed-kernel shadow (ADR 0098).
    pub radius: f32,
    /// Which side of the hard edge a soft shadow's penumbra lies on.
    pub penumbra: Penumbra,
    /// How many times the base taps a soft shadow's blocker search and
    /// filter take: 1, 2 or 4. More is less grain, at that cost.
    pub shadow_taps: u32,
    /// Direction away from a spotlight, normalized by its constructor.
    pub spot_direction: Vec3,
    /// Inner cone half-angle in radians, fully illuminated.
    pub spot_inner_angle: f32,
    /// Outer cone half-angle in radians, at which illumination reaches zero.
    pub spot_outer_angle: f32,
    /// Optional layer of the application-supplied spotlight cookie texture.
    pub cookie_layer: Option<u32>,
    /// Optional layer of the cookie texture holding a point or spot
    /// light's IES profile, as [`crate::ies::IesProfile::texels`] lays it
    /// out (ADR 0097).
    pub profile_layer: Option<u32>,
    /// Whether this light renders a shadow map.
    ///
    /// A directional light renders one tile per cascade
    /// ([`Light::shadow_cascades`]); a point light renders six, one per
    /// cube face. A rectangle renders six faces per quadrature point (96).
    /// All kinds claim their tiles from one atlas budget
    /// ([`abi::MAX_SHADOW_TILES`]), and a light whose tiles do not fit
    /// casts none rather than a partial one —
    /// [`Environment::shadow_tiles`] is where that is decided.
    pub casts_shadow: bool,
    /// Half the edge of the world-space box a single-cascade directional
    /// light shadows, centred on the origin.
    ///
    /// Fixed and centred rather than fitted to the camera: fitting the box
    /// to the view is what cascades do, and they are the knob that makes a
    /// shadow follow a view without blurring into nothing — set
    /// [`Light::with_cascades`] above one and this field stops being read.
    /// At one cascade it is the knob that trades area for texels.
    pub shadow_extent: f32,
    /// How deep the light's view is, along its own direction — the outer
    /// range the last cascade reaches, or the point light's far plane.
    pub shadow_distance: f32,
    /// How far along the surface normal a lookup is nudged before it is
    /// compared, in world units. See `shaders/wxsl/shadow.wxsl`.
    pub shadow_normal_bias: f32,
    /// How many cascades a directional light splits its shadow into.
    ///
    /// One — the default — is a single origin-centred box, the shape this
    /// light has always had. More than one fits each cascade to the
    /// camera's frustum over a slice of its depth range, near slices
    /// sharpest, and the shadow follows the camera. Clamped to
    /// [`MAX_SHADOW_CASCADES`]; meaningless for a point or area light, whose
    /// faces are not negotiable.
    pub shadow_cascades: u32,
    /// How the cascades split the camera's depth: 0 evenly, 1 by the same
    /// ratio each, which keeps the near ones sharp however far the last
    /// reaches. 0.5 — the default — is halfway between.
    pub shadow_cascade_split: f32,
    /// How far towards a cascaded light, past the nearest point a cascade
    /// receives on, its casters are still drawn, in world units. A tall
    /// caster up-light shadows a slice from much further away than the
    /// slice is deep: a 300 m tower under a sun 20° up reaches from 900 m.
    /// Zero — the default — is [`Light::shadow_distance`].
    pub shadow_caster_reach: f32,
    /// A point or spot light's shadow near plane, in world units: what is
    /// nearer the light than this casts no shadow. Zero — the default — is
    /// a thousandth of [`Light::shadow_distance`], at least 0.01. A light
    /// inside a housing that should not shadow it (a bulb behind its
    /// lens) sets it to the housing's depth.
    pub shadow_near: f32,
    /// Where a point, spot or disc light's falloff reaches zero, in world
    /// units.
    ///
    /// Zero — the default — is the unbounded inverse square every light
    /// had before the field. A finite range windows the falloff to exactly
    /// zero there, and a ranged light that casts no shadow is a *local*
    /// light: it leaves the scene uniform's [`MAX_LIGHTS`] slots for the
    /// clustered list, which is how a frame carries thousands of them
    /// ([`crate::light_grid`], ADR 0076).
    pub range: f32,
    /// Which draws this light's shadow passes draw, by their tags.
    /// Every draw by default.
    pub shadow_casters: TagExpr,
    /// The frame a static shadow's casters move in, or `None` for a
    /// shadow redrawn every frame (ADR 0101).
    ///
    /// A light that moves rigidly with everything it shadows (a car's
    /// headlight, over the car's tags in [`Light::shadow_casters`]) has its
    /// tile drawn once and kept while the light, seen in this frame,
    /// stays where it was. Spot lights only: see [`Light::with_static_shadow`].
    pub static_shadow: Option<Mat4>,
}

impl Light {
    /// The shadow settings every light starts with: none.
    ///
    /// Off by default, because a shadow map is a pass per tile and an
    /// application that has not asked for one should not pay for the
    /// atlas.
    pub const DEFAULTS: Light = Light {
        kind: LightKind::Directional,
        position_or_direction: Vec3::Y,
        color: Vec3::ONE,
        intensity: 1.0,
        area_u: Vec3::ZERO,
        area_v: Vec3::ZERO,
        radius: 0.0,
        penumbra: Penumbra::Inside,
        shadow_taps: 1,
        spot_direction: Vec3::NEG_Y,
        spot_inner_angle: 0.4,
        spot_outer_angle: 0.7,
        cookie_layer: None,
        profile_layer: None,
        casts_shadow: false,
        shadow_extent: 8.0,
        shadow_distance: 32.0,
        shadow_normal_bias: 0.03,
        shadow_cascades: 1,
        shadow_cascade_split: 0.5,
        shadow_caster_reach: 0.0,
        shadow_near: 0.0,
        range: 0.0,
        shadow_casters: TagExpr::Always,
        static_shadow: None,
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

    /// A one-sided rectangle with perpendicular, nonzero half-edges.
    /// `radiance` is emitted radiance, so flux grows with the rectangle's area.
    /// Direct lighting and visibility use sixteen Gauss quadrature points (ADR 0072).
    pub fn rectangle(centre: Vec3, half_u: Vec3, half_v: Vec3, color: Vec3, radiance: f32) -> Self {
        assert!(centre.is_finite() && half_u.is_finite() && half_v.is_finite());
        let product = half_u.length() * half_v.length();
        assert!(
            product > 1e-6 && half_u.dot(half_v).abs() <= product * 1e-4,
            "rectangle half-edges must be nonzero and perpendicular"
        );
        Light {
            kind: LightKind::Rectangle,
            position_or_direction: centre,
            area_u: half_u,
            area_v: half_v,
            color,
            intensity: radiance,
            ..Light::DEFAULTS
        }
    }

    /// A tube from `centre - half_axis` to `centre + half_axis`, `radius`
    /// thick, emitting `radiance` from its surface. It casts no shadow and
    /// is never local (ADR 0095).
    pub fn tube(centre: Vec3, half_axis: Vec3, radius: f32, color: Vec3, radiance: f32) -> Self {
        assert!(centre.is_finite() && half_axis.is_finite() && radius.is_finite());
        assert!(
            half_axis.length_squared() > 1e-8 && radius > 0.0,
            "a tube needs a nonzero axis and radius"
        );
        Light {
            kind: LightKind::Tube,
            position_or_direction: centre,
            area_u: half_axis,
            radius,
            color,
            intensity: radiance,
            ..Light::DEFAULTS
        }
    }

    /// A one-sided disc of `radius` around `centre`, emitting `radiance`
    /// towards `normal`. Seen through LTC it is the octagon of its area;
    /// sampled, the rectangle around it, masked. It casts no shadow and is
    /// never local, but a range bounds it (ADR 0104).
    pub fn disc(centre: Vec3, normal: Vec3, radius: f32, color: Vec3, radiance: f32) -> Self {
        assert!(centre.is_finite() && normal.is_finite() && radius.is_finite());
        assert!(
            normal.length_squared() > 1e-8 && radius > 0.0,
            "a disc needs a nonzero normal and radius"
        );
        let (u, v) = normal.normalize().any_orthonormal_pair();
        Light {
            kind: LightKind::Disc,
            position_or_direction: centre,
            area_u: u * radius,
            area_v: v * radius,
            color,
            intensity: radiance,
            ..Light::DEFAULTS
        }
    }

    /// The sixteen points used by rectangular lighting and its shadow views.
    pub fn area_sample_positions(&self) -> [Vec3; 16] {
        // Four-point Gauss-Legendre nodes; same order as bindings.wxsl.
        let nodes = [-0.8611363, -0.33998105, 0.33998105, 0.8611363];
        std::array::from_fn(|sample| {
            self.position_or_direction
                + self.area_u * nodes[sample % 4]
                + self.area_v * nodes[sample / 4]
        })
    }

    /// A spotlight. Cone angles are half-angles in radians, with
    /// 0 <= inner < outer < pi/2. Direction points away from the source.
    pub fn spot(
        position: Vec3,
        direction: Vec3,
        inner: f32,
        outer: f32,
        color: Vec3,
        intensity: f32,
    ) -> Self {
        assert!(position.is_finite() && direction.is_finite() && direction.length_squared() > 1e-6);
        assert!(
            inner.is_finite()
                && outer.is_finite()
                && inner >= 0.0
                && inner < outer
                && outer < core::f32::consts::FRAC_PI_2
        );
        Light {
            kind: LightKind::Spot,
            position_or_direction: position,
            spot_direction: direction.normalize(),
            spot_inner_angle: inner,
            spot_outer_angle: outer,
            color,
            intensity,
            ..Light::DEFAULTS
        }
    }

    /// Project this layer of Renderer::set_light_cookies through a spot,
    /// or emit it from a rectangle or the square around a disc (u along
    /// `area_u`, v down `area_v`) or along a tube (u along its axis, the
    /// middle row), prefiltered to the samples' spacing (ADR 0095).
    pub fn with_cookie(mut self, layer: u32) -> Self {
        assert!(
            matches!(
                self.kind,
                LightKind::Spot | LightKind::Rectangle | LightKind::Tube | LightKind::Disc
            ),
            "cookies require a spotlight, a rectangle, a tube or a disc"
        );
        self.cookie_layer = Some(layer);
        self
    }

    /// Shape this point or spot light's intensity by the IES profile in
    /// this layer of Renderer::set_light_cookies: γ = 0 along
    /// [`Light::spot_direction`] (straight down for a point light), C = 0
    /// to its right, C = 90° up (ADR 0097). A spot's cone still applies.
    pub fn with_profile(mut self, layer: u32) -> Self {
        assert!(
            matches!(self.kind, LightKind::Point | LightKind::Spot),
            "IES profiles require a point or spot light"
        );
        self.profile_layer = Some(layer);
        self
    }

    fn spot_view(&self) -> ShadowView {
        let eye = self.position_or_direction;
        let direction = self.spot_direction;
        let up = if direction.y.abs() > 0.99 {
            Vec3::Z
        } else {
            Vec3::Y
        };
        let far = self.shadow_distance.max(0.1);
        let near = self.shadow_near_plane(far);
        let projection = glam::camera::rh::proj::directx::perspective(
            self.spot_outer_angle * 2.0,
            1.0,
            near,
            far,
        );
        ShadowView {
            view_proj: projection * glam::camera::rh::view::look_at_mat4(eye, eye + direction, up),
            eye,
            split: 0.0,
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
    ///
    /// For a directional light, `extent` is half the box's edge. A point
    /// light reads nothing from it: its six faces always cover every
    /// direction, and [`Light::shadow_distance`] is their far plane.
    pub fn casting_shadow(mut self, extent: f32) -> Self {
        self.casts_shadow = true;
        self.shadow_extent = extent.max(1e-3);
        self
    }

    /// Split the directional light's shadow into `count` cascades fitted
    /// to the camera, nearest slice sharpest.
    ///
    /// Clamped to [`MAX_SHADOW_CASCADES`]; one or none means the ordinary
    /// single box.
    pub fn with_cascades(mut self, count: u32) -> Self {
        self.shadow_cascades = count.clamp(1, MAX_SHADOW_CASCADES);
        self
    }

    /// Split the cascades' depth `ratio` of the way from even (0) to
    /// logarithmic (1); see [`Light::shadow_cascade_split`].
    pub fn with_cascade_split(mut self, ratio: f32) -> Self {
        self.shadow_cascade_split = ratio.clamp(0.0, 1.0);
        self
    }

    /// Draw a cascaded light's casters up to `reach` towards it; see
    /// [`Light::shadow_caster_reach`].
    pub fn with_caster_reach(mut self, reach: f32) -> Self {
        self.shadow_caster_reach = reach.max(0.0);
        self
    }

    /// Start a point or spot light's shadow `near` world units from it;
    /// see [`Light::shadow_near`].
    pub fn with_shadow_near(mut self, near: f32) -> Self {
        self.shadow_near = near.max(0.0);
        self
    }

    /// The near plane of a point or spot light's shadow views.
    fn shadow_near_plane(&self, far: f32) -> f32 {
        if self.shadow_near > 0.0 {
            self.shadow_near.min(far * 0.5)
        } else {
            (far * 1e-3).max(0.01)
        }
    }

    /// Bound the light's reach at `range` world units (see
    /// [`Light::range`]); zero or less keeps it unbounded.
    pub fn with_range(mut self, range: f32) -> Self {
        self.range = range.max(0.0);
        self
    }

    /// Whether this light goes to the clustered list rather than the scene
    /// uniform: a ranged point or spot light that casts no shadow.
    pub fn is_local(&self) -> bool {
        self.range > 0.0
            && matches!(self.kind, LightKind::Point | LightKind::Spot)
            && !self.casts_shadow
    }

    /// Soften this point, spot or directional light's shadow as from a
    /// source of this radius: sharp where a shadow touches its caster,
    /// wider the farther the receiver is behind it (ADR 0098). World
    /// units for a point or spot; for a directional light, its angular
    /// radius in radians (the sun's is about 0.0047).
    pub fn with_source_radius(mut self, radius: f32) -> Self {
        assert!(
            matches!(
                self.kind,
                LightKind::Point | LightKind::Spot | LightKind::Directional
            ),
            "a source radius softens a point, spot or directional shadow"
        );
        assert!(radius.is_finite() && radius >= 0.0);
        self.radius = match self.kind {
            LightKind::Directional => radius.min(1.5).tan(),
            _ => radius,
        };
        self
    }

    /// Put a soft shadow's penumbra on `side` of its hard edge; see
    /// [`Penumbra`].
    pub fn with_penumbra(mut self, side: Penumbra) -> Self {
        self.penumbra = side;
        self
    }

    /// Take `scale` times the base taps in a soft shadow: 1, 2 or 4.
    pub fn with_shadow_taps(mut self, scale: u32) -> Self {
        assert!(matches!(scale, 1 | 2 | 4), "shadow taps scale by 1, 2 or 4");
        self.shadow_taps = scale;
        self
    }

    /// Draw this light's shadow from the draws `casters` matches only.
    pub fn with_shadow_casters(mut self, casters: TagExpr) -> Self {
        self.shadow_casters = casters;
        self
    }

    /// Make this spot light's shadow static: drawn once, and kept while
    /// it and its casters move together in `frame`, the transform of
    /// everything [`Light::shadow_casters`] matches (ADR 0101).
    ///
    /// Spot lights only. A point light picks its cube face by world axis,
    /// which a frame that turns would leave pointing at the wrong face.
    pub fn with_static_shadow(mut self, frame: Mat4) -> Self {
        assert!(
            self.kind == LightKind::Spot,
            "only a spot light's shadow can be static"
        );
        assert!(frame.is_finite() && frame.determinant().abs() > 1e-12);
        self.static_shadow = Some(frame);
        self
    }

    /// Set the normal-offset bias, in world units.
    pub fn with_normal_bias(mut self, bias: f32) -> Self {
        self.shadow_normal_bias = bias;
        self
    }

    /// The points of view this light renders its shadow tiles from, in
    /// tile order — near cascades first for a directional light, the
    /// +x,-x,+y,-y,+z,-z faces for a point light.
    ///
    /// Empty when the light casts none. `directx` is the same NDC
    /// convention [`Camera::view_proj`] uses, so a depth written by one
    /// and compared by the other means the same thing.
    pub fn shadow_views(&self, camera: &Camera) -> Vec<ShadowView> {
        self.shadow_views_at(camera, 0, abi::SHADOW_ATLAS_SIZE)
    }

    /// [`Light::shadow_views`] from tile `first_tile` of an atlas `atlas`
    /// texels wide, whose tiles the cascades snap to.
    fn shadow_views_at(&self, camera: &Camera, first_tile: u32, atlas: u32) -> Vec<ShadowView> {
        if !self.casts_shadow {
            return Vec::new();
        }
        let direction = self.position_or_direction.normalize_or_zero();
        match self.kind {
            LightKind::Point => self.face_views(),
            LightKind::Spot => vec![self.spot_view()],
            LightKind::Rectangle => self
                .area_sample_positions()
                .into_iter()
                .flat_map(|position| {
                    let mut sample = self.clone();
                    sample.position_or_direction = position;
                    sample.face_views()
                })
                .collect(),
            LightKind::Tube | LightKind::Disc => Vec::new(),
            LightKind::Directional => {
                if direction.length_squared() < 1e-6 {
                    return Vec::new();
                }
                if self.shadow_cascades <= 1 {
                    vec![self.origin_box(&direction)]
                } else {
                    self.cascade_views(camera, &direction, first_tile, atlas)
                }
            }
        }
    }

    /// The six cube-face views of a point light.
    ///
    /// One 90° perspective per face; the fragment picks its face by
    /// dominant axis of the surface-to-light direction, the same order
    /// [`CUBE_FACE_DIRECTIONS`] spells.
    fn face_views(&self) -> Vec<ShadowView> {
        let eye = self.position_or_direction;
        let far = self.shadow_distance.max(0.1);
        let near = self.shadow_near_plane(far);
        CUBE_FACE_DIRECTIONS
            .map(|face| {
                // An up not parallel to the face: the ±y faces look straight
                // down or up, and would degenerate on `Vec3::Y`.
                let up = if face.y.abs() > 0.9 {
                    -Vec3::Z
                } else {
                    Vec3::Y
                };
                let projection = glam::camera::rh::proj::directx::perspective(
                    core::f32::consts::FRAC_PI_2,
                    1.0,
                    near,
                    far,
                );
                ShadowView {
                    view_proj: projection
                        * glam::camera::rh::view::look_at_mat4(eye, eye + face, up),
                    eye,
                    split: 0.0,
                }
            })
            .to_vec()
    }

    /// The single-cascade box this light has always had: centred on the
    /// origin, deep enough to contain what is in front of it.
    fn origin_box(&self, direction: &Vec3) -> ShadowView {
        let half = self.shadow_extent.max(1e-3);
        let depth = self.shadow_distance.max(half);
        // `position_or_direction` points *towards* the light, so the eye
        // sits that way from the origin and looks back at it.
        let eye = *direction * depth;
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
        ShadowView {
            view_proj: projection * glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, up),
            eye,
            split: 0.0,
        }
    }

    /// One fitted box per cascade: the camera frustum split over its depth
    /// range, each slice's bounding sphere snapped to its tile's texel
    /// grid in the light's own space.
    ///
    /// The light's view *orientation* is fixed — the eye stays pinned on
    /// the light's axis — so the snapping is stable while the camera
    /// moves, which is the whole point: without it, every sub-texel step
    /// of the camera would swim the shadow across the surface.
    fn cascade_views(
        &self,
        camera: &Camera,
        direction: &Vec3,
        first_tile: u32,
        atlas: u32,
    ) -> Vec<ShadowView> {
        let depth = self.shadow_distance.max(1e-3);
        let reach = if self.shadow_caster_reach > 0.0 {
            self.shadow_caster_reach
        } else {
            depth
        };
        let eye = *direction * depth;
        let up = if direction.y.abs() > 0.99 {
            Vec3::Z
        } else {
            Vec3::Y
        };
        let light_view = glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, up);
        let forward = (camera.target - camera.eye).normalize_or_zero();
        let right = forward.cross(camera.up).normalize_or_zero();
        let cam_up = right.cross(forward).normalize_or_zero();
        let tan_h = (camera.fov_y * 0.5).tan();
        let tan_w = tan_h * camera.aspect.max(1e-3);
        let near = camera.near.max(1e-3);
        let range = camera.far.min(depth).max(near * 2.0);
        let count = self.shadow_cascades;
        let mut out = Vec::with_capacity(count as usize);
        // The splits and the slices' spheres depend on the range and the
        // lens only, never on the near plane: a chase camera's moves it a
        // little every frame, and every new sphere was a new texel size
        // that shifted a cascade's whole snapped grid, so its shadows
        // popped. The logarithmic splits start from a thousandth of the
        // range, the first slice from the eye.
        let reference = range * 1e-3;
        let mut d_near = 0.0;
        let mut slice_near = 0.0;
        for index in 0..count {
            // Every logarithmic split uses the same reference, not the
            // previous split, so the final split ends exactly at range.
            let t = (index + 1) as f32 / count as f32;
            let log = reference * (range / reference).powf(t);
            let lin = range * t;
            let ratio = self.shadow_cascade_split.clamp(0.0, 1.0);
            let d_far = log * ratio + lin * (1.0 - ratio);
            let centre = camera.eye + forward * ((slice_near + d_far) * 0.5);
            let corners: Vec<Vec3> = [slice_near, d_far]
                .into_iter()
                .flat_map(|d| {
                    [-1.0, 1.0].into_iter().flat_map(move |x| {
                        [-1.0, 1.0].into_iter().map(move |y| {
                            camera.eye
                                + forward * d
                                + right * (x * d * tan_w)
                                + cam_up * (y * d * tan_h)
                        })
                    })
                })
                .collect();
            // A sphere keeps the extent stable under camera rotation. Leave
            // a texel margin for snapping and the normal-offset lookup.
            let radius = corners
                .iter()
                .map(|p| p.distance(centre))
                .fold(0.0_f32, f32::max);
            let radius = (radius * 16.0).ceil().max(1.0) / 16.0;
            let edge = abi::shadow_tile_in(first_tile + index, atlas).edge as f32;
            let half = radius / (1.0 - 4.0 / edge);
            let texel = half * 2.0 / edge;
            let centre_ls = light_view.transform_point3(centre);
            let cx = (centre_ls.x / texel).round() * texel;
            let cy = (centre_ls.y / texel).round() * texel;
            let light_space: Vec<Vec3> = corners
                .iter()
                .map(|p| light_view.transform_point3(*p))
                .collect();
            let z_min = light_space
                .iter()
                .map(|p| p.z)
                .fold(f32::INFINITY, f32::min);
            let z_max = light_space
                .iter()
                .map(|p| p.z)
                .fold(f32::NEG_INFINITY, f32::max);
            // Include casters towards the light, even outside the camera
            // slice. Signed near distances also cover a camera past the
            // fixed light eye; clamping would silently clip its receivers.
            let projection = glam::camera::rh::proj::directx::orthographic(
                cx - half,
                cx + half,
                cy - half,
                cy + half,
                -z_max - reach,
                -z_min + texel * 2.0,
            );
            out.push(ShadowView {
                view_proj: projection * light_view,
                eye,
                split: d_far,
            });
            // The next cascade also covers this one's blend band.
            let previous_split = if index == 0 { 0.0 } else { d_near };
            slice_near = d_far - (d_far - previous_split) * SHADOW_CASCADE_BLEND;
            d_near = d_far;
        }
        out
    }

    pub(crate) fn uniform(&self, tile_first: u32, tile_count: u32) -> LightUniform {
        LightUniform {
            position_or_direction: self.position_or_direction.to_array(),
            kind: match self.kind {
                LightKind::Point => 0.0,
                LightKind::Rectangle => 2.0,
                LightKind::Spot => 3.0,
                LightKind::Directional => 1.0,
                LightKind::Tube => 4.0,
                LightKind::Disc => 5.0,
            },
            color: self.color.to_array(),
            intensity: self.intensity,
            shadow_normal_bias: self.shadow_normal_bias,
            tile_first,
            tile_count,
            range: self.range,
            area_u: self.area_u.to_array(),
            radius: self.radius,
            area_v: self.area_v.to_array(),
            profile_layer: self.profile_layer.unwrap_or(u32::MAX),
            spot_direction: self.spot_direction.to_array(),
            spot_outer_cos: self.spot_outer_angle.cos(),
            spot_inner_cos: self.spot_inner_angle.cos(),
            cookie_layer: self.cookie_layer.unwrap_or(u32::MAX),
            penumbra: match self.penumbra {
                Penumbra::Inside => 0.0,
                Penumbra::Outside => 1.0,
            },
            shadow_taps: self.shadow_taps,
        }
    }
}

/// The six cube-face directions of a point light's shadow, in tile order.
///
/// The fragment's face selection picks the dominant axis of the
/// surface-to-light direction with the same table, so a face never
/// renders into a slot the sampler cannot reach.
pub const CUBE_FACE_DIRECTIONS: [Vec3; 6] = [
    Vec3::X,
    Vec3::NEG_X,
    Vec3::Y,
    Vec3::NEG_Y,
    Vec3::Z,
    Vec3::NEG_Z,
];

/// A local probe's point of view (ADR 0102): six square 90° views from
/// `center`, one per cube face, which `PassView::Probe` renders from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalProbe {
    /// Where the cube is centred, relative to [`Environment::origin`].
    pub center: Vec3,
    /// Near plane distance.
    pub near: f32,
    /// Far plane distance.
    pub far: f32,
}

impl LocalProbe {
    /// The camera face `face` renders from, looking along
    /// [`CUBE_FACE_DIRECTIONS`]`[face]`. Its up vector is the face's
    /// downward texel axis, so that its image flipped in V is the WebGPU
    /// cube face: the flip is the composite pass's (`wxsl.probe_face`),
    /// which keeps the projection and the winding every other view has.
    pub fn face_camera(&self, face: usize) -> Camera {
        let direction = CUBE_FACE_DIRECTIONS[face % 6];
        let up = match face % 6 {
            2 => Vec3::Z,
            3 => Vec3::NEG_Z,
            _ => Vec3::NEG_Y,
        };
        Camera {
            eye: self.center,
            target: self.center + direction,
            up,
            fov_y: std::f32::consts::FRAC_PI_2,
            aspect: 1.0,
            near: self.near,
            far: self.far,
            jitter: Vec2::ZERO,
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
    /// Lights. Local ones ([`Light::is_local`]) go to the clustered list,
    /// at most [`abi::MAX_LOCAL_LIGHTS`]; of the rest, only the first
    /// [`MAX_LIGHTS`] reach the shader.
    pub lights: Vec<Light>,
    /// Whether local lights are clustered (ADR 0076). On — the default —
    /// they leave the uniform's slots for the light grid; off, every light
    /// competes for the [`MAX_LIGHTS`] slots in list order, as before the
    /// grid. The same picture either way for a frame whose lights fit:
    /// the switch is the grid's A/B instrument.
    pub cluster_lights: bool,
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
    /// Where this frame's coordinates are measured from, in the absolute
    /// world, in `f64` — the floating origin (plan3 M10).
    ///
    /// Every position the environment holds (the camera, its previous
    /// frame, the lights) and every draw transform is *relative* to it.
    /// Zero, the default, is the absolute world and every frame before
    /// the field. A scene far from the origin keeps its own positions in
    /// `f64` and lets [`Environment::look_from`] put the origin on the
    /// eye: model matrices are then pre-translated by the camera in `f64`
    /// ([`Environment::local_transform`]) and the view matrix carries no
    /// translation, so no large number ever becomes an `f32`. A material
    /// compiled with [`abi::FEATURE_RELATIVE_TO_EYE`] then sees small
    /// positions throughout; one without it still reads the absolute
    /// world, through the `f32` copy of this in the scene uniform, and
    /// pays that precision only in its shading, never in its geometry.
    pub origin: DVec3,
    /// The local probe the frame captures, if any (ADR 0102). `None` —
    /// the default — leaves its six view slots on the camera.
    pub probe: Option<LocalProbe>,
    /// The edge of the pipeline's shadow atlas, which its cascades snap
    /// to: the `shadow_atlas_size` of the `PipelineConfig` the frame was
    /// compiled with, [`abi::SHADOW_ATLAS_SIZE`] by default.
    pub shadow_atlas_size: u32,
}

impl Default for Environment {
    fn default() -> Self {
        Environment {
            camera: Camera::default(),
            lights: Vec::new(),
            cluster_lights: true,
            ambient_sky: Vec3::new(0.32, 0.40, 0.55),
            ambient_ground: Vec3::new(0.10, 0.08, 0.07),
            exposure: 1.0,
            time: 0.0,
            previous_time: 0.0,
            previous_camera: None,
            origin: DVec3::ZERO,
            probe: None,
            shadow_atlas_size: abi::SHADOW_ATLAS_SIZE,
        }
    }
}

impl Environment {
    /// Put the camera at `eye`, looking at `target`, both absolute, and
    /// move the floating origin onto the eye — relative-to-eye rendering's
    /// host half (plan3 M10). The camera's own translation is then zero,
    /// so the view matrix has none. Draw transforms, lights and the
    /// previous camera must be expressed against the new origin, through
    /// [`Environment::local_transform`] and [`Environment::local_point`].
    pub fn look_from(&mut self, eye: DVec3, target: DVec3) {
        self.origin = eye;
        self.camera.eye = Vec3::ZERO;
        self.camera.target = (target - eye).as_vec3();
    }

    /// An absolute `f64` position, measured from [`Environment::origin`].
    pub fn local_point(&self, absolute: DVec3) -> Vec3 {
        (absolute - self.origin).as_vec3()
    }

    /// An absolute `f64` object-to-world transform, pre-translated by
    /// [`Environment::origin`] before it becomes an `f32` — the form a
    /// draw's transform takes in a rebased frame.
    pub fn local_transform(&self, absolute: DMat4) -> Mat4 {
        (DMat4::from_translation(-self.origin) * absolute).as_mat4()
    }

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

    /// The atlas slots this frame claims, in slot order.
    ///
    /// Lights claim their tiles in list order from one budget of
    /// [`abi::MAX_SHADOW_TILES`]: a directional light takes one per
    /// cascade, a point light six, a rectangle 96. A light whose tiles do
    /// not fit is left with none — half a cube shadow or a missing near cascade would be
    /// worse than no shadow at all, and the pipeline's pass list is fixed,
    /// so "casts none" is the one answer that needs nothing rebuilt.
    pub fn shadow_tiles(&self) -> Vec<ShadowTileView> {
        let camera = self.camera;
        let mut tiles = Vec::new();
        for (index, light) in self.uniform_lights().into_iter().enumerate() {
            let views = light.shadow_views_at(&camera, tiles.len() as u32, self.shadow_atlas_size);
            if tiles.len() + views.len() > abi::MAX_SHADOW_TILES {
                break;
            }
            tiles.extend(views.into_iter().map(|view| ShadowTileView {
                light: index as u32,
                view_proj: view.view_proj,
                eye: view.eye,
                split: view.split,
            }));
        }
        tiles
    }

    /// The lights the scene uniform carries, in slot order: every light
    /// that is not local, or every light when clustering is off — the
    /// first [`MAX_LIGHTS`] of them.
    pub fn uniform_lights(&self) -> Vec<&Light> {
        self.lights
            .iter()
            .filter(|light| !(self.cluster_lights && light.is_local()))
            .take(MAX_LIGHTS)
            .collect()
    }

    /// The lights the light grid bins, in list order: the local ones,
    /// when clustering is on. Not truncated — [`crate::light_grid::LightGrid`]
    /// keeps the first [`abi::MAX_LOCAL_LIGHTS`] and counts the rest.
    pub fn local_lights(&self) -> Vec<&Light> {
        if !self.cluster_lights {
            return Vec::new();
        }
        self.lights
            .iter()
            .filter(|light| light.is_local())
            .collect()
    }

    /// How the light grid maps a position to its cell: the camera's own
    /// view and depth range.
    pub fn grid_mapping(&self) -> GridMapping {
        let near = self.camera.near.max(1e-4);
        GridMapping {
            view_proj: self.camera.view_proj(),
            near,
            far: self.camera.far.max(near * 1.001),
        }
    }

    /// This frame's clustered lights, binned against the camera — the
    /// bytes behind `abi::BINDING_LOCAL_LIGHTS` and
    /// `abi::BINDING_LIGHT_GRID`, to upload beside [`Environment::uniform`].
    pub fn light_grid(&self) -> LightGrid {
        LightGrid::build(&self.grid_mapping(), &self.local_lights())
    }

    /// The uniform this environment fills in.
    ///
    /// Lights beyond [`MAX_LIGHTS`] are dropped: the alternative is silently
    /// overrunning a host-shared array.
    pub fn uniform(&self) -> SceneUniform {
        let mut lights = [LightUniform::zeroed(); MAX_LIGHTS];
        let uniform_lights = self.uniform_lights();
        let count = uniform_lights.len();
        let tiles = self.shadow_tiles();
        let mut shadow_tiles = [ShadowTileUniform::zeroed(); abi::MAX_SHADOW_TILES];
        let atlas = self.shadow_atlas_size as f32;
        for (slot, tile) in tiles.iter().enumerate() {
            let t = abi::shadow_tile_in(slot as u32, self.shadow_atlas_size);
            shadow_tiles[slot] = ShadowTileUniform {
                rect: [
                    t.x as f32 / atlas,
                    t.y as f32 / atlas,
                    t.edge as f32 / atlas,
                    t.edge as f32 / atlas,
                ],
                view_proj: tile.view_proj.to_cols_array_2d(),
                split: tile.split,
                _padding: [0.0; 2],
                _host_padding3: [0; 4],
            };
        }
        let mapping = self.grid_mapping();
        let local_count = self.local_lights().len().min(abi::MAX_LOCAL_LIGHTS);
        for (index, slot) in lights.iter_mut().take(count).enumerate() {
            // The light's tiles are its own run of the table, in slot
            // order; a light that claimed none gets a zero count, which
            // `shadow.wxsl` reads as "casts nothing". Turning shadows off
            // for one light shifts the runs after it — the pass list is
            // per *slot*, not per light, so nothing downstream renumbers.
            let first = tiles.iter().position(|tile| tile.light == index as u32);
            let (tile_first, tile_count) = match first {
                Some(first) => {
                    let count = tiles[first..]
                        .iter()
                        .take_while(|tile| tile.light == index as u32)
                        .count() as u32;
                    (first as u32, count)
                }
                None => (0, 0),
            };
            *slot = uniform_lights[index].uniform(tile_first, tile_count);
        }
        SceneUniform {
            lights,
            shadow_tiles,
            ambient_sky: self.ambient_sky.to_array(),
            environment_enabled: 0.0,
            ambient_ground: self.ambient_ground.to_array(),
            environment_scale: 1.0,
            light_count: count as u32,
            time: self.time,
            exposure: self.exposure,
            previous_time: self.previous_time,
            origin: self.origin.as_vec3().to_array(),
            // The host that binds an environment says how deep it is.
            environment_levels: 1,
            light_grid_view_proj: mapping.view_proj.to_cols_array_2d(),
            light_grid: [
                abi::LIGHT_GRID_DIMENSIONS[0],
                abi::LIGHT_GRID_DIMENSIONS[1],
                abi::LIGHT_GRID_DIMENSIONS[2],
                local_count as u32,
            ],
            light_grid_depth: mapping.depth(),
        }
    }

    /// Every point of view this frame renders from, in [`PassView::slot`]
    /// order: the camera, one per shadow tile, then the probe's faces.
    ///
    /// Always the full length, so a pass's view slot is a fixed offset
    /// rather than a lookup into whatever tiles happen to exist. A slot
    /// no tile claimed gets the camera's own view, which nothing ever
    /// draws against — the pass built for it issues no draws at all.
    pub fn views(&self) -> Vec<CameraUniform> {
        let camera = self.camera.uniform_with(self.previous_camera.as_ref());
        let mut views = vec![camera; PassView::COUNT];
        for (slot, tile) in self.shadow_tiles().iter().enumerate() {
            views[PassView::Shadow { tile: slot as u32 }.slot()] =
                CameraUniform::new(tile.view_proj, tile.eye);
        }
        if let Some(probe) = self.probe {
            for face in 0..PassView::PROBE_FACES {
                views[PassView::Probe { face: face as u32 }.slot()] =
                    probe.face_camera(face).uniform();
            }
        }
        views
    }

    /// Whether shadow tile `index` belongs to a light this frame —
    /// i.e. whether the pass built for that slot has draws to make.
    pub fn tile_casts_shadow(&self, index: u32) -> bool {
        (index as usize) < self.shadow_tiles().len()
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

/// Whether the world-space box `min..max` lies wholly outside the view
/// `view_proj` renders: every corner beyond one side plane, or beyond the
/// far plane (ADR 0086).
///
/// Conservative: a box this misses is drawn and clipped. The near plane is
/// not tested, so a shadow caster between its light and the shadow view's
/// near plane still renders. The planes are tested in homogeneous clip
/// space, which holds for corners behind a perspective eye too.
pub fn outside_view(view_proj: Mat4, min: Vec3, max: Vec3) -> bool {
    let corners = box_corners(min, max).map(|corner| view_proj * corner.extend(1.0));
    let all = |outside: fn(glam::Vec4) -> bool| corners.iter().all(|&clip| outside(clip));
    all(|c| c.x > c.w)
        || all(|c| c.x < -c.w)
        || all(|c| c.y > c.w)
        || all(|c| c.y < -c.w)
        || all(|c| c.z > c.w)
}

/// The axis-aligned box around `min..max` once `transform` moves it.
pub fn transform_bounds(transform: Mat4, min: Vec3, max: Vec3) -> (Vec3, Vec3) {
    box_corners(min, max).iter().fold(
        (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
        |(low, high), &corner| {
            let moved = transform.transform_point3(corner);
            (low.min(moved), high.max(moved))
        },
    )
}

fn box_corners(min: Vec3, max: Vec3) -> [Vec3; 8] {
    std::array::from_fn(|i| {
        Vec3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxsl_core::node::ValueType;

    #[test]
    fn a_probe_face_flipped_in_v_is_the_webgpu_cube_face() {
        // WebGPU's face selection: each face's major axis, and the
        // directions its texel u and v (downwards) run along.
        let axes = [
            (Vec3::X, Vec3::NEG_Z, Vec3::NEG_Y),
            (Vec3::NEG_X, Vec3::Z, Vec3::NEG_Y),
            (Vec3::Y, Vec3::X, Vec3::Z),
            (Vec3::NEG_Y, Vec3::X, Vec3::NEG_Z),
            (Vec3::Z, Vec3::X, Vec3::NEG_Y),
            (Vec3::NEG_Z, Vec3::NEG_X, Vec3::NEG_Y),
        ];
        let probe = LocalProbe {
            center: Vec3::new(3.0, -2.0, 7.0),
            near: 0.1,
            far: 100.0,
        };
        for (face, (major, u_axis, v_axis)) in axes.into_iter().enumerate() {
            let inverse = probe.face_camera(face).view_proj().inverse();
            for (u, v) in [(0.25, 0.25), (0.8, 0.3), (0.5, 0.9)] {
                let expected =
                    (major + u_axis * (2.0 * u - 1.0) + v_axis * (2.0 * v - 1.0)).normalize();
                // Texel (u, v) of the face is the rendered image's
                // (u, 1 - v), whose NDC y is 1 - 2 (1 - v).
                let ndc = Vec2::new(2.0 * u - 1.0, 2.0 * v - 1.0);
                let near = inverse.project_point3(ndc.extend(0.0));
                let far = inverse.project_point3(ndc.extend(0.5));
                let ray = (far - near).normalize();
                assert!(
                    ray.distance(expected) < 1e-4,
                    "face {face} at ({u}, {v}): {ray} against {expected}"
                );
            }
        }
        // The views carry the faces in their slots, and the camera without
        // a probe.
        let mut environment = Environment {
            probe: Some(probe),
            ..Environment::default()
        };
        let slot = PassView::Probe { face: 4 }.slot();
        assert_eq!(slot, PassView::COUNT - 2);
        assert_eq!(environment.views()[slot].position, probe.center.to_array());
        environment.probe = None;
        assert_eq!(
            environment.views()[slot].view_proj,
            environment.views()[0].view_proj
        );
    }
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
        // Two vec3+scalar pairs, then bias + the run of tiles in the
        // shadow table — the matrix and rect moved into `ShadowTileUniform`
        // (one per tile, 96 bytes), because a light owns as many tiles as
        // its shadow kind needs.
        assert_eq!(size_of::<LightUniform>(), 112);
        assert_eq!(size_of::<ShadowTileUniform>(), 96);
        assert_eq!(
            size_of::<SceneUniform>(),
            // The tail: the light grid's matrix, size and depth slicing.
            MAX_LIGHTS * 112 + abi::MAX_SHADOW_TILES * 96 + 64 + 96,
            "scene uniform layout drifted from bindings.wxsl"
        );
        assert!(
            size_of::<SceneUniform>() <= 16384,
            "frame uniform must fit the WebGPU baseline"
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
    fn rectangle_shadows_use_the_same_sixteen_quadrature_positions_and_allocate_atomically() {
        let area = Light::rectangle(Vec3::new(0.0, 3.0, 0.0), Vec3::X, Vec3::Z, Vec3::ONE, 5.0)
            .casting_shadow(4.0);
        let views = area.shadow_views(&Camera::default());
        assert_eq!(views.len(), 96);
        for (sample, position) in area.area_sample_positions().iter().enumerate() {
            for view in &views[sample * 6..sample * 6 + 6] {
                assert_eq!(view.eye, *position);
            }
        }
        let env = Environment {
            lights: vec![area.clone(), area],
            ..Environment::default()
        };
        let uniform = env.uniform();
        assert_eq!(uniform.lights[0].tile_count, 96);
        assert_eq!(uniform.lights[1].tile_count, 0);
        assert_eq!(env.shadow_tiles().len(), 96);
        assert_eq!(uniform.lights[0].area_u, Vec3::X.to_array());
        assert_eq!(uniform.lights[0].area_v, Vec3::Z.to_array());
    }

    #[test]
    fn a_profile_rides_in_the_light_row_and_none_is_the_max_layer() {
        let spot = Light::spot(Vec3::ZERO, Vec3::NEG_Z, 0.3, 0.5, Vec3::ONE, 1.0);
        assert_eq!(spot.uniform(0, 0).profile_layer, u32::MAX);
        assert_eq!(spot.with_profile(3).uniform(0, 0).profile_layer, 3);
        let point = Light::point(Vec3::ZERO, Vec3::ONE, 1.0).with_profile(1);
        assert_eq!(point.uniform(0, 0).profile_layer, 1);
    }

    #[test]
    #[should_panic(expected = "point or spot")]
    fn only_points_and_spots_take_a_profile() {
        let _ = Light::directional(Vec3::Y, Vec3::ONE, 1.0).with_profile(0);
    }

    #[test]
    fn a_source_radius_rides_in_the_radius_and_a_suns_is_a_tangent() {
        let point = Light::point(Vec3::ZERO, Vec3::ONE, 1.0).with_source_radius(0.1);
        assert_eq!(point.uniform(0, 0).radius, 0.1);
        let sun = Light::directional(Vec3::Y, Vec3::ONE, 1.0).with_source_radius(0.2);
        assert_eq!(sun.uniform(0, 0).radius, 0.2_f32.tan());
    }

    #[test]
    #[should_panic(expected = "source radius")]
    fn a_rectangle_takes_no_source_radius() {
        let _ =
            Light::rectangle(Vec3::ZERO, Vec3::X, Vec3::Z, Vec3::ONE, 1.0).with_source_radius(0.1);
    }

    #[test]
    fn a_disc_spans_its_radius_faces_its_normal_and_never_shadows_or_clusters() {
        let normal = Vec3::new(0.0, -1.0, 1.0);
        let disc = Light::disc(Vec3::Y, normal, 0.2, Vec3::ONE, 4.0)
            .with_range(0.5)
            .casting_shadow(4.0);
        assert!((disc.area_u.length() - 0.2).abs() < 1e-6);
        assert!((disc.area_v.length() - 0.2).abs() < 1e-6);
        assert!(disc.area_u.dot(disc.area_v).abs() < 1e-6);
        let facing = disc.area_u.cross(disc.area_v).normalize();
        assert!(facing.dot(normal.normalize()) > 0.9999);
        assert!(disc.shadow_views(&Camera::default()).is_empty());
        assert!(!disc.is_local());
        let uniform = disc.uniform(0, 0);
        assert_eq!(uniform.kind, 5.0);
        assert_eq!(uniform.range, 0.5);
        let env = Environment {
            lights: vec![disc],
            ..Environment::default()
        };
        assert!(env.shadow_tiles().is_empty());
        assert_eq!(env.uniform().lights[0].tile_count, 0);
    }

    #[test]
    fn a_tube_carries_its_axis_and_radius_and_never_shadows_or_clusters() {
        let tube = Light::tube(Vec3::Y, Vec3::new(2.0, 0.0, 0.0), 0.05, Vec3::ONE, 4.0)
            .with_cookie(1)
            .with_range(3.0)
            .casting_shadow(4.0);
        assert!(tube.shadow_views(&Camera::default()).is_empty());
        assert!(!tube.is_local());
        let uniform = tube.uniform(0, 0);
        assert_eq!(uniform.kind, 4.0);
        assert_eq!(uniform.area_u, [2.0, 0.0, 0.0]);
        assert_eq!(uniform.radius, 0.05);
        assert_eq!(uniform.cookie_layer, 1);
        let env = Environment {
            lights: vec![tube],
            ..Environment::default()
        };
        assert!(env.shadow_tiles().is_empty());
        assert_eq!(env.uniform().lights[0].tile_count, 0);
    }

    #[test]
    fn spot_shadow_projection_matches_the_outer_cone_and_uses_one_tile() {
        let light = Light::spot(
            Vec3::new(1.0, 3.0, 2.0),
            Vec3::NEG_Y,
            0.2,
            0.6,
            Vec3::ONE,
            10.0,
        )
        .casting_shadow(4.0)
        .with_cookie(2);
        let views = light.shadow_views(&Camera::default());
        assert_eq!(views.len(), 1);
        let world =
            light.position_or_direction + Vec3::NEG_Y * 2.0 + Vec3::X * (2.0 * 0.6_f32.tan());
        let clip = views[0].view_proj * world.extend(1.0);
        assert!((clip.x.abs() / clip.w - 1.0).abs() < 1e-5);
        let uniform = light.uniform(0, 1);
        assert_eq!(uniform.cookie_layer, 2);
        assert_eq!(uniform.spot_outer_cos, 0.6_f32.cos());
    }

    #[test]
    fn a_spot_shadow_starts_at_its_near_plane() {
        let spot = |near: f32| {
            Light::spot(Vec3::ZERO, Vec3::NEG_Z, 0.2, 0.6, Vec3::ONE, 1.0)
                .casting_shadow(1.0)
                .with_shadow_near(near)
        };
        // DirectX depth: 0 on the near plane.
        let depth_at = |light: &Light, distance: f32| {
            let clip = light.shadow_views(&Camera::default())[0].view_proj
                * Vec3::new(0.0, 0.0, -distance).extend(1.0);
            clip.z / clip.w
        };
        let mut light = spot(0.02);
        light.shadow_distance = 160.0;
        assert!(depth_at(&light, 0.02).abs() < 1e-4);
        assert!(depth_at(&light, 0.05) > 0.0);
        // Unset, it is a thousandth of the distance.
        let mut light = spot(0.0);
        light.shadow_distance = 160.0;
        assert!(depth_at(&light, 0.16).abs() < 1e-4);
    }

    #[test]
    fn directional_lights_store_a_unit_direction() {
        let light = Light::directional(Vec3::new(0.0, 3.0, 4.0), Vec3::ONE, 2.0);
        let length = light.position_or_direction.length();
        assert!((length - 1.0).abs() < 1e-6);
        assert_eq!(light.uniform(0, 0).kind, 1.0);
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

    #[test]
    fn cascades_cover_all_slice_corners_overlap_and_offscreen_casters() {
        for eye in [Vec3::new(1.0, 6.0, 10.5), Vec3::new(101.0, 56.0, -89.5)] {
            let camera = Camera {
                eye,
                target: eye + Vec3::new(-1.0, -6.8, -10.5),
                aspect: 1.8,
                ..Camera::default()
            };
            let light = Light::directional(Vec3::new(0.7, 0.75, 0.45), Vec3::ONE, 2.4)
                .casting_shadow(24.0)
                .with_cascades(4);
            // Also exercise cascades after other lights' atlas allocations.
            for first_tile in [0, 6] {
                let views = light.shadow_views_at(&camera, first_tile, abi::SHADOW_ATLAS_SIZE);
                assert!(
                    (views.last().unwrap().split - light.shadow_distance.min(camera.far)).abs()
                        < 1e-5
                );
                let forward = (camera.target - camera.eye).normalize();
                let right = forward.cross(camera.up).normalize();
                let up = right.cross(forward).normalize();
                let mut slice_near = camera.near;
                let mut previous_split = 0.0;
                for view in &views {
                    for d in [slice_near, view.split] {
                        for x in [-1.0, 1.0] {
                            for y in [-1.0, 1.0] {
                                let point = camera.eye
                                    + forward * d
                                    + (right * (x * camera.aspect) + up * y)
                                        * (d * (camera.fov_y * 0.5).tan());
                                let ndc = view.view_proj.transform_point3(point);
                                assert!(
                                    ndc.x.abs() <= 1.0001 && ndc.y.abs() <= 1.0001,
                                    "slice corner clipped: {ndc:?}"
                                );
                                assert!((0.0..=1.0).contains(&ndc.z), "receiver clipped: {ndc:?}");
                                let caster = point + light.position_or_direction * 24.0;
                                let z = view.view_proj.transform_point3(caster).z;
                                assert!((0.0..=1.0).contains(&z), "offscreen caster clipped: {z}");
                            }
                        }
                    }
                    slice_near = view.split - (view.split - previous_split) * SHADOW_CASCADE_BLEND;
                    previous_split = view.split;
                }
            }
        }
    }

    #[test]
    fn a_box_outside_a_view_is_culled_and_one_behind_its_near_plane_is_not() {
        let view = glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.1, 50.0)
            * glam::camera::rh::view::look_at_mat4(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y);
        let unit = |at: Vec3| (at - Vec3::splat(0.5), at + Vec3::splat(0.5));
        let culled = |at: Vec3| {
            let (min, max) = unit(at);
            outside_view(view, min, max)
        };
        assert!(!culled(Vec3::new(0.0, 0.0, -5.0)), "in front");
        assert!(!culled(Vec3::new(0.0, 0.0, 0.0)), "straddling the eye");
        assert!(culled(Vec3::new(0.0, 0.0, 5.0)), "behind the eye");
        assert!(culled(Vec3::new(20.0, 0.0, -5.0)), "right of the frustum");
        assert!(culled(Vec3::new(0.0, -20.0, -5.0)), "below it");
        assert!(culled(Vec3::new(0.0, 0.0, -80.0)), "beyond the far plane");

        // An orthographic shadow view: a caster between the light and the
        // near plane still casts.
        let light = glam::camera::rh::proj::directx::orthographic(-5.0, 5.0, -5.0, 5.0, 1.0, 20.0)
            * glam::camera::rh::view::look_at_mat4(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y);
        let (min, max) = unit(Vec3::new(0.0, 0.0, 3.0));
        assert!(!outside_view(light, min, max));
    }

    #[test]
    fn transformed_bounds_contain_every_moved_corner() {
        let transform = Mat4::from_scale_rotation_translation(
            Vec3::splat(2.0),
            glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_4),
            Vec3::new(10.0, 0.0, 0.0),
        );
        let (min, max) = transform_bounds(transform, Vec3::splat(-1.0), Vec3::splat(1.0));
        let half = 2.0 * std::f32::consts::SQRT_2;
        assert!(
            (min - Vec3::new(10.0 - half, -2.0, -half))
                .abs()
                .max_element()
                < 1e-5
        );
        assert!(
            (max - Vec3::new(10.0 + half, 2.0, half))
                .abs()
                .max_element()
                < 1e-5
        );
    }
}
