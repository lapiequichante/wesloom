//! The ground: rolling hills, a ring of mountains on the horizon, and
//! kept well under the viaduct: where a hill would come too close to the
//! deck, a valley is cut for it. Pure geometry, no device.

use std::collections::HashMap;

use glam::{Vec2, Vec3};
use wxsl::render::{MeshData, Vertex};

use crate::road::Road;

/// Half the terrain's edge.
pub const EXTENT: f32 = 3600.0;
/// Grid spacing.
const SPACING: f32 = 30.0;
/// The least clearance under the deck's surface: deep enough that the
/// lamps' light ([`crate::lights::LAMP_RANGE`]) never reaches the ground
/// through the deck.
pub const CLEARANCE: f32 = 17.0;
/// How far either side of the centre line the clearance holds before the
/// valley's sides rise, and how steeply they rise.
const VALLEY_FLOOR: f32 = 30.0;
const VALLEY_SLOPE: f32 = 0.3;

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The hills, before the road has its say.
fn hills(x: f32, z: f32) -> f32 {
    let rolling = 7.0 * (x * 0.0031 + 1.0).sin() * (z * 0.0027).cos()
        + 3.5 * (x * 0.0071 - z * 0.0053 + 2.0).sin()
        + 1.5 * (x * 0.017 + z * 0.013).sin();
    // Mountains past the loop, ragged along the horizon.
    let r = Vec2::new(x / 1.25, z).length();
    let angle = z.atan2(x);
    let ridge = 0.6
        + 0.25 * (angle * 5.0 + 0.7).sin()
        + 0.15 * (angle * 13.0 + 2.1).sin()
        + 0.1 * (angle * 29.0).sin();
    let mountains = smoothstep(1700.0, 2900.0, r) * 260.0 * ridge;
    rolling + 2.0 + mountains
}

/// The road's samples bucketed by plan position, for nearest lookups that
/// only look nearby.
struct Buckets {
    samples: Vec<Vec3>,
    cells: HashMap<(i32, i32), Vec<usize>>,
}

/// The bucket size: past it, the valley's sides are above any hill.
const BUCKET: f32 = 160.0;

impl Buckets {
    fn new(road: &Road) -> Self {
        let samples = road.samples(8.0);
        let mut cells: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for (index, sample) in samples[..samples.len() - 1].iter().enumerate() {
            cells.entry(Self::cell(*sample)).or_default().push(index);
        }
        Buckets { samples, cells }
    }

    fn cell(point: Vec3) -> (i32, i32) {
        (
            (point.x / BUCKET).floor() as i32,
            (point.z / BUCKET).floor() as i32,
        )
    }

    /// The road's height and distance nearest `point`, if it is within a
    /// bucket of it.
    fn nearest(&self, point: Vec2) -> Option<(f32, f32)> {
        let (cx, cz) = Self::cell(Vec3::new(point.x, 0.0, point.y));
        let mut segments = Vec::new();
        for dx in -1..=1 {
            for dz in -1..=1 {
                if let Some(indices) = self.cells.get(&(cx + dx, cz + dz)) {
                    for &index in indices {
                        segments.push(self.samples[index]);
                        segments.push(self.samples[index + 1]);
                    }
                }
            }
        }
        if segments.is_empty() {
            return None;
        }
        segments
            .chunks(2)
            .map(|pair| Road::nearest(pair, point))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }
}

/// The ground's height at `(x, z)`.
fn height(buckets: &Buckets, x: f32, z: f32) -> f32 {
    let base = hills(x, z);
    match buckets.nearest(Vec2::new(x, z)) {
        Some((road, distance)) => {
            let cap = road - CLEARANCE + (distance - VALLEY_FLOOR).max(0.0) * VALLEY_SLOPE;
            base.min(cap)
        }
        None => base,
    }
}

/// The ground under a road.
pub struct Ground {
    buckets: Buckets,
}

impl Ground {
    pub fn new(road: &Road) -> Self {
        Ground {
            buckets: Buckets::new(road),
        }
    }

    /// The ground's height at `(x, z)`.
    pub fn height(&self, x: f32, z: f32) -> f32 {
        height(&self.buckets, x, z)
    }

    /// The terrain as one grid mesh; uv.x is how paved it is, from
    /// `paving(x, z)`.
    pub fn mesh(&self, paving: impl Fn(f32, f32) -> f32) -> MeshData {
        let side = (2.0 * EXTENT / SPACING) as usize + 1;
        let at = |column: usize, row: usize| {
            let x = -EXTENT + column as f32 * SPACING;
            let z = -EXTENT + row as f32 * SPACING;
            (x, z)
        };
        let heights: Vec<f32> = (0..side * side)
            .map(|index| {
                let (x, z) = at(index % side, index / side);
                self.height(x, z)
            })
            .collect();
        let h =
            |column: usize, row: usize| heights[row.min(side - 1) * side + column.min(side - 1)];
        let mut vertices = Vec::with_capacity(side * side);
        for row in 0..side {
            for column in 0..side {
                let (x, z) = at(column, row);
                let dx = h(column + 1, row) - h(column.saturating_sub(1), row);
                let dz = h(column, row + 1) - h(column, row.saturating_sub(1));
                let normal = Vec3::new(-dx, 2.0 * SPACING, -dz).normalize();
                let tangent = Vec3::new(2.0 * SPACING, dx, 0.0).normalize();
                vertices.push(Vertex {
                    position: [x, h(column, row), z],
                    normal: normal.to_array(),
                    tangent: [tangent.x, tangent.y, tangent.z, 1.0],
                    uv: [paving(x, z), 0.0],
                });
            }
        }
        let mut indices = Vec::with_capacity((side - 1) * (side - 1) * 6);
        for row in 0..side - 1 {
            for column in 0..side - 1 {
                let a = (row * side + column) as u32;
                let (b, c, d) = (a + 1, a + side as u32, a + side as u32 + 1);
                // Counter-clockwise from above: x right, z towards the viewer.
                indices.extend([a, c, b, b, c, d]);
            }
        }
        MeshData::new(vertices, indices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_viaduct_stands_well_clear_of_the_ground() {
        let road = Road::new();
        let ground = Ground::new(&road);
        for index in 0..300 {
            let frame = road.frame(index as f32 * 21.0);
            for lateral in [-12.0, 0.0, 12.0] {
                let point = frame.at(lateral, 0.0);
                let below = point.y - ground.height(point.x, point.z);
                assert!(below > CLEARANCE - 1.0, "{below} m at {point}");
            }
        }
    }

    #[test]
    fn the_grid_faces_up() {
        let mesh = Ground::new(&Road::new()).mesh(|_, _| 0.0);
        for triangle in mesh.indices.chunks(3).step_by(997) {
            let p =
                |corner: usize| Vec3::from_array(mesh.vertices[triangle[corner] as usize].position);
            assert!((p(1) - p(0)).cross(p(2) - p(0)).y > 0.0);
        }
    }
}
