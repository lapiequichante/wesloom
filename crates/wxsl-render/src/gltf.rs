//! glTF geometry import, behind the `gltf` feature.
//!
//! Geometry only: the reading is `wxsl-gltf`'s, device-free and shared with
//! the C ABI, and this module turns its [`Geometry`] into [`MeshData`]. A
//! whole file — materials, nodes, lights — is imported as a scene document
//! by `wxsl::asset`
//! ([ADR 0080](../../../docs/adr/0080-gltf-import-produces-a-scene-document-with-generated-graphs.md)).
//!
//! Node transforms *are* applied here: a `file` mesh is one flat mesh, and
//! a file whose parts land on top of each other is not an import, it is a
//! puzzle. `COLOR_0` and `TEXCOORD_1` become the named streams
//! [`COLOR_ATTRIBUTE`] and [`UV1_ATTRIBUTE`]
//! ([ADR 0024](../../../docs/adr/0024-a-material-declares-the-geometry-it-requires.md)),
//! and `JOINTS_0` and `WEIGHTS_0` the streams [`JOINTS_ATTRIBUTE`] and
//! [`WEIGHTS_ATTRIBUTE`] a skinned material reads (ADR 0099).

pub use wxsl_gltf::geometry::{
    Geometry, COLOR_ATTRIBUTE, JOINTS_ATTRIBUTE, UV1_ATTRIBUTE, WEIGHTS_ATTRIBUTE,
};

use crate::error::RenderError;
use crate::mesh::{AttributeValues, MeshData, Vertex};

/// Read every triangle primitive of a glTF or GLB file, in depth-first node
/// order, each in world space.
pub fn load(path: impl AsRef<std::path::Path>) -> Result<Vec<MeshData>, RenderError> {
    let primitives =
        wxsl_gltf::geometry::load_baked(path).map_err(|error| RenderError::Import {
            path: error.path,
            reason: error.reason,
        })?;
    Ok(primitives.iter().map(mesh_data).collect())
}

/// Every primitive of a file, merged into one mesh.
pub fn load_merged(path: impl AsRef<std::path::Path>) -> Result<MeshData, RenderError> {
    let mut merged = MeshData::default();
    for primitive in load(path)? {
        merged.extend(&primitive);
    }
    Ok(merged)
}

/// One imported primitive as uploadable mesh data, its optional streams
/// named.
pub fn mesh_data(geometry: &Geometry) -> MeshData {
    let vertices = geometry
        .vertices
        .iter()
        .map(|vertex| Vertex {
            position: vertex.position,
            normal: vertex.normal,
            tangent: vertex.tangent,
            uv: vertex.uv,
        })
        .collect();
    let mut data = MeshData::new(vertices, geometry.indices.clone());
    if let Some(colors) = &geometry.colors {
        data.attributes.insert(
            COLOR_ATTRIBUTE.to_string(),
            AttributeValues::Vec4(colors.clone()),
        );
    }
    if let Some(uv1) = &geometry.uv1 {
        data.attributes.insert(
            UV1_ATTRIBUTE.to_string(),
            AttributeValues::Vec2(uv1.clone()),
        );
    }
    // Joint indices as floats: vertex streams are float (ADR 0024), and
    // every index below 2^24 survives the trip.
    if let (Some(joints), Some(weights)) = (&geometry.joints, &geometry.weights) {
        data.attributes.insert(
            JOINTS_ATTRIBUTE.to_string(),
            AttributeValues::Vec4(joints.iter().map(|joint| joint.map(f32::from)).collect()),
        );
        data.attributes.insert(
            WEIGHTS_ATTRIBUTE.to_string(),
            AttributeValues::Vec4(weights.clone()),
        );
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_primitives_shifts_the_second_ones_indices() {
        let mut first = MeshData::new(vec![Vertex::default(); 3], vec![0, 1, 2]);
        let second = MeshData::new(vec![Vertex::default(); 3], vec![0, 1, 2]);
        first.extend(&second);
        assert_eq!(first.indices, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(first.triangle_count(), 2);
    }

    #[test]
    fn optional_streams_keep_their_attribute_names() {
        let geometry = Geometry {
            vertices: vec![wxsl_gltf::Vertex::default(); 3],
            indices: vec![0, 1, 2],
            colors: Some(vec![[1.0; 4]; 3]),
            uv1: Some(vec![[0.5; 2]; 3]),
            ..Geometry::default()
        };
        let data = mesh_data(&geometry);
        assert!(matches!(
            data.attributes[COLOR_ATTRIBUTE],
            AttributeValues::Vec4(_)
        ));
        assert!(matches!(
            data.attributes[UV1_ATTRIBUTE],
            AttributeValues::Vec2(_)
        ));
    }
}
