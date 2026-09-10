//! Geometry: the vertex format the shader ABI's vertex stage expects, and the
//! primitives to draw with it.
//!
//! [`Vertex::LAYOUT`] and `VertexIn` in `shaders/wxsl/vertex.wxsl` are the
//! two halves of one contract — the `@location` numbers must line up, so the
//! two are edited together.

use core::ops::Range;

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

/// One vertex: position, shading basis, texture coordinates.
///
/// The tangent's `w` is the handedness of the bitangent, the usual glTF
/// convention: `bitangent = cross(normal, tangent.xyz) * tangent.w`. Storing
/// a sign rather than a third vector keeps the vertex smaller and cannot
/// disagree with the normal after interpolation.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Vertex {
    /// Object-space position.
    pub position: [f32; 3],
    /// Object-space normal, unit length.
    pub normal: [f32; 3],
    /// Object-space tangent (xyz) and bitangent sign (w).
    pub tangent: [f32; 4],
    /// Texture coordinates.
    pub uv: [f32; 2],
}

impl Vertex {
    /// Vertex attributes, matching `VertexIn`'s locations.
    const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x3,
        2 => Float32x4,
        3 => Float32x2,
    ];

    /// The buffer layout to hand to a render pipeline.
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: core::mem::size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &Self::ATTRIBUTES,
    };
}

/// One mesh's geometry on the CPU: what a generator or an importer
/// produces, and what [`Mesh::upload`] takes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshData {
    /// The vertices.
    pub vertices: Vec<Vertex>,
    /// Triangle indices, three per triangle.
    pub indices: Vec<u32>,
}

impl MeshData {
    /// The geometry, as a pair.
    pub fn new(vertices: Vec<Vertex>, indices: Vec<u32>) -> Self {
        MeshData { vertices, indices }
    }

    /// How many triangles.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Append `other`, shifting its indices — how an importer merges every
    /// primitive of a file into one mesh.
    pub fn extend(&mut self, other: &MeshData) {
        let base = self.vertices.len() as u32;
        self.vertices.extend_from_slice(&other.vertices);
        self.indices
            .extend(other.indices.iter().map(|index| index + base));
    }
}

/// An indexed triangle mesh on the GPU.
pub struct Mesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

impl Mesh {
    /// Upload `vertices` and `indices` as a mesh.
    ///
    /// Indices are `u32`, not `u16`: 65k vertices is a limit any real mesh
    /// crosses, and paying four bytes an index is cheaper than discovering
    /// the ceiling from a glTF file that renders as confetti (ADR 0021).
    pub fn new(device: &wgpu::Device, label: &str, vertices: &[Vertex], indices: &[u32]) -> Self {
        use wgpu::util::DeviceExt as _;
        Mesh {
            vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("{label} vertices")),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("{label} indices")),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            }),
            index_count: indices.len() as u32,
        }
    }

    /// Upload CPU geometry as a mesh.
    pub fn upload(device: &wgpu::Device, label: &str, data: &MeshData) -> Self {
        Mesh::new(device, label, &data.vertices, &data.indices)
    }

    /// A cube of edge length `size`, centred on the origin.
    ///
    /// Six independent faces (24 vertices), because a cube's corners have
    /// three different normals each — sharing them would round the lighting
    /// off at every edge.
    pub fn cube(device: &wgpu::Device, size: f32) -> Self {
        let (vertices, indices) = cube_geometry(size);
        Mesh::new(device, "cube", &vertices, &indices)
    }

    /// A UV sphere of `radius`, centred on the origin.
    ///
    /// Poles included, so the top and bottom rings are degenerate triangles —
    /// which is the price of a seam-free tangent frame everywhere else, and a
    /// preview mesh is the one place that trade is obviously right.
    pub fn sphere(device: &wgpu::Device, radius: f32) -> Self {
        let (vertices, indices) = sphere_geometry(radius, 48, 32);
        Mesh::new(device, "sphere", &vertices, &indices)
    }

    /// A flat `size` x `size` quad in the xz plane, facing up.
    ///
    /// Subdivided rather than two triangles: a material that displaces or
    /// varies across the surface has nothing to vary over on two triangles.
    pub fn plane(device: &wgpu::Device, size: f32) -> Self {
        let (vertices, indices) = plane_geometry(size, 32);
        Mesh::new(device, "plane", &vertices, &indices)
    }

    /// A torus of `radius` with a tube of `tube_radius`, around the y axis.
    ///
    /// The useful third preview shape: it has curvature in two directions and
    /// a continuous UV wrap, so a normal map or a noise pattern shows up in a
    /// way neither a cube nor a sphere reveals.
    pub fn torus(device: &wgpu::Device, radius: f32, tube_radius: f32) -> Self {
        let (vertices, indices) = torus_geometry(radius, tube_radius, 64, 24);
        Mesh::new(device, "torus", &vertices, &indices)
    }

    /// The mesh for `kind`, at a size that fits the demo camera.
    pub fn from_kind(device: &wgpu::Device, kind: MeshKind) -> Self {
        match kind {
            MeshKind::Cube => Mesh::cube(device, 1.6),
            MeshKind::Sphere => Mesh::sphere(device, 1.0),
            MeshKind::Plane => Mesh::plane(device, 2.4),
            MeshKind::Torus => Mesh::torus(device, 0.9, 0.35),
        }
    }

    /// Number of indices, i.e. `triangles * 3`.
    pub fn index_count(&self) -> u32 {
        self.index_count
    }

    /// Bind this mesh's buffers and draw it once.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        self.draw_instances(pass, 0..1);
    }

    /// Bind this mesh's buffers and draw `instances` of it.
    ///
    /// The range is not a count: it is *which rows of the frame's instance
    /// buffer* to draw, because `@builtin(instance_index)` starts at the
    /// first instance rather than at zero. Drawing one object is
    /// `index..index + 1`.
    pub fn draw_instances(&self, pass: &mut wgpu::RenderPass<'_>, instances: Range<u32>) {
        self.bind(pass);
        pass.draw_indexed(0..self.index_count, 0, instances);
    }

    /// Bind this mesh's buffers without drawing — what an indirect draw
    /// needs, since the draw itself comes from a buffer.
    pub fn bind(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
    }
}

/// Which primitive a caller wants, for a UI that offers a choice.
///
/// The editor's preview panel cycles through these; nothing in the renderer
/// depends on the set, so adding one is this enum plus a geometry function.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MeshKind {
    /// Six flat faces: shows a flat shading response and the UV seams.
    #[default]
    Cube,
    /// A UV sphere: shows a smooth shading response.
    Sphere,
    /// A flat quad: shows a material the way a texture swatch would.
    Plane,
    /// A torus: curvature in two directions, and a continuous UV wrap.
    Torus,
}

impl MeshKind {
    /// Every kind, in declaration order.
    pub const ALL: &'static [MeshKind] = &[
        MeshKind::Cube,
        MeshKind::Sphere,
        MeshKind::Plane,
        MeshKind::Torus,
    ];

    /// The kind's name, as used on a command line and in a button.
    pub fn name(&self) -> &'static str {
        match self {
            MeshKind::Cube => "cube",
            MeshKind::Sphere => "sphere",
            MeshKind::Plane => "plane",
            MeshKind::Torus => "torus",
        }
    }

    /// Parse a kind from its [`MeshKind::name`].
    pub fn parse(text: &str) -> Option<Self> {
        MeshKind::ALL
            .iter()
            .copied()
            .find(|kind| kind.name().eq_ignore_ascii_case(text.trim()))
    }

    /// The next kind, wrapping — for a button that cycles.
    pub fn next(&self) -> Self {
        let index = MeshKind::ALL
            .iter()
            .position(|kind| kind == self)
            .unwrap_or(0);
        MeshKind::ALL[(index + 1) % MeshKind::ALL.len()]
    }
}

/// A UV sphere's vertices and indices, on the CPU.
///
/// `segments` divisions around the equator, `rings` from pole to pole.
pub fn sphere_geometry(radius: f32, segments: u16, rings: u16) -> (Vec<Vertex>, Vec<u32>) {
    let segments = segments.max(3);
    let rings = rings.max(2);
    let mut vertices = Vec::with_capacity(((segments + 1) * (rings + 1)) as usize);
    for ring in 0..=rings {
        // v runs from the north pole down, so that v = 0 is the top in the
        // usual texture convention.
        let v = f32::from(ring) / f32::from(rings);
        let polar = v * core::f32::consts::PI;
        let (sin_polar, cos_polar) = polar.sin_cos();
        for segment in 0..=segments {
            let u = f32::from(segment) / f32::from(segments);
            let azimuth = u * core::f32::consts::TAU;
            let (sin_azimuth, cos_azimuth) = azimuth.sin_cos();
            let normal = Vec3::new(sin_polar * cos_azimuth, cos_polar, sin_polar * sin_azimuth);
            // The direction u increases in: the derivative with respect to
            // the azimuth, which stays well defined at the poles even though
            // the surface there does not.
            let tangent = Vec3::new(-sin_azimuth, 0.0, cos_azimuth);
            vertices.push(Vertex {
                position: (normal * radius).to_array(),
                normal: normal.to_array(),
                tangent: [tangent.x, tangent.y, tangent.z, 1.0],
                uv: [u, v],
            });
        }
    }

    let stride = u32::from(segments) + 1;
    let mut indices = Vec::with_capacity(usize::from(segments) * usize::from(rings) * 6);
    for ring in 0..u32::from(rings) {
        for segment in 0..u32::from(segments) {
            let top_left = ring * stride + segment;
            let top_right = top_left + 1;
            let bottom_left = top_left + stride;
            let bottom_right = bottom_left + 1;
            // Counter-clockwise seen from outside, matching the cube. `v`
            // runs downwards from the north pole, so this is the opposite
            // winding to the plane's, whose rows run away from the camera.
            indices.extend_from_slice(&[
                top_left,
                bottom_right,
                bottom_left,
                top_left,
                top_right,
                bottom_right,
            ]);
        }
    }
    (vertices, indices)
}

/// A subdivided quad's vertices and indices, on the CPU.
///
/// In the xz plane, facing +y, centred on the origin.
pub fn plane_geometry(size: f32, subdivisions: u16) -> (Vec<Vertex>, Vec<u32>) {
    let steps = subdivisions.max(1);
    let half = size * 0.5;
    let mut vertices = Vec::with_capacity(((steps + 1) * (steps + 1)) as usize);
    for row in 0..=steps {
        let v = f32::from(row) / f32::from(steps);
        for column in 0..=steps {
            let u = f32::from(column) / f32::from(steps);
            vertices.push(Vertex {
                position: [u * size - half, 0.0, v * size - half],
                normal: [0.0, 1.0, 0.0],
                tangent: [1.0, 0.0, 0.0, 1.0],
                uv: [u, v],
            });
        }
    }

    let stride = u32::from(steps) + 1;
    let mut indices = Vec::with_capacity(usize::from(steps) * usize::from(steps) * 6);
    for row in 0..u32::from(steps) {
        for column in 0..u32::from(steps) {
            let near_left = row * stride + column;
            let near_right = near_left + 1;
            let far_left = near_left + stride;
            let far_right = far_left + 1;
            // Counter-clockwise seen from +y, i.e. from above.
            indices.extend_from_slice(&[
                near_left, far_left, far_right, near_left, far_right, near_right,
            ]);
        }
    }
    (vertices, indices)
}

/// A torus's vertices and indices, on the CPU.
///
/// `segments` divisions around the main ring, `rings` around the tube.
pub fn torus_geometry(
    radius: f32,
    tube_radius: f32,
    segments: u16,
    rings: u16,
) -> (Vec<Vertex>, Vec<u32>) {
    let segments = segments.max(3);
    let rings = rings.max(3);
    let mut vertices = Vec::with_capacity(((segments + 1) * (rings + 1)) as usize);
    for segment in 0..=segments {
        let u = f32::from(segment) / f32::from(segments);
        let major = u * core::f32::consts::TAU;
        let (sin_major, cos_major) = major.sin_cos();
        let center = Vec3::new(cos_major * radius, 0.0, sin_major * radius);
        // Around the main ring: also the direction u increases in.
        let tangent = Vec3::new(-sin_major, 0.0, cos_major);
        for ring in 0..=rings {
            let v = f32::from(ring) / f32::from(rings);
            let minor = v * core::f32::consts::TAU;
            let (sin_minor, cos_minor) = minor.sin_cos();
            let normal = Vec3::new(cos_major * cos_minor, sin_minor, sin_major * cos_minor);
            vertices.push(Vertex {
                position: (center + normal * tube_radius).to_array(),
                normal: normal.to_array(),
                tangent: [tangent.x, tangent.y, tangent.z, 1.0],
                uv: [u, v],
            });
        }
    }

    let stride = u32::from(rings) + 1;
    let mut indices = Vec::with_capacity(usize::from(segments) * usize::from(rings) * 6);
    for segment in 0..u32::from(segments) {
        for ring in 0..u32::from(rings) {
            let here = segment * stride + ring;
            let next_ring = here + 1;
            let next_segment = here + stride;
            let diagonal = next_segment + 1;
            // Counter-clockwise seen from outside the tube: around the
            // tube first, then around the ring.
            indices.extend_from_slice(&[here, next_ring, diagonal, here, diagonal, next_segment]);
        }
    }
    (vertices, indices)
}

/// The cube's vertices and indices, on the CPU.
///
/// Separate from [`Mesh::cube`] so it can be tested without a GPU.
pub fn cube_geometry(size: f32) -> (Vec<Vertex>, Vec<u32>) {
    // (normal, tangent): the tangent is the direction texture u runs in.
    let faces = [
        (Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_X, Vec3::Z),
        (Vec3::Y, Vec3::X),
        (Vec3::NEG_Y, Vec3::X),
        (Vec3::Z, Vec3::X),
        (Vec3::NEG_Z, Vec3::NEG_X),
    ];

    let half = size * 0.5;
    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (normal, tangent) in faces {
        let bitangent = normal.cross(tangent);
        let center = normal * half;
        let base = vertices.len() as u32;
        // Counter-clockwise seen from outside the cube, which is the default
        // front face — so back-face culling keeps the outside.
        for (u, v) in [(0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0)] {
            let corner =
                center + tangent * (u * 2.0 - 1.0) * half + bitangent * (1.0 - v * 2.0) * half;
            vertices.push(Vertex {
                position: corner.to_array(),
                normal: normal.to_array(),
                tangent: [tangent.x, tangent.y, tangent.z, 1.0],
                uv: [u, v],
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (vertices, indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cube_has_six_flat_faces() {
        let (vertices, indices) = cube_geometry(2.0);
        assert_eq!(vertices.len(), 24);
        assert_eq!(indices.len(), 36);
        for vertex in &vertices {
            let position = Vec3::from_array(vertex.position);
            // Edge length 2 means every coordinate is +-1.
            assert!(position.abs().max_element() - 1.0 < 1e-6);
            let normal = Vec3::from_array(vertex.normal);
            assert!((normal.length() - 1.0).abs() < 1e-6);
            let tangent = Vec3::new(vertex.tangent[0], vertex.tangent[1], vertex.tangent[2]);
            // The shading basis has to be orthonormal for normal mapping to
            // mean anything.
            assert!((tangent.length() - 1.0).abs() < 1e-6);
            assert!(normal.dot(tangent).abs() < 1e-6);
        }
    }

    #[test]
    fn faces_wind_counter_clockwise_seen_from_outside() {
        let (vertices, indices) = cube_geometry(2.0);
        for triangle in indices.chunks(3) {
            let [a, b, c] = [
                Vec3::from_array(vertices[triangle[0] as usize].position),
                Vec3::from_array(vertices[triangle[1] as usize].position),
                Vec3::from_array(vertices[triangle[2] as usize].position),
            ];
            let winding = (b - a).cross(c - a);
            let normal = Vec3::from_array(vertices[triangle[0] as usize].normal);
            assert!(
                winding.dot(normal) > 0.0,
                "triangle {triangle:?} faces inwards"
            );
        }
    }

    /// One primitive's geometry, for the invariants they all share.
    type Geometry = (MeshKind, Vec<Vertex>, Vec<u32>);

    /// Every primitive's geometry.
    fn all_geometry() -> Vec<Geometry> {
        [
            (MeshKind::Cube, cube_geometry(2.0)),
            (MeshKind::Sphere, sphere_geometry(1.0, 16, 8)),
            (MeshKind::Plane, plane_geometry(2.0, 4)),
            (MeshKind::Torus, torus_geometry(1.0, 0.3, 16, 8)),
        ]
        .into_iter()
        .map(|(kind, (vertices, indices))| (kind, vertices, indices))
        .collect()
    }

    #[test]
    fn every_primitive_has_a_usable_shading_basis() {
        // The vertex stage builds a tangent frame from these and normal
        // mapping falls apart if they are not orthonormal — which is the kind
        // of thing that looks like a shader bug for an hour.
        for (kind, vertices, indices) in all_geometry() {
            assert!(!vertices.is_empty(), "{kind:?} has no vertices");
            assert_eq!(indices.len() % 3, 0, "{kind:?} has a partial triangle");
            for index in &indices {
                assert!(
                    (*index as usize) < vertices.len(),
                    "{kind:?} indexes vertex {index} of {}",
                    vertices.len()
                );
            }
            for vertex in &vertices {
                let normal = Vec3::from_array(vertex.normal);
                let tangent = Vec3::new(vertex.tangent[0], vertex.tangent[1], vertex.tangent[2]);
                assert!(
                    (normal.length() - 1.0).abs() < 1e-4,
                    "{kind:?} has a non-unit normal {normal:?}"
                );
                assert!(
                    (tangent.length() - 1.0).abs() < 1e-4,
                    "{kind:?} has a non-unit tangent {tangent:?}"
                );
                assert!(
                    normal.dot(tangent).abs() < 1e-3,
                    "{kind:?} has a tangent {tangent:?} not perpendicular to {normal:?}"
                );
                assert!(vertex.uv.iter().all(|c| (0.0..=1.0).contains(c)));
            }
        }
    }

    #[test]
    fn every_primitive_winds_counter_clockwise_seen_from_outside() {
        for (kind, vertices, indices) in all_geometry() {
            for triangle in indices.chunks(3) {
                let [a, b, c] = [
                    Vec3::from_array(vertices[triangle[0] as usize].position),
                    Vec3::from_array(vertices[triangle[1] as usize].position),
                    Vec3::from_array(vertices[triangle[2] as usize].position),
                ];
                let winding = (b - a).cross(c - a);
                if winding.length() < 1e-6 {
                    // A sphere's poles are degenerate by construction, and a
                    // zero-area triangle has no facing to check.
                    continue;
                }
                let normal = Vec3::from_array(vertices[triangle[0] as usize].normal);
                assert!(
                    winding.dot(normal) > 0.0,
                    "{kind:?} triangle {triangle:?} faces inwards"
                );
            }
        }
    }

    #[test]
    fn a_sphere_puts_every_vertex_on_its_radius() {
        let (vertices, _) = sphere_geometry(2.5, 12, 6);
        for vertex in &vertices {
            let distance = Vec3::from_array(vertex.position).length();
            assert!(
                (distance - 2.5).abs() < 1e-4,
                "{distance} is off the radius"
            );
        }
    }

    #[test]
    fn a_torus_stays_within_its_tube_of_the_main_ring() {
        let (vertices, _) = torus_geometry(1.0, 0.25, 12, 8);
        for vertex in &vertices {
            let position = Vec3::from_array(vertex.position);
            // Distance from the main ring, which every point is exactly the
            // tube radius from.
            let radial = Vec3::new(position.x, 0.0, position.z).length() - 1.0;
            let distance = (radial * radial + position.y * position.y).sqrt();
            assert!((distance - 0.25).abs() < 1e-4, "{distance} is off the tube");
        }
    }

    #[test]
    fn degenerate_subdivision_counts_are_clamped_rather_than_empty() {
        // A UI offering a subdivision slider can ask for nonsense; a mesh
        // with no triangles is a validation error later, not here.
        for (vertices, indices) in [
            sphere_geometry(1.0, 0, 0),
            plane_geometry(1.0, 0),
            torus_geometry(1.0, 0.2, 0, 0),
        ] {
            assert!(!vertices.is_empty());
            assert!(indices.len() >= 3);
        }
    }

    #[test]
    fn mesh_kind_names_round_trip_and_cycle() {
        for kind in MeshKind::ALL {
            assert_eq!(MeshKind::parse(kind.name()), Some(*kind));
        }
        assert_eq!(MeshKind::parse(" Sphere "), Some(MeshKind::Sphere));
        assert_eq!(MeshKind::parse("teapot"), None);
        // Cycling visits every kind and comes back.
        let mut kind = MeshKind::default();
        for _ in MeshKind::ALL {
            kind = kind.next();
        }
        assert_eq!(kind, MeshKind::default());
    }

    #[test]
    fn every_face_has_a_full_uv_square() {
        let (vertices, _) = cube_geometry(1.0);
        for face in vertices.chunks(4) {
            let mut corners: Vec<[f32; 2]> = face.iter().map(|v| v.uv).collect();
            corners.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in UVs"));
            assert_eq!(
                corners,
                vec![[0.0, 0.0], [0.0, 1.0], [1.0, 0.0], [1.0, 1.0]]
            );
        }
    }
}
