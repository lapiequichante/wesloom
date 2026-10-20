//! Static shadows: which shadow tiles a frame redraws and which it keeps,
//! and the matrix a kept tile is read through (ADR 0101).
//!
//! A light with [`Light::static_shadow`] moves rigidly with its casters in
//! that frame, so the depth its tile holds stays right in the light's own
//! space. The tile is redrawn only when the slot has nothing of the
//! light's, or the light, seen in its frame, is not where it was when
//! drawn. A kept tile is read through the matrix it was drawn with,
//! carried by the frame's motion since. Pure, so it is tested without a
//! device; the wgpu renderer holds one.

use glam::{Mat4, Vec3};
use wxsl_core::scene::TagExpr;

use crate::environment::{Environment, Light, LightKind, SceneUniform};

/// A static light as its tile was drawn, in its casters' frame.
#[derive(Clone, Debug)]
struct Pose {
    position: Vec3,
    direction: Vec3,
    outer: f32,
    distance: f32,
    casters: TagExpr,
}

impl Pose {
    fn of(light: &Light, frame: Mat4) -> Pose {
        let to_frame = frame.inverse();
        Pose {
            position: to_frame.transform_point3(light.position_or_direction),
            direction: to_frame
                .transform_vector3(light.spot_direction)
                .normalize_or_zero(),
            outer: light.spot_outer_angle,
            distance: light.shadow_distance,
            casters: light.shadow_casters.clone(),
        }
    }

    /// Whether a tile drawn as `self` still holds what `other` would
    /// draw: the same light, to within float noise of the frame's motion.
    fn same(&self, other: &Pose) -> bool {
        let tolerance = 1e-4 * (1.0 + self.position.length());
        self.position.distance(other.position) <= tolerance
            && self.direction.dot(other.direction) >= 1.0 - 1e-6
            && (self.outer - other.outer).abs() <= 1e-6
            && (self.distance - other.distance).abs() <= 1e-4 * self.distance.max(1.0)
            && self.casters == other.casters
    }
}

/// What a slot holds: the light it was drawn for, the matrix it was
/// drawn with and the frame its casters were in.
#[derive(Clone, Debug)]
struct Entry {
    pose: Pose,
    view_proj: Mat4,
    frame: Mat4,
}

/// What each shadow slot holds across frames.
#[derive(Clone, Debug, Default)]
pub struct ShadowCache {
    entries: Vec<Option<Entry>>,
}

/// One frame's decision: the tiles it keeps, each with the matrix its
/// lookup reads it through. Every other claimed tile is drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShadowFrame {
    kept: Vec<(usize, Mat4)>,
}

impl ShadowCache {
    /// An empty cache: the next frame draws every tile.
    pub fn new() -> Self {
        ShadowCache::default()
    }

    /// Forget every kept tile, for an atlas that was reallocated or casters
    /// that changed shape.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Decide `environment`'s frame and remember what it draws. The frame
    /// must then draw every tile the answer does not keep, or the next
    /// answer is wrong.
    pub fn plan(&mut self, environment: &Environment) -> ShadowFrame {
        let lights = environment.uniform_lights();
        let tiles = environment.shadow_tiles();
        self.entries.resize(tiles.len(), None);
        self.entries.truncate(tiles.len());
        let mut kept = Vec::new();
        for (slot, tile) in tiles.iter().enumerate() {
            let light = lights[tile.light as usize];
            let Some(frame) = light
                .static_shadow
                .filter(|_| light.kind == LightKind::Spot)
            else {
                self.entries[slot] = None;
                continue;
            };
            let pose = Pose::of(light, frame);
            match &self.entries[slot] {
                Some(entry) if entry.pose.same(&pose) => {
                    kept.push((slot, entry.view_proj * entry.frame * frame.inverse()));
                }
                _ => {
                    self.entries[slot] = Some(Entry {
                        pose,
                        view_proj: tile.view_proj,
                        frame,
                    });
                }
            }
        }
        ShadowFrame { kept }
    }
}

impl ShadowFrame {
    /// Whether tile `slot` is kept this frame, its pass left out.
    pub fn keeps(&self, slot: usize) -> bool {
        self.kept.iter().any(|(kept, _)| *kept == slot)
    }

    /// Whether any tile is kept, so the atlas must not be cleared whole.
    pub fn keeps_any(&self) -> bool {
        !self.kept.is_empty()
    }

    /// The kept tiles and the matrices they are read through.
    pub fn kept(&self) -> &[(usize, Mat4)] {
        &self.kept
    }

    /// Read every kept tile through its own matrix in `uniform`.
    pub fn apply(&self, uniform: &mut SceneUniform) {
        for (slot, view_proj) in &self.kept {
            uniform.shadow_tiles[*slot].view_proj = view_proj.to_cols_array_2d();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A headlight on a car at `car`, aimed down its nose.
    fn headlight(car: Mat4, x: f32) -> Light {
        Light::spot(
            car.transform_point3(Vec3::new(x, 0.5, -2.0)),
            car.transform_vector3(Vec3::NEG_Z),
            0.5,
            0.9,
            Vec3::ONE,
            10.0,
        )
        .casting_shadow(1.0)
        .with_shadow_casters(TagExpr::tag("car"))
        .with_static_shadow(car)
    }

    fn environment(lights: Vec<Light>) -> Environment {
        Environment {
            lights,
            ..Environment::default()
        }
    }

    #[test]
    fn a_static_tile_is_drawn_once_then_kept() {
        let mut cache = ShadowCache::new();
        let env = environment(vec![headlight(Mat4::IDENTITY, -0.7)]);
        assert!(!cache.plan(&env).keeps_any());
        let second = cache.plan(&env);
        assert!(second.keeps(0));
    }

    #[test]
    fn a_kept_tile_follows_its_frame_even_when_the_view_rolls() {
        let mut cache = ShadowCache::new();
        let start = Mat4::IDENTITY;
        let drawn = environment(vec![headlight(start, -0.7)]);
        cache.plan(&drawn);
        let drawn_view = drawn.shadow_tiles()[0].view_proj;
        // Driven on, turned and banked: the spot's own view rolls from
        // world up, the car does not move in the kept map.
        let moved = Mat4::from_translation(Vec3::new(30.0, 1.0, -80.0))
            * Mat4::from_rotation_y(0.6)
            * Mat4::from_rotation_z(0.15);
        let frame = cache.plan(&environment(vec![headlight(moved, -0.7)]));
        let (_, kept) = frame.kept()[0];
        for local in [Vec3::new(0.3, 0.2, -1.0), Vec3::new(-0.6, 0.9, 1.5)] {
            let then = drawn_view.project_point3(start.transform_point3(local));
            let now = kept.project_point3(moved.transform_point3(local));
            assert!(then.distance(now) < 1e-4, "{then} vs {now}");
        }
    }

    #[test]
    fn a_light_that_moves_on_its_frame_is_redrawn() {
        let mut cache = ShadowCache::new();
        cache.plan(&environment(vec![headlight(Mat4::IDENTITY, -0.7)]));
        let swapped = cache.plan(&environment(vec![headlight(Mat4::IDENTITY, 0.7)]));
        assert!(!swapped.keeps(0));
    }

    #[test]
    fn a_slot_another_light_drew_in_is_redrawn() {
        let mut cache = ShadowCache::new();
        let static_light = environment(vec![headlight(Mat4::IDENTITY, -0.7)]);
        cache.plan(&static_light);
        let mut dynamic = headlight(Mat4::IDENTITY, -0.7);
        dynamic.static_shadow = None;
        assert!(!cache.plan(&environment(vec![dynamic])).keeps_any());
        assert!(!cache.plan(&static_light).keeps_any());
        cache.clear();
        assert!(!cache.plan(&static_light).keeps_any());
    }

    #[test]
    fn a_kept_tile_reads_through_its_own_matrix() {
        let mut cache = ShadowCache::new();
        let env = environment(vec![headlight(Mat4::IDENTITY, -0.7)]);
        cache.plan(&env);
        let moved = environment(vec![headlight(Mat4::from_translation(Vec3::X * 5.0), -0.7)]);
        let frame = cache.plan(&moved);
        let mut uniform = moved.uniform();
        frame.apply(&mut uniform);
        assert_eq!(
            uniform.shadow_tiles[0].view_proj,
            frame.kept()[0].1.to_cols_array_2d()
        );
    }
}
