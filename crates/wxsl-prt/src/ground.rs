//! A ground for a model to stand on: a flat grid under its posed box,
//! written as a `.glb` of its own. Baked with [`crate::bake_receiver`], it
//! carries the model's shadow on the sky, and a material that reads that
//! transfer darkens what is drawn under the model.
//!
//! The grid is in the model's space, facing +y, just above the model's
//! lowest point, with UVs running 0..1 across it so that a shader can fade
//! its edges out.

use std::fmt::Write as _;

use glam::Vec3;

use crate::PrtError;

/// How the ground is laid out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundGrid {
    /// How far the grid reaches past the model's box on every side, metres.
    pub margin: f32,
    /// The distance between neighbouring vertices, metres.
    pub spacing: f32,
    /// How far above the model's lowest point the grid lies, metres: clear
    /// of the surface the model stands on, so the two do not fight in the
    /// depth buffer.
    pub lift: f32,
}

impl Default for GroundGrid {
    fn default() -> Self {
        GroundGrid {
            margin: 1.5,
            spacing: 0.025,
            lift: 0.01,
        }
    }
}

/// The name of the grid's mesh, node and material in the file.
const NAME: &str = "ground";

/// The grid under the box `min..max`, as a binary glTF.
pub(crate) fn glb(min: Vec3, max: Vec3, grid: &GroundGrid) -> Result<Vec<u8>, PrtError> {
    if grid.spacing <= 0.0 || grid.margin < 0.0 {
        return Err(PrtError(format!(
            "a ground needs a positive spacing and no negative margin, not {grid:?}"
        )));
    }
    let low = Vec3::new(min.x - grid.margin, min.y + grid.lift, min.z - grid.margin);
    let size = Vec3::new(
        max.x - min.x + 2.0 * grid.margin,
        0.0,
        max.z - min.z + 2.0 * grid.margin,
    );
    let columns = (size.x / grid.spacing).ceil().max(1.0) as u32;
    let rows = (size.z / grid.spacing).ceil().max(1.0) as u32;
    let count = (columns + 1) * (rows + 1);
    if u64::from(count) > u64::from(u32::MAX / 4) {
        return Err(PrtError(format!(
            "a ground of {columns}×{rows} cells is too fine; widen the spacing"
        )));
    }

    let mut positions = Vec::with_capacity(count as usize);
    let mut uvs = Vec::with_capacity(count as usize);
    for row in 0..=rows {
        for column in 0..=columns {
            let uv = [column as f32 / columns as f32, row as f32 / rows as f32];
            positions.push([low.x + uv[0] * size.x, low.y, low.z + uv[1] * size.z]);
            uvs.push(uv);
        }
    }
    // Counter-clockwise seen from above: +z is down the rows.
    let mut indices = Vec::with_capacity((columns * rows * 6) as usize);
    for row in 0..rows {
        for column in 0..columns {
            let corner = row * (columns + 1) + column;
            let below = corner + columns + 1;
            indices.extend([corner, below, corner + 1, corner + 1, below, below + 1]);
        }
    }

    let mut bin = Vec::new();
    let mut views = String::new();
    let mut view = |bytes: &[u8], target: u32| {
        if !views.is_empty() {
            views.push(',');
        }
        let _ = write!(
            views,
            r#"{{"buffer":0,"byteOffset":{},"byteLength":{},"target":{target}}}"#,
            bin.len(),
            bytes.len()
        );
        bin.extend_from_slice(bytes);
    };
    let floats =
        |values: &[f32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_le_bytes()).collect() };
    const VERTICES: u32 = 34962;
    view(&floats(positions.as_flattened()), VERTICES);
    view(
        &floats(&vec![[0.0f32, 1.0, 0.0]; positions.len()].concat()),
        VERTICES,
    );
    view(&floats(uvs.as_flattened()), VERTICES);
    let words: Vec<u8> = indices.iter().flat_map(|v| v.to_le_bytes()).collect();
    view(&words, 34963);
    let high = low + size;
    let json = format!(
        concat!(
            r#"{{"asset":{{"version":"2.0","generator":"wxsl-prt ground"}},"#,
            r#""scene":0,"scenes":[{{"nodes":[0]}}],"#,
            r#""nodes":[{{"name":"{name}","mesh":0}}],"#,
            r#""meshes":[{{"name":"{name}","primitives":[{{"attributes":{{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2}},"indices":3,"material":0}}]}}],"#,
            r#""materials":[{{"name":"{name}","pbrMetallicRoughness":{{"baseColorFactor":[1,1,1,1],"metallicFactor":0,"roughnessFactor":1}}}}],"#,
            r#""accessors":["#,
            r#"{{"bufferView":0,"componentType":5126,"count":{count},"type":"VEC3","min":[{lx},{ly},{lz}],"max":[{hx},{ly},{hz}]}},"#,
            r#"{{"bufferView":1,"componentType":5126,"count":{count},"type":"VEC3"}},"#,
            r#"{{"bufferView":2,"componentType":5126,"count":{count},"type":"VEC2"}},"#,
            r#"{{"bufferView":3,"componentType":5125,"count":{indices},"type":"SCALAR"}}],"#,
            r#""bufferViews":[{views}],"buffers":[{{"byteLength":{bytes}}}]}}"#
        ),
        name = NAME,
        count = count,
        indices = indices.len(),
        lx = low.x,
        ly = low.y,
        lz = low.z,
        hx = high.x,
        hz = high.z,
        views = views,
        bytes = bin.len(),
    );
    Ok(container(json.into_bytes(), bin))
}

/// The binary glTF container: the header, then the JSON and BIN chunks,
/// each padded to four bytes.
fn container(mut json: Vec<u8>, mut bin: Vec<u8>) -> Vec<u8> {
    json.resize(json.len().next_multiple_of(4), b' ');
    bin.resize(bin.len().next_multiple_of(4), 0);
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    for (chunk, kind) in [(&json, b"JSON"), (&bin, b"BIN\0")] {
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(chunk);
    }
    out
}
