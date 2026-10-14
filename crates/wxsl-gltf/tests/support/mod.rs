//! Building small GLB files in a test, so no binary is committed for each
//! mapping (plan6 G2). Shared with the facade's GPU test.

#![allow(dead_code)]

use serde_json::{json, Value};

/// A GLB under construction: the JSON chunk and the binary chunk.
pub struct Glb {
    pub json: Value,
    bin: Vec<u8>,
}

impl Default for Glb {
    fn default() -> Self {
        Glb {
            json: json!({
                "asset": { "version": "2.0" },
                "buffers": [], "bufferViews": [], "accessors": [],
                "images": [], "textures": [], "samplers": [], "materials": [],
                "meshes": [], "nodes": [], "scenes": [{ "nodes": [] }], "scene": 0,
            }),
            bin: Vec::new(),
        }
    }
}

impl Glb {
    fn push(&mut self, key: &str, value: Value) -> usize {
        let array = self.json[key].as_array_mut().expect("an array");
        array.push(value);
        array.len() - 1
    }

    fn view(&mut self, bytes: &[u8]) -> usize {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        self.push(
            "bufferViews",
            json!({ "buffer": 0, "byteOffset": offset, "byteLength": bytes.len() }),
        )
    }

    /// A float accessor of `kind` (`"VEC3"`, `"VEC2"`, …).
    pub fn floats(&mut self, values: &[f32], kind: &str) -> usize {
        let bytes: Vec<u8> = values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        let view = self.view(&bytes);
        let width = match kind {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            _ => 4,
        };
        let mut accessor = json!({
            "bufferView": view, "componentType": 5126,
            "count": values.len() / width, "type": kind,
        });
        if kind == "VEC3" {
            let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
            for point in values.chunks(3) {
                for axis in 0..3 {
                    min[axis] = min[axis].min(point[axis]);
                    max[axis] = max[axis].max(point[axis]);
                }
            }
            accessor["min"] = json!(min);
            accessor["max"] = json!(max);
        }
        self.push("accessors", accessor)
    }

    /// One triangle in the xy plane facing +z, with UVs; returns the mesh.
    /// `material` may be `None`; `uv1` adds a second UV set.
    pub fn triangle(&mut self, material: Option<usize>, uv1: bool) -> usize {
        let position = self.floats(&[-1.0, -1.0, 0.0, 1.0, -1.0, 0.0, 0.0, 1.0, 0.0], "VEC3");
        let normal = self.floats(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "VEC3");
        let uv = self.floats(&[0.0, 1.0, 1.0, 1.0, 0.5, 0.0], "VEC2");
        let mut attributes = json!({ "POSITION": position, "NORMAL": normal, "TEXCOORD_0": uv });
        if uv1 {
            attributes["TEXCOORD_1"] = json!(uv);
        }
        let mut primitive = json!({ "attributes": attributes });
        if let Some(material) = material {
            primitive["material"] = json!(material);
        }
        self.push(
            "meshes",
            json!({ "name": "triangle", "primitives": [primitive] }),
        )
    }

    /// A PNG of one colour, in the binary chunk, as an image index.
    pub fn png(&mut self, rgba: [u8; 4]) -> usize {
        let mut bytes = Vec::new();
        image::write_buffer_with_format(
            &mut std::io::Cursor::new(&mut bytes),
            &rgba.repeat(4),
            2,
            2,
            image::ExtendedColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .expect("encodes a png");
        let view = self.view(&bytes);
        self.push(
            "images",
            json!({ "bufferView": view, "mimeType": "image/png" }),
        )
    }

    /// An image in a format the importer does not decode.
    pub fn ktx2(&mut self) -> usize {
        let view = self.view(b"\xabKTX 20\xbb\r\n\x1a\n");
        self.push(
            "images",
            json!({ "name": "compressed", "bufferView": view, "mimeType": "image/ktx2" }),
        )
    }

    pub fn texture(&mut self, image: usize) -> usize {
        self.push("textures", json!({ "source": image }))
    }

    pub fn material(&mut self, material: Value) -> usize {
        self.push("materials", material)
    }

    /// A node, added to the scene's roots unless it has a parent.
    pub fn node(&mut self, node: Value, parent: Option<usize>) -> usize {
        let index = self.push("nodes", node);
        match parent {
            Some(parent) => {
                let children = &mut self.json["nodes"][parent]["children"];
                if children.is_null() {
                    *children = json!([]);
                }
                children.as_array_mut().unwrap().push(json!(index));
            }
            None => self.json["scenes"][0]["nodes"]
                .as_array_mut()
                .unwrap()
                .push(json!(index)),
        }
        index
    }

    pub fn uses(&mut self, extension: &str) {
        let used = &mut self.json["extensionsUsed"];
        if used.is_null() {
            *used = json!([]);
        }
        used.as_array_mut().unwrap().push(json!(extension));
    }

    /// The file's bytes.
    pub fn finish(mut self) -> Vec<u8> {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
        self.json["buffers"] = json!([{ "byteLength": self.bin.len() }]);
        let mut text = serde_json::to_vec(&self.json).unwrap();
        while text.len() % 4 != 0 {
            text.push(b' ');
        }
        let total = 12 + 8 + text.len() + 8 + self.bin.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(text.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&text);
        out.extend_from_slice(&(self.bin.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&self.bin);
        out
    }
}
