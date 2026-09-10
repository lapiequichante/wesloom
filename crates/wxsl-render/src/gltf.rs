//! glTF geometry import, behind the `gltf` feature.
//!
//! Geometry only, and deliberately: a glTF file's materials are a PBR
//! parameter set, and this project's materials are *graphs*. Importing the
//! former as the latter would either be a lie (a fixed graph with the
//! numbers poked in) or a translator nobody asked for. What a scene needs
//! from a file is the mesh; the material comes from the editor
//! ([ADR 0021](../../../docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md)).
//!
//! What is filled in, and how:
//!
//! * **Positions** are required. A primitive without them is skipped.
//! * **Normals**, when absent, are accumulated per face and normalized —
//!   flat-shaded geometry comes out smooth-ish rather than black.
//! * **Tangents**, when absent, are derived from the UVs, and from an
//!   arbitrary perpendicular where there are no UVs either. The vertex
//!   stage builds a frame from them, so a zero tangent is a normal map that
//!   silently does nothing.
//! * **Indices**, when absent, are generated as `0..vertex_count`.
//! * Only triangles. A points or lines primitive is skipped rather than
//!   reinterpreted.
//! * **`COLOR_0` and `TEXCOORD_1`** come across as *named streams*
//!   ([`COLOR_ATTRIBUTE`], [`UV1_ATTRIBUTE`]) rather than as fields of
//!   [`Vertex`]. They are what a material *may* declare and most do not,
//!   which is exactly the shape a declared per-vertex attribute has
//!   ([ADR 0024](../../../docs/adr/0024-a-material-declares-the-geometry-it-requires.md)).
//!   Nothing else is: joints and weights want a skinning stage that does
//!   not exist yet, and inventing a name for every semantic a file might
//!   carry is a vocabulary nobody agreed to.
//!
//! Node transforms *are* applied: a glTF mesh is authored in its node's
//! space, and a file whose parts land on top of each other is not an
//! import, it is a puzzle.

use glam::{Mat4, Vec2, Vec3};

use crate::error::RenderError;
use crate::mesh::{AttributeValues, MeshData, Vertex};

/// Name a glTF `COLOR_0` stream is imported under, and therefore the name
/// a graph declares to read it.
pub const COLOR_ATTRIBUTE: &str = "color";
/// Name a glTF `TEXCOORD_1` stream is imported under.
pub const UV1_ATTRIBUTE: &str = "uv1";

/// Read every triangle primitive of a glTF or GLB file, in depth-first node
/// order, each in world space.
pub fn load(path: impl AsRef<std::path::Path>) -> Result<Vec<MeshData>, RenderError> {
    let path = path.as_ref();
    let (document, buffers, _images) =
        ::gltf::import(path).map_err(|error| RenderError::Import {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;

    let mut primitives = Vec::new();
    for scene in document.scenes() {
        for node in scene.nodes() {
            visit(&node, Mat4::IDENTITY, &buffers, &mut primitives);
        }
    }
    if primitives.is_empty() {
        return Err(RenderError::Import {
            path: path.display().to_string(),
            reason: "no triangle primitive with positions".to_string(),
        });
    }
    Ok(primitives)
}

/// Every primitive of a file, merged into one mesh.
pub fn load_merged(path: impl AsRef<std::path::Path>) -> Result<MeshData, RenderError> {
    let mut merged = MeshData::default();
    for primitive in load(path)? {
        merged.extend(&primitive);
    }
    Ok(merged)
}

fn visit(
    node: &::gltf::Node<'_>,
    parent: Mat4,
    buffers: &[::gltf::buffer::Data],
    out: &mut Vec<MeshData>,
) {
    let transform = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
    if let Some(mesh) = node.mesh() {
        for primitive in mesh.primitives() {
            if primitive.mode() != ::gltf::mesh::Mode::Triangles {
                continue;
            }
            if let Some(data) = read_primitive(&primitive, transform, buffers) {
                out.push(data);
            }
        }
    }
    for child in node.children() {
        visit(&child, transform, buffers, out);
    }
}

fn read_primitive(
    primitive: &::gltf::Primitive<'_>,
    transform: Mat4,
    buffers: &[::gltf::buffer::Data],
) -> Option<MeshData> {
    let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(|data| &data.0[..]));
    let positions: Vec<Vec3> = reader
        .read_positions()?
        .map(|position| transform.transform_point3(Vec3::from_array(position)))
        .collect();
    if positions.is_empty() {
        return None;
    }

    let indices: Vec<u32> = match reader.read_indices() {
        Some(indices) => indices.into_u32().collect(),
        None => (0..positions.len() as u32).collect(),
    };

    // Normals and tangents live in the node's space too, but a normal is
    // transformed by the inverse transpose or a non-uniform scale shears it
    // off the surface.
    let normal_matrix = Mat4::from_mat3(glam::Mat3::from_mat4(transform).inverse().transpose());
    let normals: Vec<Vec3> = match reader.read_normals() {
        Some(normals) => normals
            .map(|normal| {
                normal_matrix
                    .transform_vector3(Vec3::from_array(normal))
                    .normalize_or(Vec3::Y)
            })
            .collect(),
        None => derive_normals(&positions, &indices),
    };

    let uvs: Vec<Vec2> = match reader.read_tex_coords(0) {
        Some(coords) => coords.into_f32().map(Vec2::from_array).collect(),
        None => vec![Vec2::ZERO; positions.len()],
    };

    let tangents: Vec<[f32; 4]> = match reader.read_tangents() {
        Some(tangents) => tangents
            .map(|tangent| {
                let direction = transform
                    .transform_vector3(Vec3::new(tangent[0], tangent[1], tangent[2]))
                    .normalize_or(Vec3::X);
                [direction.x, direction.y, direction.z, tangent[3]]
            })
            .collect(),
        None => derive_tangents(&positions, &normals, &uvs, &indices),
    };

    let vertices = (0..positions.len())
        .map(|index| Vertex {
            position: positions[index].to_array(),
            normal: normals.get(index).copied().unwrap_or(Vec3::Y).to_array(),
            tangent: tangents.get(index).copied().unwrap_or([1.0, 0.0, 0.0, 1.0]),
            uv: uvs.get(index).copied().unwrap_or(Vec2::ZERO).to_array(),
        })
        .collect();
    let mut data = MeshData::new(vertices, indices);

    // Always `vec4f`, whatever the file stored: glTF allows RGB or RGBA at
    // three precisions, `into_rgba_f32` normalizes all six, and a material
    // that declared `color` should not have to have been authored against
    // one particular exporter.
    if let Some(colors) = reader.read_colors(0) {
        let values: Vec<[f32; 4]> = colors.into_rgba_f32().collect();
        if values.len() == positions.len() {
            data.attributes
                .insert(COLOR_ATTRIBUTE.to_string(), AttributeValues::Vec4(values));
        }
    }
    if let Some(coords) = reader.read_tex_coords(1) {
        let values: Vec<[f32; 2]> = coords.into_f32().collect();
        if values.len() == positions.len() {
            data.attributes
                .insert(UV1_ATTRIBUTE.to_string(), AttributeValues::Vec2(values));
        }
    }
    Some(data)
}

/// Face normals accumulated onto their vertices, then normalized.
fn derive_normals(positions: &[Vec3], indices: &[u32]) -> Vec<Vec3> {
    let mut normals = vec![Vec3::ZERO; positions.len()];
    for triangle in indices.chunks_exact(3) {
        let [a, b, c] = [
            positions[triangle[0] as usize],
            positions[triangle[1] as usize],
            positions[triangle[2] as usize],
        ];
        // Unnormalized, so a large triangle counts for more than a sliver —
        // the usual area weighting, for free.
        let face = (b - a).cross(c - a);
        for index in triangle {
            normals[*index as usize] += face;
        }
    }
    normals
        .into_iter()
        .map(|normal| normal.normalize_or(Vec3::Y))
        .collect()
}

/// Per-triangle tangents from the UV derivatives, accumulated and
/// orthonormalized against the normal.
fn derive_tangents(
    positions: &[Vec3],
    normals: &[Vec3],
    uvs: &[Vec2],
    indices: &[u32],
) -> Vec<[f32; 4]> {
    let mut accumulated = vec![Vec3::ZERO; positions.len()];
    for triangle in indices.chunks_exact(3) {
        let [i0, i1, i2] = [
            triangle[0] as usize,
            triangle[1] as usize,
            triangle[2] as usize,
        ];
        let edge1 = positions[i1] - positions[i0];
        let edge2 = positions[i2] - positions[i0];
        let delta1 = uvs[i1] - uvs[i0];
        let delta2 = uvs[i2] - uvs[i0];
        let determinant = delta1.x * delta2.y - delta2.x * delta1.y;
        if determinant.abs() < 1e-12 {
            // Degenerate UVs: no direction to derive, and guessing one is
            // worse than leaving the fallback below to handle it.
            continue;
        }
        let tangent = (edge1 * delta2.y - edge2 * delta1.y) / determinant;
        for index in [i0, i1, i2] {
            accumulated[index] += tangent;
        }
    }

    accumulated
        .into_iter()
        .enumerate()
        .map(|(index, tangent)| {
            let normal = normals.get(index).copied().unwrap_or(Vec3::Y);
            // Gram-Schmidt: the vertex stage assumes an orthonormal frame,
            // and interpolation is unkind enough to it already.
            let orthogonal = (tangent - normal * normal.dot(tangent)).normalize_or_zero();
            let direction = if orthogonal.length_squared() > 0.5 {
                orthogonal
            } else {
                any_perpendicular(normal)
            };
            [direction.x, direction.y, direction.z, 1.0]
        })
        .collect()
}

/// Some unit vector perpendicular to `normal`, for geometry with no UVs.
fn any_perpendicular(normal: Vec3) -> Vec3 {
    let axis = if normal.x.abs() < 0.9 {
        Vec3::X
    } else {
        Vec3::Y
    };
    normal.cross(axis).normalize_or(Vec3::X)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normals_are_derived_from_the_faces_when_a_file_has_none() {
        // One triangle in the xz plane, wound counter-clockwise seen from
        // +y: every vertex normal should come out +y.
        let positions = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 0.0),
        ];
        let normals = derive_normals(&positions, &[0, 1, 2]);
        for normal in normals {
            assert!((normal - Vec3::Y).length() < 1e-5, "{normal:?}");
        }
    }

    #[test]
    fn tangents_follow_the_uv_direction_and_stay_orthonormal() {
        let positions = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ];
        let normals = vec![Vec3::Y; 3];
        // u runs along +x, so the tangent must too.
        let uvs = vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)];
        let tangents = derive_tangents(&positions, &normals, &uvs, &[0, 1, 2]);
        for tangent in tangents {
            let direction = Vec3::new(tangent[0], tangent[1], tangent[2]);
            assert!((direction - Vec3::X).length() < 1e-5, "{direction:?}");
            assert!((direction.length() - 1.0).abs() < 1e-5);
            assert!(direction.dot(Vec3::Y).abs() < 1e-5);
        }
    }

    #[test]
    fn geometry_with_no_uvs_still_gets_a_usable_frame() {
        // Degenerate UVs give no direction at all, and a zero tangent is a
        // tangent frame that collapses the moment a normal map is used.
        let positions = vec![Vec3::ZERO, Vec3::X, Vec3::Z];
        let normals = vec![Vec3::Y; 3];
        let uvs = vec![Vec2::ZERO; 3];
        for tangent in derive_tangents(&positions, &normals, &uvs, &[0, 1, 2]) {
            let direction = Vec3::new(tangent[0], tangent[1], tangent[2]);
            assert!((direction.length() - 1.0).abs() < 1e-5, "{direction:?}");
            assert!(direction.dot(Vec3::Y).abs() < 1e-5);
        }
    }

    #[test]
    fn merging_primitives_shifts_the_second_ones_indices() {
        let mut first = MeshData::new(vec![Vertex::default(); 3], vec![0, 1, 2]);
        let second = MeshData::new(vec![Vertex::default(); 3], vec![0, 1, 2]);
        first.extend(&second);
        assert_eq!(first.indices, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(first.triangle_count(), 2);
    }
}
