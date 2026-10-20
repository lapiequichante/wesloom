//! The city across the valley, north of the loop: towers on a street
//! grid, tallest in the centre, ahead of the car for half the drive. Pure
//! layout, no device.
//!
//! Each building sits inside its own cell of the grid, so the facade
//! shader (`shaders/facade.wxsl`) recovers a per-building seed from the
//! world position alone: `floor(xz / CELL)` is the same over a building
//! and different for its neighbours. [`CELL`] is the one number both sides
//! must agree on, passed to the material as a parameter.

use glam::{Mat4, Vec2, Vec3};

/// The street grid's pitch, in metres.
pub const CELL: f32 = 46.0;
/// Where the city's centre is, in plan (x, z).
pub const CENTER: Vec2 = Vec2::new(-150.0, -1950.0);
/// How far from the centre the city reaches.
pub const RADIUS: f32 = 420.0;
/// The ground the city stands on.
pub const GROUND: f32 = -4.0;

/// One tower: a box, by its transform of the unit cube.
#[derive(Clone, Copy, Debug)]
pub struct Building {
    pub center: Vec3,
    pub size: Vec3,
}

impl Building {
    /// The unit cube (edge 1, centred) scaled and placed.
    pub fn transform(&self) -> Mat4 {
        Mat4::from_translation(self.center) * Mat4::from_scale(self.size)
    }

    pub fn bounds(&self) -> (Vec3, Vec3) {
        (self.center - self.size * 0.5, self.center + self.size * 0.5)
    }
}

/// A deterministic value in 0..1 for a cell and a salt.
fn hash(x: i32, z: i32, salt: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343)
        ^ (z as u32).wrapping_mul(0xd816_3841)
        ^ salt.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0x00ff_ffff) as f32 / 0x0100_0000 as f32
}

/// Every building, row by row.
pub fn buildings() -> Vec<Building> {
    // Cells are the world's grid, so the facade's seed can find them.
    let cells = (RADIUS / CELL).ceil() as i32;
    let (cx, cz) = (
        (CENTER.x / CELL).round() as i32,
        (CENTER.y / CELL).round() as i32,
    );
    let mut out = Vec::new();
    for x in cx - cells..cx + cells {
        for z in cz - cells..cz + cells {
            let center = Vec3::new((x as f32 + 0.5) * CELL, 0.0, (z as f32 + 0.5) * CELL);
            let r = Vec2::new(center.x, center.z).distance(CENTER);
            if r > RADIUS || hash(x, z, 0) < 0.12 {
                continue;
            }
            // Taller towards the centre, with the odd spire anywhere.
            let falloff = (1.0 - r / RADIUS).max(0.0).powf(1.4);
            let mut height = 22.0 + 210.0 * falloff * (0.35 + 0.65 * hash(x, z, 1));
            if hash(x, z, 2) > 0.93 {
                height *= 1.6;
            }
            // A footprint and an offset that keep a street between it and
            // its cell's border.
            let width = CELL * (0.45 + 0.37 * hash(x, z, 3));
            let depth = CELL * (0.45 + 0.37 * hash(x, z, 4));
            let shift = (CELL - width.max(depth)) * 0.5 - 4.0;
            let offset = Vec3::new(
                (hash(x, z, 5) - 0.5) * shift,
                0.0,
                (hash(x, z, 6) - 0.5) * shift,
            );
            out.push(Building {
                center: center + offset + Vec3::Y * (GROUND + height * 0.5),
                size: Vec3::new(width, height, depth),
            });
            // A podium and a setback on the tall ones.
            if height > 90.0 && hash(x, z, 7) > 0.4 {
                let top = height * (0.25 + 0.2 * hash(x, z, 8));
                out.push(Building {
                    center: center + offset + Vec3::Y * (GROUND + height + top * 0.5),
                    size: Vec3::new(width * 0.6, top, depth * 0.6),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_building_stays_inside_its_cell() {
        for building in buildings() {
            let (min, max) = building.bounds();
            let cell = |value: f32| (value / CELL).floor();
            assert_eq!(cell(min.x), cell(max.x), "{building:?}");
            assert_eq!(cell(min.z), cell(max.z), "{building:?}");
        }
    }

    #[test]
    fn the_city_is_a_skyline() {
        let buildings = buildings();
        assert!(buildings.len() > 150, "{}", buildings.len());
        let tallest = buildings
            .iter()
            .map(|building| building.bounds().1.y)
            .fold(0.0, f32::max);
        assert!(tallest > 180.0, "{tallest}");
    }
}
