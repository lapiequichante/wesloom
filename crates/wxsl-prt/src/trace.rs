//! The bake: one pass for the shadowed transfer and the visibility, then
//! one per bounce.
//!
//! Every pass casts the same rays from each vertex: the same seed turns
//! the same cosine-weighted set about the normal. The shadowed pass keeps
//! the rays that escape. A bounce pass follows the rays that are blocked
//! to the first front face they hit. There it takes the previous pass's
//! transfer, interpolated across the triangle, times the diffuse colour
//! of the surface at that point. A Lambertian surface sends back albedo
//! over π of its irradiance, and with cosine-weighted rays the cosine
//! and the pdf cancel to π over the ray count, so a bounce is the mean
//! over all rays of albedo times the hit's transfer (0 for an escaped
//! ray).
//!
//! Both modes come out of the same rays: grayscale weighs each bounce by
//! the albedo's luminance, colour by each channel.
//!
//! The geometry is posed first, on the CPU: the scene's nodes at the
//! first frame of the chosen clip, and each skinned vertex blended by its
//! joints as `animation.skin` does on the GPU. The hierarchy is built
//! over the posed triangles.
//!
//! A double-sided material is lit from behind too, so its vertices are
//! traced a second time about the flipped normal. That transfer is never
//! written out: it is what a bounce off the back of the surface reads.
//!
//! A receiver (a ground under the model) is traced against the model but
//! is not in the hierarchy: it shadows nothing, so the model's own bake is
//! the same with or without it. Only the receiver's transfer is written.
//! The model's vertices are traced too only when there are bounces, for
//! the light they send onto the receiver.

use std::borrow::Cow;

use glam::{Mat3, Mat4, Vec3};
use wxsl_core::scene::{Instance, MeshSource, TAG_TRANSPARENT};
use wxsl_gltf::{Geometry, ImportedScene, TAG_REFRACTIVE};

use crate::albedo::Albedo;
use crate::{bvh, sh, Bake, BakeOptions, PrtError, Transfer};

/// Rec. 709 luminance weights, for a grayscale bounce.
const LUMINANCE: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// One occluding triangle's origin.
struct Owner {
    /// Imported mesh.
    mesh: usize,
    /// Document material.
    material: usize,
    /// Index of its first corner in the mesh's index list.
    first: usize,
    /// The world-space side it faces, from its corners' normals.
    front: Vec3,
    /// Whether its material is alpha-masked, so a hit must check the
    /// texture.
    masked: bool,
}

/// Posed vertices in world space: position and unit normal.
type Posed = Vec<(Vec3, Vec3)>;

/// A mesh's first instance: its posed vertices, and whether it is lit from
/// behind.
type Placed = (Posed, bool);

struct Scene<'a> {
    imported: &'a ImportedScene,
    albedo: &'a Albedo,
    bvh: bvh::Bvh,
    owners: Vec<Owner>,
    floor: Option<f32>,
    offset: f32,
    /// Cosine-distributed directions about +z.
    samples: Vec<Vec3>,
    /// Per imported mesh, its first vertex in the job list, if placed.
    starts: Vec<Option<usize>>,
    /// Per imported mesh, its first back-facing vertex, if double-sided.
    backs: Vec<Option<usize>>,
    /// Per receiver mesh, its first vertex in the job list, if placed.
    receivers: Vec<Option<usize>>,
    /// Every placed vertex, then the back sides: world position and
    /// normal.
    jobs: Vec<(Vec3, Vec3)>,
    threads: usize,
}

/// Bake `imported`, or with a `receiver`, that receiver alone.
pub(crate) fn bake(
    imported: &ImportedScene,
    albedo: &Albedo,
    receiver: Option<&ImportedScene>,
    options: &BakeOptions,
) -> Result<Vec<Bake>, PrtError> {
    if options.rays == 0 {
        return Err(PrtError("a bake needs at least one ray".to_string()));
    }
    let scene = Scene::new(imported, albedo, receiver, options);
    let count = scene.jobs.len();

    // The shadowed pass: the visibility, then nine coefficients.
    let direct = scene.parallel(12, |job, out| scene.shadowed(job, out));
    let visibility = |job: usize| &direct[job * 12..job * 12 + 3];
    let shadowed = |job: usize| &direct[job * 12 + 3..job * 12 + 12];

    // Running totals and the latest bounce, per mode.
    let mut gray: Vec<f32> = (0..count).flat_map(shadowed).copied().collect();
    let mut color: Vec<f32> = (0..count).flat_map(|job| shadowed(job).repeat(3)).collect();
    let (mut gray_last, mut color_last) = (gray.clone(), color.clone());
    for _ in 0..options.bounces {
        let next = scene.parallel(36, |job, out| {
            scene.bounce(job, &gray_last, &color_last, out)
        });
        gray_last = next
            .chunks_exact(36)
            .flat_map(|v| &v[..9])
            .copied()
            .collect();
        color_last = next
            .chunks_exact(36)
            .flat_map(|v| &v[9..])
            .copied()
            .collect();
        for (total, value) in gray.iter_mut().zip(&gray_last) {
            *total += value;
        }
        for (total, value) in color.iter_mut().zip(&color_last) {
            *total += value;
        }
    }

    Ok(options
        .transfers
        .iter()
        .map(|&transfer| {
            let (totals, stride) = match transfer {
                Transfer::Gray => (&gray, 9),
                Transfer::Color => (&color, 27),
            };
            let (written, starts) = match receiver {
                Some(receiver) => (receiver, &scene.receivers),
                None => (imported, &scene.starts),
            };
            let meshes = written
                .meshes
                .iter()
                .zip(starts)
                .map(|(geometry, start)| {
                    let vertices = geometry.vertices.len();
                    match start {
                        Some(start) => (*start..start + vertices)
                            .flat_map(|job| {
                                visibility(job)
                                    .iter()
                                    .chain(&totals[job * stride..(job + 1) * stride])
                                    .copied()
                            })
                            .collect(),
                        // No instance draws it, so there is nothing to see.
                        None => vec![0.0; vertices * transfer.values_per_vertex()],
                    }
                })
                .collect();
            Bake { transfer, meshes }
        })
        .collect())
}

/// Every embedded instance of `imported` posed at the first frame of
/// `clip` (or as saved): its mesh, the instance, and its world vertices,
/// position and unit normal.
fn instances(imported: &ImportedScene, clip: Option<usize>) -> Vec<(usize, &Instance, Posed)> {
    let scene = &imported.scene;
    // The nodes where the clip's first frame puts them.
    let posed = match clip.and_then(|clip| imported.animations.get(clip)) {
        Some(animation) => {
            let mut posed = scene.clone();
            animation.pose(0.0, &scene.nodes, &mut posed.nodes);
            Cow::Owned(posed)
        }
        None => Cow::Borrowed(scene),
    };
    let mut out = Vec::new();
    for instance in &scene.instances {
        let Some(MeshSource::Embedded { index }) =
            scene.meshes.get(instance.mesh).map(|mesh| &mesh.source)
        else {
            continue;
        };
        let Some(geometry) = imported.meshes.get(*index) else {
            continue;
        };
        let skin = imported
            .material_skins
            .get(instance.material)
            .copied()
            .flatten()
            .and_then(|skin| imported.skins.get(skin));
        let (world, joints) = match skin {
            // The joints carry the motion, relative to the skin's node as
            // imported, and the instance stays where it was: what the bone
            // texture and the draw do.
            Some(skin) => (
                scene.instance_world(instance),
                skin.joint_matrices(&posed, &scene.node_world(skin.node))
                    .iter()
                    .map(Mat4::from_cols_array)
                    .collect(),
            ),
            None => (posed.instance_world(instance), Vec::new()),
        };
        let vertices = pose(geometry, Mat4::from_cols_array(&world), &joints);
        out.push((*index, instance, vertices));
    }
    out
}

/// The box around every posed vertex of `imported`, if it has any.
pub(crate) fn bounds(imported: &ImportedScene, clip: Option<usize>) -> Option<(Vec3, Vec3)> {
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for (_, _, vertices) in instances(imported, clip) {
        for (point, _) in vertices {
            min = min.min(point);
            max = max.max(point);
        }
    }
    (min.x <= max.x).then_some((min, max))
}

impl<'a> Scene<'a> {
    fn new(
        imported: &'a ImportedScene,
        albedo: &'a Albedo,
        receiver: Option<&ImportedScene>,
        options: &BakeOptions,
    ) -> Self {
        let scene = &imported.scene;
        let mut placement: Vec<Option<Placed>> = vec![None; imported.meshes.len()];
        let mut triangles = Vec::new();
        let mut owners = Vec::new();
        let mut lowest = f32::MAX;
        for (index, instance, vertices) in instances(imported, options.clip) {
            let geometry = &imported.meshes[index];
            lowest = vertices
                .iter()
                .fold(lowest, |low, (point, _)| low.min(point.y));
            let material = scene.materials.get(instance.material);
            let clear = material.is_some_and(|material| {
                let tags = instance.tags.as_ref().unwrap_or(&material.config.tags);
                tags.contains(TAG_TRANSPARENT) || tags.contains(TAG_REFRACTIVE)
            });
            if !clear {
                let masked = albedo.masked(instance.material);
                for (triangle, corners) in geometry.indices.chunks_exact(3).enumerate() {
                    let corner = |at: usize| vertices[corners[at] as usize];
                    triangles.push([0, 1, 2].map(|at| corner(at).0));
                    let front = (0..3).map(|at| corner(at).1).sum::<Vec3>();
                    owners.push(Owner {
                        mesh: index,
                        material: instance.material,
                        first: triangle * 3,
                        front: front.normalize_or_zero(),
                        masked,
                    });
                }
            }
            // Light passes a clear surface, so nothing bounces off its back.
            let two_sided = !clear && material.is_some_and(|material| material.config.double_sided);
            placement[index].get_or_insert((vertices, two_sided));
        }

        let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for corner in triangles.iter().flatten() {
            min = min.min(*corner);
            max = max.max(*corner);
        }
        // Rays start this far off the surface and ignore hits closer than
        // it, so a vertex does not shadow itself.
        let offset = if min.x <= max.x {
            (max - min).length() * 1e-4
        } else {
            1e-4
        };

        // A receiver reads the model's transfer only through its bounces.
        if receiver.is_some() && options.bounces == 0 {
            placement.fill(None);
        }
        let mut starts = vec![None; imported.meshes.len()];
        let mut jobs = Vec::new();
        for (index, placed) in placement.iter().enumerate() {
            if let Some((vertices, _)) = placed {
                starts[index] = Some(jobs.len());
                jobs.extend_from_slice(vertices);
            }
        }
        let mut backs = vec![None; imported.meshes.len()];
        for (index, placed) in placement.iter().enumerate() {
            if let Some((vertices, true)) = placed {
                backs[index] = Some(jobs.len());
                jobs.extend(vertices.iter().map(|&(point, normal)| (point, -normal)));
            }
        }
        let mut receivers = Vec::new();
        if let Some(receiver) = receiver {
            receivers = vec![None; receiver.meshes.len()];
            // Rigid, as saved: a receiver has no clip of its own.
            for (index, _, vertices) in instances(receiver, None) {
                if receivers[index].is_none() {
                    receivers[index] = Some(jobs.len());
                    jobs.extend(vertices);
                }
            }
        }
        Scene {
            imported,
            albedo,
            bvh: bvh::Bvh::new(&triangles),
            owners,
            floor: options.floor.then_some(lowest),
            offset,
            samples: hemisphere(options.rays),
            starts,
            backs,
            receivers,
            jobs,
            threads: match options.threads {
                0 => std::thread::available_parallelism().map_or(1, usize::from),
                count => count,
            },
        }
    }

    /// `stride` floats per job, filled by `work` across the threads.
    fn parallel<F>(&self, stride: usize, work: F) -> Vec<f32>
    where
        F: Fn(usize, &mut [f32]) + Sync,
    {
        let mut out = vec![0.0; self.jobs.len() * stride];
        let chunk = self.jobs.len().div_ceil(self.threads * 16).max(1);
        {
            // Chunks handed out in turn, so a slow region does not hold
            // one thread up while the others idle.
            let next = std::sync::Mutex::new(out.chunks_mut(chunk * stride).enumerate());
            std::thread::scope(|scope| {
                for _ in 0..self.threads {
                    scope.spawn(|| loop {
                        let Some((index, slice)) = next.lock().unwrap().next() else {
                            break;
                        };
                        for (item, values) in slice.chunks_mut(stride).enumerate() {
                            work(index * chunk + item, values);
                        }
                    });
                }
            });
        }
        out
    }

    /// The rays of job `job`: where they start, and each direction.
    fn rays(&self, job: usize) -> Option<(Vec3, impl Iterator<Item = Vec3> + '_)> {
        let (position, normal) = self.jobs[job];
        if normal == Vec3::ZERO {
            return None;
        }
        let (tangent, bitangent) = normal.any_orthonormal_pair();
        // A per-vertex turn about the normal, so neighbours do not share
        // the same few blocked directions.
        let (sin, cos) = (random(job as u32) * std::f32::consts::TAU).sin_cos();
        let directions = self.samples.iter().map(move |sample| {
            let (x, y) = (
                cos * sample.x - sin * sample.y,
                sin * sample.x + cos * sample.y,
            );
            tangent * x + bitangent * y + normal * sample.z
        });
        Some((position + normal * self.offset, directions))
    }

    /// Whether a hit is on the surface rather than a cut-out texel of it.
    fn solid(&self, hit: bvh::Hit) -> bool {
        let owner = &self.owners[hit.triangle as usize];
        !owner.masked || self.albedo.covers(owner.material, self.uv(owner, hit))
    }

    /// The corners of the triangle `owner` names, and `hit`'s weights on
    /// them.
    fn corners(&self, owner: &Owner, hit: bvh::Hit) -> (&[u32], [f32; 3]) {
        let geometry = &self.imported.meshes[owner.mesh];
        (
            &geometry.indices[owner.first..owner.first + 3],
            [1.0 - hit.u - hit.v, hit.u, hit.v],
        )
    }

    /// Both UV sets at `hit`.
    fn uv(&self, owner: &Owner, hit: bvh::Hit) -> [[f32; 2]; 2] {
        let geometry = &self.imported.meshes[owner.mesh];
        let (corners, weights) = self.corners(owner, hit);
        let mut uv = [[0.0f32; 2]; 2];
        for (&corner, weight) in corners.iter().zip(weights) {
            let vertex = &geometry.vertices[corner as usize];
            let second = geometry
                .uv1
                .as_ref()
                .map_or(vertex.uv, |uv1| uv1[corner as usize]);
            for axis in 0..2 {
                uv[0][axis] += vertex.uv[axis] * weight;
                uv[1][axis] += second[axis] * weight;
            }
        }
        uv
    }

    fn floored(&self, origin: Vec3, direction: Vec3) -> bool {
        self.floor
            .is_some_and(|floor| direction.y < 0.0 && origin.y > floor)
    }

    /// The visibility (bent normal times occlusion) into `out[..3]` and
    /// the shadowed transfer into `out[3..12]`.
    fn shadowed(&self, job: usize, out: &mut [f32]) {
        let Some((origin, directions)) = self.rays(job) else {
            return;
        };
        let mut bent = Vec3::ZERO;
        let mut open = 0u32;
        for direction in directions {
            if self.floored(origin, direction)
                || self
                    .bvh
                    .occluded(origin, direction, self.offset, f32::MAX, |hit| {
                        self.solid(hit)
                    })
            {
                continue;
            }
            open += 1;
            bent += direction;
            for (coefficient, y) in out[3..12].iter_mut().zip(sh::basis(direction)) {
                *coefficient += y;
            }
        }
        // Cosine-weighted samples: the cosine and the pdf cancel to π / N.
        let count = self.samples.len() as f32;
        for coefficient in &mut out[3..12] {
            *coefficient *= std::f32::consts::PI / count;
        }
        let visibility = bent.normalize_or_zero() * (open as f32 / count);
        out[..3].copy_from_slice(&visibility.to_array());
    }

    /// One bounce more than `gray` and `color` hold: 9 grayscale then 27
    /// colour coefficients into `out`.
    fn bounce(&self, job: usize, gray: &[f32], color: &[f32], out: &mut [f32]) {
        let Some((origin, directions)) = self.rays(job) else {
            return;
        };
        for direction in directions {
            if self.floored(origin, direction) {
                continue;
            }
            let Some(hit) = self
                .bvh
                .closest(origin, direction, self.offset, f32::MAX, |hit| {
                    self.solid(hit)
                })
            else {
                continue;
            };
            let owner = &self.owners[hit.triangle as usize];
            // The back of a single-sided surface was never baked: it sends
            // nothing.
            let start = match direction.dot(owner.front) < 0.0 {
                true => self.starts[owner.mesh],
                false => self.backs[owner.mesh],
            };
            let Some(start) = start else {
                continue;
            };
            let (corners, weights) = self.corners(owner, hit);
            let albedo = self.albedo.sample(owner.material, self.uv(owner, hit));
            let luminance: f32 = albedo.iter().zip(LUMINANCE).map(|(c, w)| c * w).sum();
            for (&corner, weight) in corners.iter().zip(weights) {
                let vertex = start + corner as usize;
                for (slot, value) in out[..9].iter_mut().zip(&gray[vertex * 9..vertex * 9 + 9]) {
                    *slot += luminance * weight * value;
                }
                for channel in 0..3 {
                    let at = vertex * 27 + channel * 9;
                    for (slot, value) in out[9 + channel * 9..18 + channel * 9]
                        .iter_mut()
                        .zip(&color[at..at + 9])
                    {
                        *slot += albedo[channel] * weight * value;
                    }
                }
            }
        }
        let count = self.samples.len() as f32;
        for value in out.iter_mut() {
            *value /= count;
        }
    }
}

/// Every vertex of `geometry` in world space, position and unit normal:
/// blended by `joints` (none for a rigid mesh) as `animation.skin` does,
/// then placed by `world`.
fn pose(geometry: &Geometry, world: Mat4, joints: &[Mat4]) -> Posed {
    let normals = Mat3::from_mat4(world).inverse().transpose();
    let skin = match (joints.is_empty(), &geometry.joints, &geometry.weights) {
        (false, Some(indices), Some(weights)) => Some((indices, weights)),
        _ => None,
    };
    geometry
        .vertices
        .iter()
        .enumerate()
        .map(|(at, vertex)| {
            let mut point = Vec3::from_array(vertex.position);
            let mut normal = Vec3::from_array(vertex.normal);
            if let Some((indices, weights)) = skin {
                let blend = indices[at]
                    .iter()
                    .zip(weights[at])
                    .filter(|(_, weight)| *weight != 0.0)
                    .filter_map(|(&joint, weight)| Some(*joints.get(joint as usize)? * weight))
                    .fold(Mat4::ZERO, |sum, matrix| sum + matrix);
                // No weight at all: the vertex stays where the mesh put it.
                if blend != Mat4::ZERO {
                    point = blend.transform_point3(point);
                    // The linear part, as the shader turns the normal.
                    normal = blend.transform_vector3(normal);
                }
            }
            (
                world.transform_point3(point),
                (normals * normal).normalize_or_zero(),
            )
        })
        .collect()
}

/// `count` cosine-weighted directions about +z, from a Hammersley set.
pub(crate) fn hemisphere(count: u32) -> Vec<Vec3> {
    (0..count)
        .map(|index| {
            let u = (index as f32 + 0.5) / count as f32;
            let v = index.reverse_bits() as f32 / 4_294_967_296.0;
            let radius = u.sqrt();
            let angle = v * std::f32::consts::TAU;
            Vec3::new(radius * angle.cos(), radius * angle.sin(), (1.0 - u).sqrt())
        })
        .collect()
}

/// A hash of `seed` in [0, 1).
fn random(seed: u32) -> f32 {
    let mut x = seed.wrapping_mul(0x9E37_79B9) ^ 0x85EB_CA6B;
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    (x >> 8) as f32 / 16_777_216.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skinned_vertices_blend_their_joints_and_unweighted_ones_stay() {
        let vertex = |x: f32| wxsl_gltf::Vertex {
            position: [x, 0.0, 0.0],
            normal: [0.0, 1.0, 0.0],
            ..Default::default()
        };
        let geometry = Geometry {
            vertices: vec![vertex(1.0), vertex(2.0)],
            joints: Some(vec![[0, 1, 0, 0], [0; 4]]),
            weights: Some(vec![[0.5, 0.5, 0.0, 0.0], [0.0; 4]]),
            ..Default::default()
        };
        // Joint 0 lifts by 2, joint 1 turns a quarter about z; then the
        // instance moves everything 10 along x.
        let joints = [
            Mat4::from_translation(Vec3::Y * 2.0),
            Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2),
        ];
        let world = Mat4::from_translation(Vec3::X * 10.0);
        let posed = pose(&geometry, world, &joints);
        // Half of (1, 2, 0) and half of (0, 1, 0), moved.
        assert!(
            posed[0].0.distance(Vec3::new(10.5, 1.5, 0.0)) < 1e-5,
            "{}",
            posed[0].0
        );
        assert!(posed[0].1.distance(Vec3::new(-1.0, 1.0, 0.0).normalize()) < 1e-5);
        assert_eq!(posed[1], (Vec3::new(12.0, 0.0, 0.0), Vec3::Y));
    }

    #[test]
    fn the_hemisphere_is_cosine_weighted() {
        let samples = hemisphere(4096);
        let mean = samples.iter().map(|d| d.z).sum::<f32>() / samples.len() as f32;
        // E[cos θ] under a cosine pdf is 2/3.
        assert!((mean - 2.0 / 3.0).abs() < 1e-3, "{mean}");
        assert!(samples
            .iter()
            .all(|d| (d.length() - 1.0).abs() < 1e-5 && d.z > 0.0));
    }
}
