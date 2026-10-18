//! A bounding volume hierarchy over world-space triangles, answering two
//! questions: is anything between here and there, and what does a ray
//! hit first?
//!
//! Built top-down with a binned surface-area heuristic; leaves hold up to
//! four triangles. An occlusion query stops at the first hit; a closest
//! one visits the nearer child first and skips boxes behind its best hit.

use glam::Vec3;

const BINS: usize = 12;
const LEAF: usize = 4;

#[derive(Clone, Copy)]
struct Bounds {
    min: Vec3,
    max: Vec3,
}

impl Bounds {
    const EMPTY: Bounds = Bounds {
        min: Vec3::splat(f32::MAX),
        max: Vec3::splat(f32::MIN),
    };

    fn grow(&mut self, point: Vec3) {
        self.min = self.min.min(point);
        self.max = self.max.max(point);
    }

    fn join(&mut self, other: &Bounds) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }

    fn area(&self) -> f32 {
        let size = (self.max - self.min).max(Vec3::ZERO);
        size.x * size.y + size.y * size.z + size.z * size.x
    }
}

/// An interior node's children are `first` and `first + 1`; a leaf's
/// triangles are `count` from `first`.
#[derive(Clone, Copy)]
struct Node {
    bounds: Bounds,
    first: u32,
    count: u32,
}

/// One triangle as a corner and two edges, for Möller–Trumbore.
#[derive(Clone, Copy)]
struct Triangle {
    corner: Vec3,
    edge1: Vec3,
    edge2: Vec3,
}

/// The hierarchy, ready for rays.
pub struct Bvh {
    nodes: Vec<Node>,
    triangles: Vec<Triangle>,
    /// Each stored triangle's index in the list the hierarchy was built
    /// from.
    ids: Vec<u32>,
}

/// What a ray hit first: a triangle of the build list, and where on it.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    /// Index in the list [`Bvh::new`] took.
    pub triangle: u32,
    /// Barycentric weight of the second corner.
    pub u: f32,
    /// Barycentric weight of the third corner.
    pub v: f32,
}

impl Bvh {
    /// Build over `triangles`.
    pub fn new(triangles: &[[Vec3; 3]]) -> Self {
        let mut order: Vec<u32> = (0..triangles.len() as u32).collect();
        let centroids: Vec<Vec3> = triangles
            .iter()
            .map(|t| (t[0] + t[1] + t[2]) / 3.0)
            .collect();
        let boxes: Vec<Bounds> = triangles
            .iter()
            .map(|t| {
                let mut bounds = Bounds::EMPTY;
                t.iter().for_each(|&point| bounds.grow(point));
                bounds
            })
            .collect();
        let mut nodes = vec![Node {
            bounds: Bounds::EMPTY,
            first: 0,
            count: triangles.len() as u32,
        }];
        let mut pending = vec![0usize];
        while let Some(index) = pending.pop() {
            let (first, count) = (nodes[index].first as usize, nodes[index].count as usize);
            let span = &mut order[first..first + count];
            let mut bounds = Bounds::EMPTY;
            let mut centres = Bounds::EMPTY;
            for &triangle in span.iter() {
                bounds.join(&boxes[triangle as usize]);
                centres.grow(centroids[triangle as usize]);
            }
            nodes[index].bounds = bounds;
            if count <= LEAF {
                continue;
            }
            let Some((axis, split)) = best_split(span, &centroids, &boxes, &centres, &bounds)
            else {
                continue;
            };
            let extent = centres.max[axis] - centres.min[axis];
            let bin = |triangle: u32| {
                let offset = (centroids[triangle as usize][axis] - centres.min[axis]) / extent;
                ((offset * BINS as f32) as usize).min(BINS - 1)
            };
            let mut left = 0;
            for item in 0..count {
                if bin(span[item]) < split {
                    span.swap(item, left);
                    left += 1;
                }
            }
            if left == 0 || left == count {
                continue;
            }
            let children = nodes.len();
            nodes[index].first = children as u32;
            nodes[index].count = 0;
            for (start, length) in [(first, left), (first + left, count - left)] {
                nodes.push(Node {
                    bounds: Bounds::EMPTY,
                    first: start as u32,
                    count: length as u32,
                });
            }
            pending.extend([children, children + 1]);
        }
        let triangles = order
            .iter()
            .map(|&index| {
                let [a, b, c] = triangles[index as usize];
                Triangle {
                    corner: a,
                    edge1: b - a,
                    edge2: c - a,
                }
            })
            .collect();
        Bvh {
            nodes,
            triangles,
            ids: order,
        }
    }

    /// The first triangle the ray crosses between `near` and `far`.
    pub fn closest(&self, origin: Vec3, direction: Vec3, near: f32, far: f32) -> Option<Hit> {
        if self.triangles.is_empty() {
            return None;
        }
        let inverse = direction.recip();
        let mut best: Option<(f32, Hit)> = None;
        let mut stack = [0u32; 64];
        let mut depth = 1;
        while depth > 0 {
            depth -= 1;
            let limit = best.map_or(far, |(t, _)| t);
            let node = &self.nodes[stack[depth] as usize];
            if box_entry(&node.bounds, origin, inverse, near, limit).is_none() {
                continue;
            }
            if node.count == 0 {
                let (a, b) = (node.first, node.first + 1);
                let entry = |child: u32| {
                    box_entry(
                        &self.nodes[child as usize].bounds,
                        origin,
                        inverse,
                        near,
                        limit,
                    )
                };
                // The nearer child goes on top, so it is searched first.
                let (first, second) = match (entry(a), entry(b)) {
                    (Some(ta), Some(tb)) if tb < ta => (Some(b), Some(a)),
                    (Some(_), Some(_)) => (Some(a), Some(b)),
                    (Some(_), None) => (Some(a), None),
                    (None, Some(_)) => (Some(b), None),
                    (None, None) => (None, None),
                };
                for child in [second, first].into_iter().flatten() {
                    stack[depth] = child;
                    depth += 1;
                }
                continue;
            }
            let first = node.first as usize;
            for (offset, triangle) in self.triangles[first..first + node.count as usize]
                .iter()
                .enumerate()
            {
                let limit = best.map_or(far, |(t, _)| t);
                if let Some((t, u, v)) = intersect(triangle, origin, direction, near, limit) {
                    let hit = Hit {
                        triangle: self.ids[first + offset],
                        u,
                        v,
                    };
                    best = Some((t, hit));
                }
            }
        }
        best.map(|(_, hit)| hit)
    }

    /// Whether any triangle crosses the ray between `near` and `far`.
    pub fn occluded(&self, origin: Vec3, direction: Vec3, near: f32, far: f32) -> bool {
        if self.triangles.is_empty() {
            return false;
        }
        let inverse = direction.recip();
        let mut stack = [0u32; 64];
        let mut depth = 1;
        while depth > 0 {
            depth -= 1;
            let node = &self.nodes[stack[depth] as usize];
            if !hits_box(&node.bounds, origin, inverse, near, far) {
                continue;
            }
            if node.count == 0 {
                stack[depth] = node.first;
                stack[depth + 1] = node.first + 1;
                depth += 2;
                continue;
            }
            let first = node.first as usize;
            for triangle in &self.triangles[first..first + node.count as usize] {
                if hits_triangle(triangle, origin, direction, near, far) {
                    return true;
                }
            }
        }
        false
    }
}

/// The axis and bin boundary with the lowest surface-area cost, or
/// `None` when no split beats keeping the node whole.
fn best_split(
    span: &[u32],
    centroids: &[Vec3],
    boxes: &[Bounds],
    centres: &Bounds,
    bounds: &Bounds,
) -> Option<(usize, usize)> {
    let mut best: Option<(f32, usize, usize)> = None;
    for axis in [0, 1, 2] {
        let extent = centres.max[axis] - centres.min[axis];
        if extent <= 0.0 {
            continue;
        }
        let mut bins = [(Bounds::EMPTY, 0u32); BINS];
        for &triangle in span {
            let offset = (centroids[triangle as usize][axis] - centres.min[axis]) / extent;
            let bin = ((offset * BINS as f32) as usize).min(BINS - 1);
            bins[bin].0.join(&boxes[triangle as usize]);
            bins[bin].1 += 1;
        }
        // Costs of everything left of each boundary, then right of it.
        let mut left = [(0.0f32, 0u32); BINS];
        let (mut grown, mut count) = (Bounds::EMPTY, 0);
        for (split, (bin, members)) in bins.iter().enumerate().take(BINS - 1) {
            grown.join(bin);
            count += members;
            left[split + 1] = (grown.area(), count);
        }
        let (mut grown, mut count) = (Bounds::EMPTY, 0);
        for split in (1..BINS).rev() {
            grown.join(&bins[split].0);
            count += bins[split].1;
            let (left_area, left_count) = left[split];
            if left_count == 0 || count == 0 {
                continue;
            }
            let cost = left_area * left_count as f32 + grown.area() * count as f32;
            if best.is_none_or(|(lowest, _, _)| cost < lowest) {
                best = Some((cost, axis, split));
            }
        }
    }
    let (cost, axis, split) = best?;
    (cost < bounds.area() * span.len() as f32).then_some((axis, split))
}

fn hits_box(bounds: &Bounds, origin: Vec3, inverse: Vec3, near: f32, far: f32) -> bool {
    box_entry(bounds, origin, inverse, near, far).is_some()
}

/// Where the ray enters the box, if it does between `near` and `far`.
fn box_entry(bounds: &Bounds, origin: Vec3, inverse: Vec3, near: f32, far: f32) -> Option<f32> {
    let a = (bounds.min - origin) * inverse;
    let b = (bounds.max - origin) * inverse;
    let enter = a.min(b).max_element().max(near);
    let exit = a.max(b).min_element().min(far);
    (enter <= exit).then_some(enter)
}

fn hits_triangle(triangle: &Triangle, origin: Vec3, direction: Vec3, near: f32, far: f32) -> bool {
    intersect(triangle, origin, direction, near, far).is_some()
}

/// Möller–Trumbore: the distance and the barycentric (u, v) of a crossing
/// strictly between `near` and `far`.
fn intersect(
    triangle: &Triangle,
    origin: Vec3,
    direction: Vec3,
    near: f32,
    far: f32,
) -> Option<(f32, f32, f32)> {
    let p = direction.cross(triangle.edge2);
    let determinant = triangle.edge1.dot(p);
    // Both sides count: an occluder blocks whichever way it faces.
    if determinant.abs() < 1e-12 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let s = origin - triangle.corner;
    let u = s.dot(p) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(triangle.edge1);
    let v = direction.dot(q) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = triangle.edge2.dot(q) * inverse;
    (t > near && t < far).then_some((t, u, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(height: f32, size: f32) -> [[Vec3; 3]; 2] {
        let corner = |x: f32, z: f32| Vec3::new(x * size, height, z * size);
        [
            [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0)],
            [corner(-1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)],
        ]
    }

    #[test]
    fn rays_find_what_is_in_their_way_and_only_that() {
        // A thousand small quads in a row, and one roof above them.
        let mut triangles = Vec::new();
        for index in 0..1000 {
            for mut triangle in quad(0.0, 0.1) {
                for corner in &mut triangle {
                    corner.x += index as f32 * 0.3;
                }
                triangles.push(triangle);
            }
        }
        triangles.extend(quad(5.0, 2.0));
        let bvh = Bvh::new(&triangles);
        assert!(bvh.occluded(Vec3::new(0.0, 1.0, 0.0), Vec3::Y, 0.0, f32::MAX));
        assert!(!bvh.occluded(Vec3::new(0.0, 1.0, 0.0), Vec3::Y, 0.0, 3.0));
        assert!(!bvh.occluded(Vec3::new(10.0, 1.0, 0.0), Vec3::Y, 0.0, f32::MAX));
        assert!(bvh.occluded(Vec3::new(30.0, 1.0, 0.0), -Vec3::Y, 0.0, f32::MAX));
        // Between two quads of the row.
        assert!(!bvh.occluded(Vec3::new(30.15, 1.0, 0.0), -Vec3::Y, 0.0, f32::MAX));

        // Straight up from under the roof, the roof; down, the row's quad
        // 100, whose second triangle holds this point.
        let up = bvh.closest(Vec3::new(0.5, 1.0, 0.3), Vec3::Y, 0.0, f32::MAX);
        assert_eq!(up.map(|hit| hit.triangle), Some(2000));
        let down = bvh
            .closest(Vec3::new(30.0, 1.0, 0.05), -Vec3::Y, 0.0, f32::MAX)
            .unwrap();
        assert_eq!(down.triangle / 2, 100);
        let [a, b, c] = triangles[down.triangle as usize];
        let point = a + (b - a) * down.u + (c - a) * down.v;
        assert!(point.distance(Vec3::new(30.0, 0.0, 0.05)) < 1e-4, "{point}");
        // From above the roof, looking down: the roof, not the row.
        let above = bvh.closest(Vec3::new(0.5, 9.0, 0.3), -Vec3::Y, 0.0, f32::MAX);
        assert!(above.unwrap().triangle >= 2000);
    }
}
