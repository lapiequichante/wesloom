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

use glam::{Mat3, Mat4, Vec3};
use wxsl_core::scene::{MeshSource, TAG_TRANSPARENT};
use wxsl_gltf::{ImportedScene, TAG_REFRACTIVE};

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
}

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
    /// Every placed vertex: world position and normal.
    jobs: Vec<(Vec3, Vec3)>,
    threads: usize,
}

pub(crate) fn bake(
    imported: &ImportedScene,
    albedo: &Albedo,
    options: &BakeOptions,
) -> Result<Vec<Bake>, PrtError> {
    if options.rays == 0 {
        return Err(PrtError("a bake needs at least one ray".to_string()));
    }
    let scene = Scene::new(imported, albedo, options);
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
            let meshes = imported
                .meshes
                .iter()
                .zip(&scene.starts)
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

impl<'a> Scene<'a> {
    fn new(imported: &'a ImportedScene, albedo: &'a Albedo, options: &BakeOptions) -> Self {
        let scene = &imported.scene;
        let mut placement: Vec<Option<Mat4>> = vec![None; imported.meshes.len()];
        let mut triangles = Vec::new();
        let mut owners = Vec::new();
        let mut lowest = f32::MAX;
        for instance in &scene.instances {
            let Some(MeshSource::Embedded { index }) =
                scene.meshes.get(instance.mesh).map(|mesh| &mesh.source)
            else {
                continue;
            };
            let Some(geometry) = imported.meshes.get(*index) else {
                continue;
            };
            let world = Mat4::from_cols_array(&scene.instance_world(instance));
            placement[*index].get_or_insert(world);
            let points: Vec<Vec3> = geometry
                .vertices
                .iter()
                .map(|vertex| world.transform_point3(Vec3::from_array(vertex.position)))
                .collect();
            lowest = points.iter().fold(lowest, |low, point| low.min(point.y));
            let clear = scene
                .materials
                .get(instance.material)
                .is_some_and(|material| {
                    let tags = instance.tags.as_ref().unwrap_or(&material.config.tags);
                    tags.contains(TAG_TRANSPARENT) || tags.contains(TAG_REFRACTIVE)
                });
            if clear {
                continue;
            }
            let normals = Mat3::from_mat4(world).inverse().transpose();
            for (triangle, corners) in geometry.indices.chunks_exact(3).enumerate() {
                triangles.push([0, 1, 2].map(|corner| points[corners[corner] as usize]));
                let front = corners
                    .iter()
                    .map(|&corner| Vec3::from_array(geometry.vertices[corner as usize].normal))
                    .sum::<Vec3>();
                owners.push(Owner {
                    mesh: *index,
                    material: instance.material,
                    first: triangle * 3,
                    front: (normals * front).normalize_or_zero(),
                });
            }
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

        let mut starts = vec![None; imported.meshes.len()];
        let mut jobs = Vec::new();
        for (index, geometry) in imported.meshes.iter().enumerate() {
            let Some(world) = placement[index] else {
                continue;
            };
            starts[index] = Some(jobs.len());
            let normals = Mat3::from_mat4(world).inverse().transpose();
            for vertex in &geometry.vertices {
                jobs.push((
                    world.transform_point3(Vec3::from_array(vertex.position)),
                    (normals * Vec3::from_array(vertex.normal)).normalize_or_zero(),
                ));
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
                || self.bvh.occluded(origin, direction, self.offset, f32::MAX)
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
            let Some(hit) = self.bvh.closest(origin, direction, self.offset, f32::MAX) else {
                continue;
            };
            let owner = &self.owners[hit.triangle as usize];
            // The back of a surface was never baked: it sends nothing.
            if direction.dot(owner.front) >= 0.0 {
                continue;
            }
            let Some(start) = self.starts[owner.mesh] else {
                continue;
            };
            let geometry = &self.imported.meshes[owner.mesh];
            let corners = &geometry.indices[owner.first..owner.first + 3];
            let weights = [1.0 - hit.u - hit.v, hit.u, hit.v];
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
            let albedo = self.albedo.sample(owner.material, uv);
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
