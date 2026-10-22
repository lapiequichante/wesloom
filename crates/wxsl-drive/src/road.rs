//! The road: a 2×2-lane motorway on a viaduct, looping around the valley,
//! curving in plan, rising and falling, banked into its bends. Pure
//! geometry, no device.
//!
//! The loop is a sum of harmonics in an angle θ, re-parametrized by arc
//! length so that a speed in metres per second moves the car by that much.
//! Everything that sits on the road (the car, the camera, the lamps, the
//! ground below it) asks [`Road::frame`] where that is.
//!
//! Across the deck, from the centre line outwards on each side: a concrete
//! median barrier, an inner shoulder, two 3.5 m lanes, a hard shoulder,
//! and a concrete parapet with a steel rail on posts. Under it, piers down
//! to the ground.

use glam::{Mat3, Mat4, Vec2, Vec3};
use wxsl::render::{MeshData, Vertex};

/// Half the deck's driving surface, shoulders included.
pub const HALF_WIDTH: f32 = 11.0;
/// Where the inner edge line is, from the centre line.
pub const INNER_EDGE: f32 = 1.4;
/// A lane's width.
pub const LANE_WIDTH: f32 = 3.5;
/// The centre of the slow lane of the right-hand carriageway, where the
/// car drives.
pub const LANE: f32 = INNER_EDGE + LANE_WIDTH * 1.5;
/// How deep the deck is under its surface.
const DECK: f32 = 1.8;
/// The parapet's thickness and height; the rail's bottom and top.
const PARAPET: (f32, f32) = (0.45, 0.6);
const RAIL: (f32, f32) = (0.85, 1.05);
/// The median barrier's half-widths at its foot and top, and its height.
const MEDIAN: (f32, f32, f32) = (0.4, 0.15, 0.85);
/// Spacing of the mesh's cross-sections, of the rail posts and the piers.
const STEP: f32 = 2.0;
/// How many strips the driving surface is cut into across.
const ASPHALT_STRIPS: usize = 16;
const POST_SPACING: f32 = 2.5;
const PIER_SPACING: f32 = 36.0;
/// Lamp spacing along the median.
pub const LAMP_SPACING: f32 = 36.0;
/// How high the lamp heads hang above the deck.
pub const LAMP_HEIGHT: f32 = 10.0;
/// How far each arm reaches over its carriageway.
pub const ARM_REACH: f32 = INNER_EDGE + LANE_WIDTH;
/// The height every point of the loop is lifted by, over the hills.
const LIFT: f32 = 20.0;

/// Where something on the road is, and which way the road runs there.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// On the centre line, on the surface.
    pub position: Vec3,
    /// Along the road, unit length.
    pub forward: Vec3,
    /// Across the road to the right of `forward`, banked with it.
    pub right: Vec3,
    /// Out of the surface.
    pub up: Vec3,
}

impl Frame {
    /// The point `lateral` metres right of the centre line and `height`
    /// above the surface.
    pub fn at(&self, lateral: f32, height: f32) -> Vec3 {
        self.position + self.right * lateral + self.up * height
    }
}

/// The loop, sampled for arc-length lookups.
pub struct Road {
    /// θ at each sample, evenly spaced over the loop.
    thetas: Vec<f64>,
    /// Arc length at each sample; the last one is the loop's length.
    lengths: Vec<f64>,
}

/// One harmonic of the centre line: amplitude, frequency in θ, phase.
type Harmonic = (f64, f64, f64);

/// x is a sum of cosines, y and z of sines. Long, gentle rises only: a
/// short vertical wave reads as a bump at motorway speed.
const X: &[Harmonic] = &[(1150.0, 1.0, 0.0), (120.0, 3.0, 0.6), (35.0, 7.0, 2.0)];
const Y: &[Harmonic] = &[(14.0, 2.0, 0.4), (4.0, 3.0, 1.1)];
const Z: &[Harmonic] = &[(850.0, 1.0, 0.0), (90.0, 2.0, 1.3), (60.0, 5.0, 0.0)];

/// The centre line at angle θ and its first two derivatives in θ, in
/// `f64`: positions are a kilometre from the origin, and a frame built
/// from `f32` differences of them shakes by centimetres every metre.
fn curve(theta: f64) -> [glam::DVec3; 3] {
    let sum = |harmonics: &[Harmonic], cosine: bool, derivative: usize| {
        harmonics
            .iter()
            .map(|&(amplitude, frequency, phase)| {
                let angle = frequency * theta + phase;
                // The derivatives of cos are -sin, -cos; of sin, cos, -sin.
                let value = match (cosine, derivative) {
                    (true, 0) => angle.cos(),
                    (true, 1) => -angle.sin() * frequency,
                    (true, _) => -angle.cos() * frequency * frequency,
                    (false, 0) => angle.sin(),
                    (false, 1) => angle.cos() * frequency,
                    (false, _) => -angle.sin() * frequency * frequency,
                };
                amplitude * value
            })
            .sum::<f64>()
    };
    [0, 1, 2].map(|derivative| {
        glam::DVec3::new(
            sum(X, true, derivative),
            sum(Y, false, derivative) + if derivative == 0 { LIFT as f64 } else { 0.0 },
            sum(Z, false, derivative),
        )
    })
}

/// The bank's tangent per unit of curvature (1/m), and its limit.
const BANK_GAIN: f64 = 50.0;
const MAX_BANK: f64 = 0.07;

/// How many samples the arc-length table holds.
const SAMPLES: usize = 16384;

/// The road's meshes, one per material.
pub struct RoadMeshes {
    /// The driving surface; uv is metres across (u, from the centre line)
    /// and along (v).
    pub asphalt: MeshData,
    /// The deck's sides and underside, the parapets, the median barrier
    /// and the piers; uv is metres, u along the road on the extruded
    /// faces and level on the boxes' sides.
    pub concrete: MeshData,
    /// The rails and their posts, in uv like the concrete.
    pub steel: MeshData,
}

impl Road {
    pub fn new() -> Self {
        let mut thetas = Vec::with_capacity(SAMPLES + 1);
        let mut lengths = Vec::with_capacity(SAMPLES + 1);
        let mut length = 0.0f64;
        let mut previous = curve(0.0)[0];
        for index in 0..=SAMPLES {
            let theta = index as f64 / SAMPLES as f64 * std::f64::consts::TAU;
            let point = curve(theta)[0];
            length += point.distance(previous);
            previous = point;
            thetas.push(theta);
            lengths.push(length);
        }
        Road { thetas, lengths }
    }

    /// The loop's length in metres.
    pub fn length(&self) -> f32 {
        *self.lengths.last().expect("sampled") as f32
    }

    /// θ at arc length `s`, wrapped onto the loop.
    fn theta(&self, s: f32) -> f64 {
        let total = *self.lengths.last().expect("sampled");
        let s = (s as f64).rem_euclid(total);
        let upper = self
            .lengths
            .partition_point(|length| *length < s)
            .clamp(1, SAMPLES);
        let (s0, s1) = (self.lengths[upper - 1], self.lengths[upper]);
        let t = if s1 > s0 { (s - s0) / (s1 - s0) } else { 0.0 };
        self.thetas[upper - 1] + (self.thetas[upper] - self.thetas[upper - 1]) * t
    }

    /// The road's frame at arc length `s`.
    pub fn frame(&self, s: f32) -> Frame {
        let [here, velocity, acceleration] = curve(self.theta(s));
        let forward = velocity.normalize();
        // The curvature vector: the acceleration's part across the road,
        // per metre², flattened. It points at the bend's centre.
        let across = acceleration - forward * acceleration.dot(forward);
        let bend = across / velocity.length_squared();
        let inward = glam::DVec3::new(bend.x, 0.0, bend.z);
        // Banked in towards the bend's centre, saturating smoothly at about
        // 4°: a hard clamp is a kink in the roll, and six metres out in the
        // lane a kink is a jolt.
        let tight = inward.length() * BANK_GAIN;
        let bank = if tight > 1e-9 {
            inward * (MAX_BANK * (tight / MAX_BANK).tanh() / tight * BANK_GAIN)
        } else {
            glam::DVec3::ZERO
        };
        let right = forward.cross(glam::DVec3::Y + bank).normalize();
        Frame {
            position: here.as_vec3(),
            forward: forward.as_vec3(),
            right: right.as_vec3(),
            up: right.cross(forward).as_vec3(),
        }
    }

    /// The deck, its furniture and its piers, the piers standing on
    /// `ground(x, z)`.
    pub fn meshes(&self, ground: impl Fn(f32, f32) -> f32) -> RoadMeshes {
        let rings = (self.length() / STEP).round() as usize;
        let step = self.length() / rings as f32;
        let frames: Vec<(f32, Frame)> = (0..=rings)
            .map(|ring| {
                let s = ring as f32 * step;
                (s, self.frame(s))
            })
            .collect();
        let extrude = |faces: &[Face]| {
            let mut mesh = MeshData::new(Vec::new(), Vec::new());
            for face in faces {
                extrude_face(&mut mesh, &frames, face);
            }
            mesh
        };
        let (wide, tall) = PARAPET;
        let edge = HALF_WIDTH + wide;
        let (foot, top, height) = MEDIAN;
        let concrete_faces: Vec<Face> = [
            // The parapet's inner face, its top and the deck's side.
            Face::flat((HALF_WIDTH, 0.0), (HALF_WIDTH, tall)),
            Face::flat((HALF_WIDTH, tall), (edge, tall)),
            Face::flat((edge, tall), (edge, -DECK)),
            // The median barrier's right flank.
            Face::flat((top, height), (foot, 0.0)),
        ]
        .into_iter()
        .flat_map(|face| [face, face.mirrored()])
        .chain([
            Face::flat((edge, -DECK), (-edge, -DECK)),
            Face::flat((-top, height), (top, height)),
        ])
        .collect();
        let (low, high) = RAIL;
        let rail = HALF_WIDTH + wide * 0.5;
        let steel_faces: Vec<Face> = [
            Face::flat((rail - 0.04, low), (rail - 0.04, high)),
            Face::flat((rail - 0.04, high), (rail + 0.04, high)),
            Face::flat((rail + 0.04, high), (rail + 0.04, low)),
            Face::flat((rail + 0.04, low), (rail - 0.04, low)),
        ]
        .into_iter()
        .flat_map(|face| [face, face.mirrored()])
        .collect();
        // In strips a little over a metre wide: one quad across the whole
        // deck twists with the bank, and the near-mirror puddles show its
        // fold as a line across the road.
        let mut asphalt = MeshData::new(Vec::new(), Vec::new());
        let strips = ASPHALT_STRIPS as f32;
        for strip in 0..ASPHALT_STRIPS {
            let across = |index: f32| -HALF_WIDTH + 2.0 * HALF_WIDTH * index / strips;
            extrude_face(
                &mut asphalt,
                &frames,
                &Face {
                    from: (across(strip as f32), 0.0),
                    to: (across(strip as f32 + 1.0), 0.0),
                    lateral_uv: true,
                },
            );
        }
        let mut concrete = extrude(&concrete_faces);
        let mut steel = extrude(&steel_faces);

        let posts = (self.length() / POST_SPACING).round() as usize;
        for post in 0..posts {
            let frame = self.frame(post as f32 * self.length() / posts as f32);
            for side in [-1.0, 1.0] {
                let center = frame.at(side * rail, (tall + high) * 0.5);
                push_box(
                    &mut steel,
                    oriented(&frame, center, Vec3::new(0.1, high - tall, 0.1)),
                );
            }
        }
        let piers = (self.length() / PIER_SPACING).round() as usize;
        for pier in 0..piers {
            let frame = self.frame(pier as f32 * self.length() / piers as f32);
            let under = frame.at(0.0, -DECK);
            let foot = ground(under.x, under.z) - 1.0;
            // A cap across the deck, and one column down to the ground.
            push_box(
                &mut concrete,
                oriented(
                    &frame,
                    under - frame.up * 0.6,
                    Vec3::new(edge * 1.4, 1.2, 2.4),
                ),
            );
            let column = (under.y - 1.2 - foot).max(0.5);
            push_box(
                &mut concrete,
                Mat4::from_translation(Vec3::new(under.x, foot + column * 0.5, under.z))
                    * yaw(frame.forward)
                    * Mat4::from_scale(Vec3::new(3.2, column, 2.0)),
            );
        }
        for mesh in [&mut asphalt, &mut concrete, &mut steel] {
            shade(mesh);
        }
        RoadMeshes {
            asphalt,
            concrete,
            steel,
        }
    }

    /// Where the lamps stand: one pole on the median every
    /// [`LAMP_SPACING`] metres, an arm over each carriageway.
    pub fn lamps(&self) -> Vec<Lamp> {
        let count = (self.length() / LAMP_SPACING).floor() as usize;
        (0..count)
            .map(|index| {
                let frame = self.frame(index as f32 * self.length() / count as f32);
                let foot = frame.at(0.0, MEDIAN.2);
                let across = Vec3::new(frame.right.x, 0.0, frame.right.z).normalize();
                let top = foot + Vec3::Y * (LAMP_HEIGHT - MEDIAN.2);
                Lamp {
                    foot,
                    across,
                    heads: [top - across * ARM_REACH, top + across * ARM_REACH],
                }
            })
            .collect()
    }

    /// The nearest point of the centre line to `point` in plan, as its
    /// height and its horizontal distance, from [`Road::samples`].
    pub fn nearest(samples: &[Vec3], point: Vec2) -> (f32, f32) {
        let mut best = (f32::MAX, 0.0);
        for pair in samples.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (a2, b2) = (Vec2::new(a.x, a.z), Vec2::new(b.x, b.z));
            let along = b2 - a2;
            let t = ((point - a2).dot(along) / along.length_squared().max(1e-6)).clamp(0.0, 1.0);
            let distance = point.distance(a2 + along * t);
            if distance < best.0 {
                best = (distance, a.y + (b.y - a.y) * t);
            }
        }
        (best.1, best.0)
    }

    /// The centre line every `spacing` metres, closed.
    pub fn samples(&self, spacing: f32) -> Vec<Vec3> {
        let count = (self.length() / spacing).ceil() as usize;
        (0..=count)
            .map(|index| {
                self.frame(index as f32 * self.length() / count as f32)
                    .position
            })
            .collect()
    }
}

/// One median lamp: a pole and two heads.
#[derive(Clone, Copy, Debug)]
pub struct Lamp {
    /// The pole's foot, on the median barrier.
    pub foot: Vec3,
    /// Horizontal and unit length, to the right of the road.
    pub across: Vec3,
    /// Over the left carriageway, then the right one.
    pub heads: [Vec3; 2],
}

/// One flat face of the deck's cross-section, extruded along the road:
/// from `from` to `to`, each (lateral, height). Its front is to the left
/// of the way it runs in the (right, up) plane, so a face running right
/// faces up and one running down faces right.
#[derive(Clone, Copy)]
struct Face {
    from: (f32, f32),
    to: (f32, f32),
    /// uv is the lateral position (the carriageway's markings) and the
    /// distance along, rather than the distance along and across the face.
    lateral_uv: bool,
}

impl Face {
    fn flat(from: (f32, f32), to: (f32, f32)) -> Self {
        Face {
            from,
            to,
            lateral_uv: false,
        }
    }

    /// The same face on the other side of the centre line, facing the
    /// mirrored way.
    fn mirrored(self) -> Self {
        Face {
            from: (-self.to.0, self.to.1),
            to: (-self.from.0, self.from.1),
            lateral_uv: self.lateral_uv,
        }
    }
}

fn extrude_face(mesh: &mut MeshData, frames: &[(f32, Frame)], face: &Face) {
    let base = mesh.vertices.len() as u32;
    let width = Vec2::from(face.to).distance(Vec2::from(face.from));
    // The face's normal in the (right, up) plane: its direction turned a
    // quarter counter-clockwise.
    let direction = (Vec2::from(face.to) - Vec2::from(face.from)).normalize();
    let across = Vec2::new(-direction.y, direction.x);
    for (s, frame) in frames {
        let normal = frame.right * across.x + frame.up * across.y;
        for (point, u) in [(face.from, 0.0), (face.to, width)] {
            mesh.vertices.push(Vertex {
                position: frame.at(point.0, point.1).to_array(),
                normal: normal.to_array(),
                tangent: [0.0, 0.0, 0.0, 1.0],
                uv: if face.lateral_uv {
                    [point.0, *s]
                } else {
                    [*s, u]
                },
            });
        }
    }
    for ring in 0..frames.len() as u32 - 1 {
        let (a0, b0) = (base + ring * 2, base + ring * 2 + 1);
        let (a1, b1) = (a0 + 2, b0 + 2);
        mesh.indices.extend([a0, b0, a1, b0, b1, a1]);
    }
}

/// Yaw about +y so that +z points along `forward`'s plan.
fn yaw(forward: Vec3) -> Mat4 {
    Mat4::from_rotation_y(forward.x.atan2(forward.z))
}

/// The unit cube at `center` in the road's frame, scaled (x across, y up,
/// z along).
fn oriented(frame: &Frame, center: Vec3, size: Vec3) -> Mat4 {
    Mat4::from_cols(
        frame.right.extend(0.0),
        frame.up.extend(0.0),
        // right × up is backwards: a right-handed basis keeps the winding.
        (-frame.forward).extend(0.0),
        center.extend(1.0),
    ) * Mat4::from_scale(size)
}

/// Append the unit cube moved by `transform`, its uv in the units the
/// transform scales it to.
pub fn push_box(mesh: &mut MeshData, transform: Mat4) {
    let (vertices, indices) = wxsl::render::mesh::cube_geometry(1.0);
    let linear = Mat3::from_mat4(transform);
    let normals = linear.inverse().transpose();
    let base = mesh.vertices.len() as u32;
    for vertex in vertices {
        let local = Vec3::from_slice(&vertex.tangent[..3]);
        let stretch = Vec2::new(
            (linear * local).length(),
            (linear * Vec3::from_array(vertex.normal).cross(local)).length(),
        );
        let normal = (normals * Vec3::from_array(vertex.normal)).normalize();
        let tangent = (linear * Vec3::new(vertex.tangent[0], vertex.tangent[1], vertex.tangent[2]))
            .normalize();
        mesh.vertices.push(Vertex {
            position: transform
                .transform_point3(Vec3::from_array(vertex.position))
                .to_array(),
            normal: normal.to_array(),
            tangent: [tangent.x, tangent.y, tangent.z, vertex.tangent[3]],
            uv: (Vec2::from(vertex.uv) * stretch).to_array(),
        });
    }
    mesh.indices
        .extend(indices.iter().map(|index| base + index));
}

/// Tangents of the extruded faces, along their u (a box's own are kept).
fn shade(mesh: &mut MeshData) {
    let mut tangents = vec![Vec3::ZERO; mesh.vertices.len()];
    for triangle in mesh.indices.chunks(3) {
        let [a, b] = [0, 1].map(|corner| triangle[corner] as usize);
        let edge = Vec3::from_array(mesh.vertices[b].position)
            - Vec3::from_array(mesh.vertices[a].position);
        tangents[a] += edge;
        tangents[b] += edge;
    }
    for (vertex, tangent) in mesh.vertices.iter_mut().zip(&tangents) {
        // Only the extruded faces carry the placeholder tangent.
        if vertex.tangent[..3] != [0.0, 0.0, 0.0] {
            continue;
        }
        let normal = Vec3::from_array(vertex.normal);
        let tangent = (*tangent - normal * normal.dot(*tangent))
            .normalize_or(normal.any_orthonormal_vector());
        vertex.tangent = [tangent.x, tangent.y, tangent.z, 1.0];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_loop_closes_and_is_a_few_kilometres_long() {
        let road = Road::new();
        assert!(
            (5000.0..9000.0).contains(&road.length()),
            "{}",
            road.length()
        );
        let start = road.frame(0.0).position;
        let end = road.frame(road.length() - 0.01).position;
        assert!(start.distance(end) < 0.1);
    }

    #[test]
    fn a_frame_is_orthonormal_and_mostly_level() {
        let road = Road::new();
        for index in 0..200 {
            let frame = road.frame(index as f32 * 37.0);
            assert!((frame.forward.length() - 1.0).abs() < 1e-3);
            assert!((frame.right.length() - 1.0).abs() < 1e-3);
            assert!((frame.up.length() - 1.0).abs() < 1e-3);
            assert!(frame.forward.dot(frame.right).abs() < 1e-3);
            assert!(frame.up.dot(frame.forward).abs() < 1e-3);
            // A gentle road: under 10% grade, a few degrees of bank.
            assert!(frame.forward.y.abs() < 0.1, "grade {}", frame.forward.y);
            assert!(frame.up.y > 0.99, "bank {}", frame.up.y);
        }
    }

    #[test]
    fn the_car_rides_smoothly_at_motorway_speed() {
        // Frame to frame at 60 Hz and 150 km/h, the lane's centre neither
        // jolts up and down nor rocks.
        let road = Road::new();
        let (dt, speed) = (1.0 / 60.0, 42.0);
        let lane = |s: f32| road.frame(s).at(LANE, 0.0);
        for index in 1..4000 {
            let s = index as f32 * 1.7;
            let step = speed * dt;
            let jolt = (lane(s + step) - 2.0 * lane(s) + lane(s - step)) / (dt * dt);
            assert!(jolt.y.abs() < 2.0, "{} m/s² at {s} m", jolt.y);
            let rock = road.frame(s + step).up.dot(road.frame(s).right).abs();
            assert!(rock < 3e-3, "{rock} at {s} m");
        }
    }

    #[test]
    fn arc_length_moves_at_the_speed_asked_for() {
        let road = Road::new();
        let (a, b) = (road.frame(1000.0).position, road.frame(1010.0).position);
        assert!((a.distance(b) - 10.0).abs() < 0.05, "{}", a.distance(b));
    }

    #[test]
    fn every_face_is_wound_the_way_its_normal_points() {
        let meshes = Road::new().meshes(|_, _| 0.0);
        for mesh in [&meshes.asphalt, &meshes.concrete, &meshes.steel] {
            for triangle in mesh.indices.chunks(3).step_by(7) {
                let p = |corner: usize| {
                    Vec3::from_array(mesh.vertices[triangle[corner] as usize].position)
                };
                let winding = (p(1) - p(0)).cross(p(2) - p(0)).normalize();
                let stored = Vec3::from_array(mesh.vertices[triangle[0] as usize].normal);
                assert!(
                    winding.dot(stored) > 0.5,
                    "{winding} against {stored} at {}",
                    p(0)
                );
            }
        }
        for triangle in meshes.asphalt.indices.chunks(3).step_by(101) {
            let normal = Vec3::from_array(meshes.asphalt.vertices[triangle[0] as usize].normal);
            assert!(normal.y > 0.9);
        }
    }

    #[test]
    fn lamps_reach_over_both_carriageways() {
        let road = Road::new();
        let lamps = road.lamps();
        assert!(lamps.len() > 100);
        let samples = road.samples(2.0);
        for lamp in &lamps[..10] {
            for head in lamp.heads {
                let (height, distance) = Road::nearest(&samples, Vec2::new(head.x, head.z));
                assert!((distance - ARM_REACH).abs() < 0.5, "{distance}");
                assert!(head.y > height + LAMP_HEIGHT - 0.5);
            }
        }
    }
}
