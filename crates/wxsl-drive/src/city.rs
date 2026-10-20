//! The city: blocks along the whole loop, on both sides of the viaduct,
//! dense by the road and thinning out into the fields, rising into a
//! skyline around a few downtowns. Pure layout and geometry, no device.
//!
//! A building is one of a few [`Kind`]s, each a mesh over the unit
//! footprint (x and z in -0.5..0.5, y in 0..1) that every building of its
//! kind instances, scaled and turned square to the road. Its seed is a
//! per-instance attribute, from which the facade shader
//! (`shaders/facade.wxsl`) draws its windows and its neon.

use std::collections::HashMap;

use glam::{Mat4, Vec2, Vec3};
use wxsl::render::{MeshData, Vertex};

use crate::road::{self, Road};

/// The per-instance attribute holding a building's seed, and the facade's
/// socket it feeds.
pub const SEED: &str = "seed";
/// Lots are laid on a world grid this far apart, then jittered.
const LOT: f32 = 40.0;
/// The street left between two buildings.
const STREET: f32 = 6.0;
/// How close a building comes to the road's centre line: clear of the
/// deck, its parapets and the pier caps, and out of the lamps' reach.
pub const CLEARANCE: f32 = 34.0;
/// How far from the road the city reaches, and how much further around a
/// downtown.
const REACH: f32 = 240.0;
const REACH_DOWNTOWN: f32 = 480.0;
/// The downtowns: where along the loop, as a fraction of it, and how far
/// along it each spreads, metres.
const DOWNTOWNS: [(f32, f32); 5] = [
    (0.02, 420.0),
    (0.22, 300.0),
    (0.43, 380.0),
    (0.63, 260.0),
    (0.81, 340.0),
];

/// A building's shape, one mesh for every building of the kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A plain block.
    Block,
    /// An L: a full-height wing and a lower one.
    Wing,
    /// A tower stepping in twice.
    Setback,
    /// A twelve-sided tower with a crown.
    Round,
    /// A slender tower on a wide podium, with a spire.
    Podium,
    /// Two towers of different heights and a skybridge.
    Twin,
    /// Five shrinking tiers.
    Ziggurat,
}

impl Kind {
    /// In declaration order, so a kind as `usize` indexes it.
    pub const ALL: [Kind; 7] = [
        Kind::Block,
        Kind::Wing,
        Kind::Setback,
        Kind::Round,
        Kind::Podium,
        Kind::Twin,
        Kind::Ziggurat,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Block => "drive block",
            Kind::Wing => "drive wing",
            Kind::Setback => "drive setback",
            Kind::Round => "drive round tower",
            Kind::Podium => "drive podium tower",
            Kind::Twin => "drive twin towers",
            Kind::Ziggurat => "drive ziggurat",
        }
    }

    /// Its mesh over the unit footprint, y from 0 to 1.
    pub fn mesh(self) -> MeshData {
        let mut mesh = MeshData::new(Vec::new(), Vec::new());
        let block = |mesh: &mut MeshData, min: [f32; 3], max: [f32; 3]| {
            let (min, max) = (Vec3::from(min), Vec3::from(max));
            road::push_box(
                mesh,
                Mat4::from_translation((min + max) * 0.5) * Mat4::from_scale(max - min),
            );
        };
        match self {
            Kind::Block => block(&mut mesh, [-0.5, 0.0, -0.5], [0.5, 1.0, 0.5]),
            Kind::Wing => {
                block(&mut mesh, [-0.5, 0.0, 0.1], [0.5, 1.0, 0.5]);
                block(&mut mesh, [-0.5, 0.0, -0.5], [-0.1, 0.65, 0.1]);
            }
            Kind::Setback => {
                block(&mut mesh, [-0.5, 0.0, -0.5], [0.5, 0.55, 0.5]);
                block(&mut mesh, [-0.36, 0.55, -0.36], [0.36, 0.82, 0.36]);
                block(&mut mesh, [-0.24, 0.82, -0.24], [0.24, 1.0, 0.24]);
            }
            Kind::Round => {
                prism(&mut mesh, 0.5, 0.0, 0.93);
                prism(&mut mesh, 0.4, 0.93, 1.0);
            }
            Kind::Podium => {
                block(&mut mesh, [-0.5, 0.0, -0.5], [0.5, 0.12, 0.5]);
                block(&mut mesh, [-0.28, 0.12, -0.28], [0.28, 0.95, 0.28]);
                block(&mut mesh, [-0.04, 0.95, -0.04], [0.04, 1.0, 0.04]);
            }
            Kind::Twin => {
                block(&mut mesh, [-0.5, 0.0, -0.5], [-0.1, 1.0, 0.5]);
                block(&mut mesh, [0.1, 0.0, -0.5], [0.5, 0.82, 0.5]);
                block(&mut mesh, [-0.1, 0.55, -0.12], [0.1, 0.6, 0.12]);
            }
            Kind::Ziggurat => {
                for tier in 0..5 {
                    let half = 0.5 - 0.09 * tier as f32;
                    let (low, high) = (tier as f32 * 0.2, (tier + 1) as f32 * 0.2);
                    block(&mut mesh, [-half, low, -half], [half, high, half]);
                }
            }
        }
        mesh
    }

    /// The kind for a building `height` metres tall, by `pick` in 0..1.
    fn pick(height: f32, pick: f32) -> Kind {
        let odds: &[(Kind, f32)] = if height < 35.0 {
            &[(Kind::Block, 0.55), (Kind::Wing, 0.3), (Kind::Round, 0.15)]
        } else if height < 100.0 {
            &[
                (Kind::Block, 0.2),
                (Kind::Setback, 0.25),
                (Kind::Round, 0.2),
                (Kind::Wing, 0.15),
                (Kind::Twin, 0.2),
            ]
        } else {
            &[
                (Kind::Setback, 0.3),
                (Kind::Podium, 0.25),
                (Kind::Round, 0.2),
                (Kind::Twin, 0.15),
                (Kind::Ziggurat, 0.1),
            ]
        };
        let mut sum = 0.0;
        for &(kind, odd) in odds {
            sum += odd;
            if pick < sum {
                return kind;
            }
        }
        odds[odds.len() - 1].0
    }
}

/// A twelve-sided prism of `radius` (to its corners) from `low` to
/// `high`, flat-shaded so that each side is a wall of its own, capped.
fn prism(mesh: &mut MeshData, radius: f32, low: f32, high: f32) {
    const SIDES: usize = 12;
    let corner = |side: usize| {
        let angle = side as f32 / SIDES as f32 * std::f32::consts::TAU;
        Vec3::new(angle.cos() * radius, 0.0, angle.sin() * radius)
    };
    let push = |mesh: &mut MeshData, points: [Vec3; 3], normal: Vec3, tangent: Vec3| {
        let base = mesh.vertices.len() as u32;
        for point in points {
            mesh.vertices.push(Vertex {
                position: point.to_array(),
                normal: normal.to_array(),
                tangent: [tangent.x, tangent.y, tangent.z, 1.0],
                uv: [0.0, 0.0],
            });
        }
        mesh.indices.extend([base, base + 1, base + 2]);
    };
    let (up, down) = (Vec3::Y * high, Vec3::Y * low);
    for side in 0..SIDES {
        let (a, b) = (corner(side), corner(side + 1));
        let normal = ((a + b) * 0.5).normalize();
        let tangent = (b - a).normalize();
        push(mesh, [a + down, a + up, b + up], normal, tangent);
        push(mesh, [a + down, b + up, b + down], normal, tangent);
        push(mesh, [up, b + up, a + up], Vec3::Y, Vec3::X);
        push(mesh, [down, a + down, b + down], Vec3::NEG_Y, Vec3::X);
    }
}

/// One building, placed.
#[derive(Clone, Copy, Debug)]
pub struct Building {
    pub kind: Kind,
    /// The unit footprint to the world: turned square to the road, scaled,
    /// its foot a little under the lowest ground it stands on.
    pub transform: Mat4,
    /// Its world box.
    pub bounds: (Vec3, Vec3),
    /// Its own value in 0..1, for the facade.
    pub seed: f32,
    /// How far along the road it stands, which keeps the buildings a pass
    /// draws close together in the draw list.
    pub s: f32,
}

/// A footprint in plan: a rectangle turned to `axis`.
#[derive(Clone, Copy, Debug)]
struct Footprint {
    center: Vec2,
    /// Unit length, along the width.
    axis: Vec2,
    /// Half the width and half the depth.
    half: Vec2,
}

impl Footprint {
    fn corners(&self) -> [Vec2; 4] {
        let (u, v) = (self.axis * self.half.x, self.axis.perp() * self.half.y);
        [
            self.center - u - v,
            self.center + u - v,
            self.center + u + v,
            self.center - u + v,
        ]
    }

    /// Its extent along `direction` (unit length).
    fn project(&self, direction: Vec2) -> (f32, f32) {
        let middle = self.center.dot(direction);
        let reach = self.half.x * self.axis.dot(direction).abs()
            + self.half.y * self.axis.perp().dot(direction).abs();
        (middle - reach, middle + reach)
    }

    /// Whether the two overlap once each is grown by `margin`, by the
    /// separating axes.
    fn overlaps(&self, other: &Footprint, margin: f32) -> bool {
        [self.axis, self.axis.perp(), other.axis, other.axis.perp()]
            .into_iter()
            .all(|direction| {
                let (a, b) = (self.project(direction), other.project(direction));
                a.0 - margin < b.1 + margin && b.0 - margin < a.1 + margin
            })
    }
}

/// A deterministic value in 0..1 for a lot and a salt.
fn hash(x: i32, z: i32, salt: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343)
        ^ (z as u32).wrapping_mul(0xd816_3841)
        ^ salt.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0x00ff_ffff) as f32 / 0x0100_0000 as f32
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How much of a downtown arc length `s` is in, 0..1.
fn downtown(s: f32, length: f32) -> f32 {
    DOWNTOWNS
        .iter()
        .map(|&(at, spread)| {
            let apart = (s - at * length).rem_euclid(length);
            let apart = apart.min(length - apart);
            (-(apart / spread).powi(2)).exp()
        })
        .fold(0.0, f32::max)
}

/// The nearest point of the closed polyline `samples` to `point`: the
/// segment's index, how far along it (0..1), and the distance.
fn nearest(samples: &[Vec2], point: Vec2) -> (usize, f32, f32) {
    let mut best = (0, 0.0, f32::MAX);
    for (index, pair) in samples.windows(2).enumerate() {
        let along = pair[1] - pair[0];
        let t = ((point - pair[0]).dot(along) / along.length_squared().max(1e-6)).clamp(0.0, 1.0);
        let distance = point.distance(pair[0] + along * t);
        if distance < best.2 {
            best = (index, t, distance);
        }
    }
    best
}

/// A building chosen for a lot, before it is fitted among the others.
struct Lot {
    kind: Kind,
    footprint: Footprint,
    height: f32,
    seed: f32,
    s: f32,
}

/// Every building, standing on `ground(x, z)`, by kind and then along the
/// road, so that the draws of one kind a pass keeps are mostly adjacent
/// and drawn as one instanced call (ADR 0086).
pub fn buildings(road: &Road, ground: impl Fn(f32, f32) -> f32) -> Vec<Building> {
    let samples: Vec<Vec2> = road
        .samples(20.0)
        .iter()
        .map(|point| Vec2::new(point.x, point.z))
        .collect();
    let length = road.length();
    let step = length / (samples.len() - 1) as f32;
    let (low, high) = samples.iter().fold(
        (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)),
        |(low, high), point| (low.min(*point), high.max(*point)),
    );
    let reach = REACH + REACH_DOWNTOWN;
    let first = ((low - reach) / LOT).floor().as_ivec2();
    let last = ((high + reach) / LOT).ceil().as_ivec2();
    let mut lots = Vec::new();
    for x in first.x..last.x {
        for z in first.y..last.y {
            let r = |salt: u32| hash(x, z, salt);
            let center =
                (Vec2::new(x as f32, z as f32) + 0.5) * LOT + (Vec2::new(r(1), r(2)) - 0.5) * 10.0;
            let (index, t, distance) = nearest(&samples, center);
            let s = (index as f32 + t) * step;
            let downtown = downtown(s, length);
            let reach = REACH + REACH_DOWNTOWN * downtown;
            if distance > reach {
                continue;
            }
            // Nearly every lot by the road, fewer out towards the fields.
            let near = 1.0 - smoothstep(reach * 0.5, reach, distance);
            if r(3) > 0.15 + 0.75 * near {
                continue;
            }
            // Taller in a downtown, the towers standing back a block from
            // the viaduct so that the skyline shows over the low-rise
            // along it; the odd lone tower anywhere.
            let tall = downtown.powf(1.3)
                * (0.25 + 0.75 * smoothstep(50.0, 120.0, distance))
                * (1.0 - 0.7 * smoothstep(120.0, reach, distance));
            let mut height = 14.0 + 30.0 * r(4) + tall * (40.0 + 210.0 * r(5) * r(5));
            if r(6) > 0.97 {
                height = height * 1.6 + 40.0;
            }
            let kind = Kind::pick(height, r(7));
            let (mut width, mut depth) = (18.0 + 16.0 * r(8), 18.0 + 16.0 * r(9));
            match kind {
                Kind::Round => depth = width,
                Kind::Twin => width *= 1.3,
                Kind::Ziggurat => {
                    width = 48.0 + 30.0 * r(8);
                    depth = width * (0.8 + 0.2 * r(9));
                    height = (height * 0.7).clamp(60.0, 160.0);
                }
                _ => {}
            }
            let footprint = Footprint {
                center,
                axis: (samples[index + 1] - samples[index]).normalize(),
                half: Vec2::new(width, depth) * 0.5,
            };
            if distance - footprint.half.length() < CLEARANCE {
                continue;
            }
            lots.push(Lot {
                kind,
                footprint,
                height,
                seed: r(10),
                s,
            });
        }
    }

    // The tallest first, so that a tower keeps its lot against the
    // low-rise around it.
    lots.sort_by(|a, b| b.height.total_cmp(&a.height));
    const BUCKET: f32 = 120.0;
    let bucket = |point: Vec2| (point / BUCKET).floor().as_ivec2();
    let mut taken: HashMap<(i32, i32), Vec<Footprint>> = HashMap::new();
    let mut out = Vec::new();
    for lot in lots {
        let cell = bucket(lot.footprint.center);
        let clashes = (-1..=1).any(|dx| {
            (-1..=1).any(|dz| {
                taken.get(&(cell.x + dx, cell.y + dz)).is_some_and(|others| {
                    others
                        .iter()
                        .any(|other| other.overlaps(&lot.footprint, STREET * 0.5))
                })
            })
        });
        if clashes {
            continue;
        }
        taken
            .entry((cell.x, cell.y))
            .or_default()
            .push(lot.footprint);
        out.push(place(&lot, &ground));
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.s.total_cmp(&b.s)));
    out
}

/// How paved the ground at a point is, 0..1: fully within a street of a
/// building, and none 25 m further out.
pub fn paving(buildings: &[Building]) -> impl Fn(f32, f32) -> f32 + '_ {
    const BUCKET: f32 = 100.0;
    let mut near: HashMap<(i32, i32), Vec<(Vec2, f32)>> = HashMap::new();
    for building in buildings {
        let (min, max) = building.bounds;
        let center = Vec2::new(min.x + max.x, min.z + max.z) * 0.5;
        let radius = Vec2::new(max.x - min.x, max.z - min.z).length() * 0.5;
        let cell = (center / BUCKET).floor().as_ivec2();
        near.entry((cell.x, cell.y)).or_default().push((center, radius));
    }
    move |x, z| {
        let point = Vec2::new(x, z);
        let cell = (point / BUCKET).floor().as_ivec2();
        let mut paved: f32 = 0.0;
        for dx in -1..=1 {
            for dz in -1..=1 {
                for (center, radius) in near.get(&(cell.x + dx, cell.y + dz)).into_iter().flatten() {
                    let outside = point.distance(*center) - radius;
                    paved = paved.max(1.0 - smoothstep(STREET, STREET + 25.0, outside));
                }
            }
        }
        paved
    }
}

/// `lot` on the ground: its foot under the lowest corner, its roof
/// `height` over the highest.
fn place(lot: &Lot, ground: &impl Fn(f32, f32) -> f32) -> Building {
    let footprint = &lot.footprint;
    let heights = footprint
        .corners()
        .into_iter()
        .chain([footprint.center])
        .map(|point| ground(point.x, point.y));
    let (foot, top) = heights.fold((f32::MAX, f32::MIN), |(low, high), height| {
        (low.min(height), high.max(height))
    });
    let foot = foot - 1.5;
    let transform = Mat4::from_translation(Vec3::new(footprint.center.x, foot, footprint.center.y))
        * Mat4::from_rotation_y((-footprint.axis.y).atan2(footprint.axis.x))
        * Mat4::from_scale(Vec3::new(
            footprint.half.x * 2.0,
            top + lot.height - foot,
            footprint.half.y * 2.0,
        ));
    let bounds = [0.0, 1.0]
        .into_iter()
        .flat_map(|y| {
            [[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]]
                .map(|[x, z]| transform.transform_point3(Vec3::new(x, y, z)))
        })
        .fold(
            (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
            |(min, max), corner| (min.min(corner), max.max(corner)),
        );
    Building {
        kind: lot.kind,
        transform,
        bounds,
        seed: lot.seed,
        s: lot.s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn city() -> (Road, Vec<Building>) {
        let road = Road::new();
        let ground = crate::terrain::Ground::new(&road);
        let buildings = buildings(&road, |x, z| ground.height(x, z));
        (road, buildings)
    }

    /// A building's footprint back from its transform.
    fn footprint(building: &Building) -> Footprint {
        let t = building.transform;
        let x = t.transform_vector3(Vec3::X);
        let z = t.transform_vector3(Vec3::Z);
        let center = t.transform_point3(Vec3::ZERO);
        Footprint {
            center: Vec2::new(center.x, center.z),
            axis: Vec2::new(x.x, x.z).normalize(),
            half: Vec2::new(Vec2::new(x.x, x.z).length(), Vec2::new(z.x, z.z).length()) * 0.5,
        }
    }

    #[test]
    fn the_city_lines_the_whole_loop_and_has_a_skyline() {
        let (road, buildings) = city();
        assert!(buildings.len() > 1200, "{}", buildings.len());
        // Every stretch of the loop has buildings by it.
        let stretches = 16;
        let mut by_stretch = vec![0; stretches];
        for building in &buildings {
            by_stretch[(building.s / road.length() * stretches as f32) as usize % stretches] += 1;
        }
        assert!(by_stretch.iter().all(|&count| count > 25), "{by_stretch:?}");
        let tallest = buildings
            .iter()
            .map(|building| building.bounds.1.y - building.bounds.0.y)
            .fold(0.0, f32::max);
        assert!(tallest > 180.0, "{tallest}");
        for kind in Kind::ALL {
            assert!(
                buildings.iter().any(|building| building.kind == kind),
                "no {kind:?}"
            );
        }
    }

    #[test]
    fn every_building_stands_clear_of_the_road_and_of_the_others() {
        let (road, buildings) = city();
        let samples: Vec<Vec2> = road
            .samples(5.0)
            .iter()
            .map(|point| Vec2::new(point.x, point.z))
            .collect();
        let footprints: Vec<Footprint> = buildings.iter().map(footprint).collect();
        for (index, print) in footprints.iter().enumerate() {
            for corner in print.corners() {
                let (_, _, distance) = nearest(&samples, corner);
                assert!(distance > CLEARANCE - 1.0, "{distance} m at {corner}");
            }
            for other in &footprints[index + 1..] {
                let apart = print.center.distance(other.center);
                if apart < print.half.length() + other.half.length() {
                    assert!(!print.overlaps(other, 0.0), "{print:?} and {other:?}");
                }
            }
        }
    }

    #[test]
    fn a_building_stands_on_the_ground() {
        let road = Road::new();
        let ground = crate::terrain::Ground::new(&road);
        for building in buildings(&road, |x, z| ground.height(x, z))
            .iter()
            .step_by(17)
        {
            let foot = building.transform.transform_point3(Vec3::ZERO);
            assert!(foot.y < ground.height(foot.x, foot.z), "{foot}");
        }
    }

    #[test]
    fn every_kind_is_wound_outwards_inside_the_unit_footprint() {
        for kind in Kind::ALL {
            let mesh = kind.mesh();
            for triangle in mesh.indices.chunks(3) {
                let p = |corner: usize| {
                    Vec3::from_array(mesh.vertices[triangle[corner] as usize].position)
                };
                let winding = (p(1) - p(0)).cross(p(2) - p(0)).normalize();
                let stored = Vec3::from_array(mesh.vertices[triangle[0] as usize].normal);
                assert!(winding.dot(stored) > 0.9, "{kind:?}: {winding} against {stored}");
            }
            for vertex in &mesh.vertices {
                let [x, y, z] = vertex.position;
                assert!(x.abs() <= 0.5 + 1e-5 && z.abs() <= 0.5 + 1e-5, "{kind:?}");
                assert!((-1e-5..=1.0 + 1e-5).contains(&y), "{kind:?}");
            }
        }
    }

    #[test]
    fn footprints_overlap_only_when_they_do() {
        let at = |x: f32, angle: f32| Footprint {
            center: Vec2::new(x, 0.0),
            axis: Vec2::from_angle(angle),
            half: Vec2::new(10.0, 5.0),
        };
        assert!(at(0.0, 0.0).overlaps(&at(19.0, 0.0), 0.0));
        assert!(!at(0.0, 0.0).overlaps(&at(21.0, 0.0), 0.0));
        assert!(at(0.0, 0.0).overlaps(&at(21.0, 0.0), 1.0));
        // Turned a quarter, the second reaches only 5 m back.
        assert!(!at(0.0, 0.0).overlaps(&at(16.0, std::f32::consts::FRAC_PI_2), 0.0));
    }
}
