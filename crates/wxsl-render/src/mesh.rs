//! Geometry: the vertex format the shader ABI's vertex stage expects, and the
//! primitives to draw with it.
//!
//! [`Vertex::LAYOUT`] and `VertexIn` in `shaders/wxsl/vertex.wxsl` are the
//! two halves of one contract — the `@location` numbers must line up, so the
//! two are edited together.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

/// One vertex: position, shading basis, texture coordinates.
///
/// The tangent's `w` is the handedness of the bitangent, the usual glTF
/// convention: `bitangent = cross(normal, tangent.xyz) * tangent.w`. Storing
/// a sign rather than a third vector keeps the vertex smaller and cannot
/// disagree with the normal after interpolation.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
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

/// An indexed triangle mesh on the GPU.
pub struct Mesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

impl Mesh {
    /// Upload `vertices` and `indices` as a mesh.
    pub fn new(device: &wgpu::Device, label: &str, vertices: &[Vertex], indices: &[u16]) -> Self {
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

    /// A cube of edge length `size`, centred on the origin.
    ///
    /// Six independent faces (24 vertices), because a cube's corners have
    /// three different normals each — sharing them would round the lighting
    /// off at every edge.
    pub fn cube(device: &wgpu::Device, size: f32) -> Self {
        let (vertices, indices) = cube_geometry(size);
        Mesh::new(device, "cube", &vertices, &indices)
    }

    /// Number of indices, i.e. `triangles * 3`.
    pub fn index_count(&self) -> u32 {
        self.index_count
    }

    /// Bind this mesh's buffers and draw it once.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..self.index_count, 0, 0..1);
    }
}

/// The cube's vertices and indices, on the CPU.
///
/// Separate from [`Mesh::cube`] so it can be tested without a GPU.
pub fn cube_geometry(size: f32) -> (Vec<Vertex>, Vec<u16>) {
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
        let base = vertices.len() as u16;
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
