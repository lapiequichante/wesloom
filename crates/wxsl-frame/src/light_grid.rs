//! The clustered light list, binned on the host
//! ([ADR 0076](../../docs/adr/0076-bin-ranged-lights-into-a-host-built-light-grid.md)).
//!
//! A *local* light — a point or spot light with a finite
//! [`Light::range`] and no shadow — leaves the scene uniform's sixteen
//! slots for an unbounded list, and the camera's frustum is cut into
//! [`abi::LIGHT_GRID_DIMENSIONS`] clusters, each listing the local lights
//! whose range reaches it. A fragment reads its own cluster's run and
//! loops over that, so the cost of a light is the pixels it can reach,
//! not the whole screen.
//!
//! Binned here rather than by a compute pass so that every backend and
//! every profile — the Dawn host, WebGL2 — consumes the same bytes from
//! one pure function, testable without a device. The mapping from a
//! position to its cell travels in the scene uniform ([`GridMapping`]),
//! so the shader's `local_light_span` and [`GridMapping::cell`] are the
//! same arithmetic over the same numbers.

use glam::{Mat4, Vec3, Vec4};
use wxsl_core::abi;

use crate::environment::{Light, LightUniform};

const GRID_X: u32 = abi::LIGHT_GRID_DIMENSIONS[0];
const GRID_Y: u32 = abi::LIGHT_GRID_DIMENSIONS[1];
const GRID_Z: u32 = abi::LIGHT_GRID_DIMENSIONS[2];

/// How far, in cells, a light's footprint is widened before it is floored
/// to cells: room for the shader's `f32` and `log` to round a point near a
/// cell boundary the other way than the host did.
const CELL_MARGIN: f32 = 1e-3;

/// How a position finds its cell: the view the lights were binned against
/// and its exponential depth slicing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridMapping {
    /// World (buffer space) to clip, the camera's own matrix.
    pub view_proj: Mat4,
    /// View depth of the first slice's near edge.
    pub near: f32,
    /// View depth of the last slice's far edge.
    pub far: f32,
}

impl GridMapping {
    /// Slices per natural-log unit of depth past `near`.
    pub fn slice_scale(&self) -> f32 {
        GRID_Z as f32 / (self.far / self.near).ln()
    }

    /// The `light_grid_depth` the scene uniform carries.
    pub fn depth(&self) -> [f32; 4] {
        [self.near, self.slice_scale(), 0.0, 0.0]
    }

    /// The cell `point` reads — `local_light_span` in `bindings.wxsl`,
    /// without the margin. `None` behind the view.
    pub fn cell(&self, point: Vec3) -> Option<usize> {
        let clip = self.view_proj * point.extend(1.0);
        if clip.w.is_nan() || clip.w <= 0.0 {
            return None;
        }
        let x = cell_index(((clip.x / clip.w) * 0.5 + 0.5) * GRID_X as f32, GRID_X);
        let y = cell_index(((clip.y / clip.w) * 0.5 + 0.5) * GRID_Y as f32, GRID_Y);
        let z = cell_index(self.slice(clip.w), GRID_Z);
        Some(((z * GRID_Y + y) * GRID_X + x) as usize)
    }

    /// Continuous slice coordinate of view depth `w`.
    fn slice(&self, w: f32) -> f32 {
        (w.max(self.near) / self.near).ln() * self.slice_scale()
    }
}

fn cell_index(continuous: f32, count: u32) -> u32 {
    continuous.floor().clamp(0.0, (count - 1) as f32) as u32
}

/// Which grid cells a sphere may touch, as an inclusive cell box — or
/// `None` when it cannot reach anything the view sees.
///
/// Conservative by construction: the sphere lies inside its bounding box,
/// a projection with every corner in front of the eye maps the box's hull
/// into the hull of the projected corners, and clip `w` is affine, so its
/// extremes are at corners too. A box that straddles the eye plane covers
/// every column and row.
fn footprint(mapping: &GridMapping, centre: Vec3, radius: f32) -> Option<[[u32; 2]; 3]> {
    let mut clips = [Vec4::ZERO; 8];
    for (index, clip) in clips.iter_mut().enumerate() {
        let corner = centre
            + Vec3::new(
                if index & 1 == 0 { -radius } else { radius },
                if index & 2 == 0 { -radius } else { radius },
                if index & 4 == 0 { -radius } else { radius },
            );
        *clip = mapping.view_proj * corner.extend(1.0);
    }
    let w_min = clips
        .iter()
        .map(|clip| clip.w)
        .fold(f32::INFINITY, f32::min);
    let w_max = clips
        .iter()
        .map(|clip| clip.w)
        .fold(f32::NEG_INFINITY, f32::max);
    // Wholly nearer than the near plane, or past the far one: nothing the
    // camera draws is inside it.
    if w_max.is_nan() || w_min.is_nan() || w_max < mapping.near || w_min > mapping.far {
        return None;
    }
    let (mut x, mut y) = ([0.0, GRID_X as f32], [0.0, GRID_Y as f32]);
    if w_min > 0.0 {
        let mut ndc = [[f32::INFINITY, f32::NEG_INFINITY]; 2];
        for clip in &clips {
            for axis in 0..2 {
                let value = clip[axis] / clip.w;
                ndc[axis][0] = ndc[axis][0].min(value);
                ndc[axis][1] = ndc[axis][1].max(value);
            }
        }
        // Wholly off one side of the screen.
        if ndc[0][1] < -1.0 || ndc[0][0] > 1.0 || ndc[1][1] < -1.0 || ndc[1][0] > 1.0 {
            return None;
        }
        x = ndc[0].map(|value| (value * 0.5 + 0.5) * GRID_X as f32);
        y = ndc[1].map(|value| (value * 0.5 + 0.5) * GRID_Y as f32);
    }
    let z = [mapping.slice(w_min.max(0.0)), mapping.slice(w_max)];
    let range = |[low, high]: [f32; 2], count: u32| {
        [
            cell_index(low - CELL_MARGIN, count),
            cell_index(high + CELL_MARGIN, count),
        ]
    };
    Some([range(x, GRID_X), range(y, GRID_Y), range(z, GRID_Z)])
}

/// One frame's clustered lights, ready to upload.
#[derive(Clone, Debug, Default)]
pub struct LightGrid {
    /// The local light rows, `abi::BINDING_LOCAL_LIGHTS`. Never empty: a
    /// frame with none uploads one zero row, since a zero-sized binding is
    /// a validation error.
    pub lights: Vec<LightUniform>,
    /// The grid words, `abi::BINDING_LIGHT_GRID`, a whole number of
    /// `vec4u` rows: [`abi::LIGHT_GRID_CELLS`] pairs of (first word,
    /// count), then the index runs. Four zero words when there are no
    /// local lights — the shader reads none of it then.
    pub words: Vec<u32>,
    /// Local lights left out: past [`abi::MAX_LOCAL_LIGHTS`], plus one per
    /// (light, cell) pair a full cell ([`abi::MAX_LIGHTS_PER_CELL`])
    /// refused. Zero is every light reaching every cell it touches.
    pub dropped: usize,
}

impl LightGrid {
    /// Bin `lights` — local lights, in list order — against `mapping`.
    pub fn build(mapping: &GridMapping, lights: &[&Light]) -> LightGrid {
        let kept = lights.len().min(abi::MAX_LOCAL_LIGHTS);
        let mut dropped = lights.len() - kept;
        let lights = &lights[..kept];
        if lights.is_empty() {
            return LightGrid {
                lights: vec![LightUniform::default()],
                words: vec![0; 4],
                dropped,
            };
        }
        let footprints: Vec<_> = lights
            .iter()
            .map(|light| footprint(mapping, light.position_or_direction, light.range))
            .collect();
        let cells = |[x, y, z]: [[u32; 2]; 3]| {
            (z[0]..=z[1]).flat_map(move |z| {
                (y[0]..=y[1]).flat_map(move |y| {
                    (x[0]..=x[1]).map(move |x| ((z * GRID_Y + y) * GRID_X + x) as usize)
                })
            })
        };
        // A counting sort: sizes first, so the runs are laid out once and
        // each cell lists its lights in list order — the first
        // `MAX_LIGHTS_PER_CELL` of them, deterministically.
        let mut counts = vec![0u32; abi::LIGHT_GRID_CELLS];
        for footprint in footprints.iter().flatten() {
            for cell in cells(*footprint) {
                if (counts[cell] as usize) < abi::MAX_LIGHTS_PER_CELL {
                    counts[cell] += 1;
                } else {
                    dropped += 1;
                }
            }
        }
        let header = 2 * abi::LIGHT_GRID_CELLS;
        let total: usize = counts.iter().map(|&count| count as usize).sum();
        let mut words = vec![0u32; (header + total).div_ceil(4) * 4];
        let mut next = header as u32;
        for (cell, &count) in counts.iter().enumerate() {
            words[2 * cell] = next;
            next += count;
        }
        let mut filled = vec![0u32; abi::LIGHT_GRID_CELLS];
        for (index, footprint) in footprints.iter().enumerate() {
            let Some(footprint) = footprint else {
                continue;
            };
            for cell in cells(*footprint) {
                if filled[cell] < counts[cell] {
                    let at = (words[2 * cell] + filled[cell]) as usize;
                    words[at] = index as u32;
                    filled[cell] += 1;
                }
            }
        }
        for (cell, &count) in counts.iter().enumerate() {
            words[2 * cell + 1] = count;
        }
        LightGrid {
            lights: lights.iter().map(|light| light.uniform(0, 0)).collect(),
            words,
            dropped,
        }
    }

    /// The indices into [`LightGrid::lights`] that cell `cell` lists.
    pub fn cell_lights(&self, cell: usize) -> &[u32] {
        if self.words.len() < 2 * abi::LIGHT_GRID_CELLS {
            return &[];
        }
        let first = self.words[2 * cell] as usize;
        &self.words[first..first + self.words[2 * cell + 1] as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Camera;

    fn mapping() -> GridMapping {
        let camera = Camera {
            eye: Vec3::new(0.0, 3.0, 12.0),
            target: Vec3::ZERO,
            aspect: 16.0 / 9.0,
            ..Camera::default()
        };
        GridMapping {
            view_proj: camera.view_proj(),
            near: camera.near,
            far: camera.far,
        }
    }

    /// A deterministic scatter in [-1, 1).
    fn scatter(seed: u32) -> f32 {
        let mut x = seed.wrapping_mul(0x9e37_79b9) ^ 0x85eb_ca6b;
        x ^= x >> 15;
        x = x.wrapping_mul(0x2c1b_3c6d);
        x ^= x >> 12;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// The property the shader relies on: every visible point a light's
    /// range reaches finds that light in its own cell.
    #[test]
    fn every_point_in_range_finds_its_light() {
        let mapping = mapping();
        let lights: Vec<Light> = (0..300)
            .map(|index| {
                let position = Vec3::new(
                    scatter(index * 3) * 14.0,
                    scatter(index * 3 + 1) * 4.0,
                    scatter(index * 3 + 2) * 14.0,
                );
                Light::point(position, Vec3::ONE, 1.0)
                    .with_range(0.3 + scatter(index * 7).abs() * 3.0)
            })
            .collect();
        let refs: Vec<&Light> = lights.iter().collect();
        let grid = LightGrid::build(&mapping, &refs);
        assert_eq!(grid.dropped, 0);
        let mut checked = 0;
        for sample in 0..20_000u32 {
            let point = Vec3::new(
                scatter(sample * 5 + 100_000) * 16.0,
                scatter(sample * 5 + 100_001) * 5.0,
                scatter(sample * 5 + 100_002) * 16.0,
            );
            let clip = mapping.view_proj * point.extend(1.0);
            let ndc = clip.truncate() / clip.w;
            let visible = clip.w > mapping.near
                && clip.w < mapping.far
                && ndc.x.abs() <= 1.0
                && ndc.y.abs() <= 1.0;
            if !visible {
                continue;
            }
            let cell = mapping.cell(point).expect("a visible point has a cell");
            let listed = grid.cell_lights(cell);
            for (index, light) in lights.iter().enumerate() {
                if light.position_or_direction.distance(point) < light.range {
                    checked += 1;
                    assert!(
                        listed.contains(&(index as u32)),
                        "light {index} reaches {point} but cell {cell} does not list it"
                    );
                }
            }
        }
        assert!(checked > 500, "the scatter exercised only {checked} pairs");
    }

    #[test]
    fn a_light_behind_or_beside_the_view_is_culled() {
        let mapping = mapping();
        let behind = Light::point(Vec3::new(0.0, 3.0, 20.0), Vec3::ONE, 1.0).with_range(2.0);
        let beside = Light::point(Vec3::new(60.0, 0.0, 0.0), Vec3::ONE, 1.0).with_range(2.0);
        let grid = LightGrid::build(&mapping, &[&behind, &beside]);
        assert!(grid.words[..2 * abi::LIGHT_GRID_CELLS]
            .chunks(2)
            .all(|cell| cell[1] == 0));
        // An eye inside a light's range is in every column and row of the
        // slices it reaches.
        let around = Light::point(Vec3::new(0.0, 3.0, 12.0), Vec3::ONE, 1.0).with_range(2.0);
        let grid = LightGrid::build(&mapping, &[&around]);
        assert_eq!(grid.cell_lights(0), &[0]);
        assert_eq!(grid.cell_lights((GRID_X * GRID_Y - 1) as usize), &[0]);
    }

    #[test]
    fn a_full_cell_keeps_its_first_lights_and_counts_the_rest() {
        let mapping = mapping();
        let lights: Vec<Light> = (0..abi::MAX_LIGHTS_PER_CELL + 5)
            .map(|_| Light::point(Vec3::ZERO, Vec3::ONE, 1.0).with_range(0.5))
            .collect();
        let refs: Vec<&Light> = lights.iter().collect();
        let grid = LightGrid::build(&mapping, &refs);
        let cell = mapping.cell(Vec3::ZERO).unwrap();
        let listed = grid.cell_lights(cell);
        assert_eq!(listed.len(), abi::MAX_LIGHTS_PER_CELL);
        assert_eq!(listed[0], 0);
        assert_eq!(
            *listed.last().unwrap() as usize,
            abi::MAX_LIGHTS_PER_CELL - 1
        );
        assert!(grid.dropped >= 5);
        assert_eq!(grid.words.len() % 4, 0);
    }

    #[test]
    fn no_local_lights_is_one_zero_row_and_one_zero_word_row() {
        let grid = LightGrid::build(&mapping(), &[]);
        assert_eq!(grid.lights.len(), 1);
        assert_eq!(grid.words, vec![0; 4]);
        assert_eq!(grid.dropped, 0);
    }
}
