//! `wxsl-prt`: precomputed radiance transfer, baked offline on the CPU
//! ([ADR 0092](../../../docs/adr/0092-precomputed-radiance-transfer-is-baked-offline-and-applied-as-environment-occlusion.md)).
//!
//! For every vertex of an imported glTF scene, [`bake`] traces rays
//! against the whole scene in world space. Each vertex gets:
//!
//! - **the diffuse transfer:** its cosine-weighted visibility, projected
//!   onto order-3 spherical harmonics ([`sh`]), plus, per bounce, the
//!   light its surroundings reflect onto it, coloured by their [`Albedo`]
//!   sampled where each ray lands;
//! - **the visibility cone:** the bent normal, scaled by the ambient
//!   occlusion.
//!
//! A bounce is either weighed by the albedo's luminance
//! ([`Transfer::Gray`], 9 coefficients) or per channel
//! ([`Transfer::Color`], 27); one bake produces either or both from the
//! same rays. [`Bake`] writes each to a sidecar file. At load time,
//! [`Bake::streams`] packs a mesh's values into two or four `vec4` vertex
//! streams, and [`apply`] rewires a material graph to turn them into the
//! surface's `diffuse_occlusion` and `specular_occlusion`, lit by the
//! environment's own coefficients ([`sh::project_equirect`]).
//!
//! A model can also shadow a surface it stands on: [`ground`] lays a grid
//! under it ([`GroundGrid`]) and [`bake_receiver`] bakes that grid alone,
//! against the model, for a material that darkens what is under it.
//!
//! The lighting can change after the bake and the geometry cannot: the
//! transfer belongs to one pose, the first frame of the file's first clip
//! by default ([`BakeOptions::clip`]), skinned on the CPU. No device is
//! involved.

mod albedo;
mod bvh;
mod ground;
pub mod sh;
mod trace;

use std::fmt;
use std::io::{self, Read, Write};

use wxsl_core::graph::{AttributeDecl, StageConstraint};
use wxsl_core::node::{Value, ValueType, SETTING_NAME};
use wxsl_core::{Graph, NodeId, NodeRegistry, SocketRef};
use wxsl_gltf::ImportedScene;

pub use albedo::Albedo;
pub use ground::GroundGrid;
pub use sh::Sh9;

/// The vertex streams a baked mesh carries and an applied graph declares;
/// a grayscale bake uses the first two.
pub const STREAMS: [&str; 4] = ["prt0", "prt1", "prt2", "prt3"];
/// The material parameter that blends between no PRT (0) and PRT (1).
pub const STRENGTH_PARAM: &str = "prt_strength";
/// The material parameters holding the environment's coefficients, `0`
/// to `8` appended.
pub const SH_PARAM_PREFIX: &str = "prt_sh";

const MAGIC: &[u8; 8] = b"WXSLPRT\0";
const VERSION: u32 = 2;

/// The largest magnitude a packed value keeps; beyond it, it is clamped.
const RANGE: f32 = 4.0;

/// Something a bake, a sidecar or a graph refused, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrtError(pub String);

impl fmt::Display for PrtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PrtError {}

impl From<io::Error> for PrtError {
    fn from(error: io::Error) -> Self {
        PrtError(error.to_string())
    }
}

/// How bounced light is coloured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Transfer {
    /// One set of coefficients: each bounce weighed by the luminance of
    /// the surface it comes off. Two vertex streams.
    #[default]
    Gray,
    /// One set per channel, so a red panel bounces red. Four vertex
    /// streams.
    Color,
}

impl Transfer {
    /// Both, grayscale first.
    pub const ALL: [Transfer; 2] = [Transfer::Gray, Transfer::Color];

    /// The name a sidecar carries: `<model>.<name>.prt`.
    pub fn name(self) -> &'static str {
        match self {
            Transfer::Gray => "gray",
            Transfer::Color => "color",
        }
    }

    /// Transfer coefficients per vertex.
    pub fn coefficients(self) -> usize {
        match self {
            Transfer::Gray => 9,
            Transfer::Color => 27,
        }
    }

    /// Floats per baked vertex: the visibility, then the coefficients.
    pub fn values_per_vertex(self) -> usize {
        3 + self.coefficients()
    }

    /// The vertex streams this mode packs into.
    pub fn streams(self) -> &'static [&'static str] {
        match self {
            Transfer::Gray => &STREAMS[..2],
            Transfer::Color => &STREAMS[..],
        }
    }
}

/// How to bake.
#[derive(Clone, Debug)]
pub struct BakeOptions {
    /// Rays per vertex, over the cosine-weighted hemisphere.
    pub rays: u32,
    /// Bounces of diffuse light after the direct, shadowed one; 0 bakes
    /// shadowing alone.
    pub bounces: u32,
    /// The clip whose first frame poses the scene: its nodes, and the
    /// skinned vertices their joints move. `None`, or a clip the file
    /// does not have, bakes the pose the file was saved in.
    pub clip: Option<usize>,
    /// The modes to produce, from the same rays.
    pub transfers: Vec<Transfer>,
    /// An infinite floor under the scene's lowest point that blocks the
    /// lower hemisphere, for a model meant to stand on the ground.
    pub floor: bool,
    /// Worker threads; 0 uses every core.
    pub threads: usize,
}

impl Default for BakeOptions {
    fn default() -> Self {
        BakeOptions {
            rays: 256,
            bounces: 3,
            clip: Some(0),
            transfers: Transfer::ALL.to_vec(),
            floor: false,
            threads: 0,
        }
    }
}

/// Bake every mesh of `imported`, once per mode in `options.transfers`.
///
/// The scene is posed at the first frame of `options.clip`. Every
/// instance is an occluder, except those whose material is transparent or
/// refractive, which light passes through; an alpha-masked one blocks
/// only where its texture is solid. Each mesh is baked where its first
/// instance places it. `albedo` colours the bounces and holds the masks;
/// [`Albedo::load`] reads it from the file `imported` came from.
pub fn bake(
    imported: &ImportedScene,
    albedo: &Albedo,
    options: &BakeOptions,
) -> Result<Vec<Bake>, PrtError> {
    trace::bake(imported, albedo, None, options)
}

/// Bake every mesh of `receiver`, a surface `imported` shadows but that
/// shadows nothing itself (a [`ground`]), once per mode in
/// `options.transfers`.
///
/// `imported` is posed and occludes as in [`bake`]; `receiver` is placed
/// as saved, in the same space. With bounces, `imported` is traced too, for
/// the light it sends onto the receiver, and only the receiver's transfer
/// is returned, per mesh of `receiver`.
pub fn bake_receiver(
    imported: &ImportedScene,
    albedo: &Albedo,
    receiver: &ImportedScene,
    options: &BakeOptions,
) -> Result<Vec<Bake>, PrtError> {
    trace::bake(imported, albedo, Some(receiver), options)
}

/// A flat grid under `imported`, posed for `options.clip`, as a binary
/// glTF in the model's space: the receiver [`bake_receiver`] bakes.
pub fn ground(
    imported: &ImportedScene,
    options: &BakeOptions,
    grid: &GroundGrid,
) -> Result<Vec<u8>, PrtError> {
    let (min, max) = trace::bounds(imported, options.clip)
        .ok_or_else(|| PrtError("the model has no geometry to stand a ground under".to_string()))?;
    ground::glb(min, max, grid)
}

/// A scene's baked transfer in one mode, per imported mesh in
/// [`ImportedScene::meshes`] order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bake {
    /// How the bounces are coloured.
    pub transfer: Transfer,
    /// Per mesh, [`Transfer::values_per_vertex`] floats per vertex: the
    /// bent normal times the ambient occlusion, then the coefficients
    /// (per channel, red first, in colour).
    pub meshes: Vec<Vec<f32>>,
}

impl Bake {
    /// Mesh `index`'s values packed into the [`Transfer::streams`].
    ///
    /// Two values share a float as 12-bit codes over ±4,
    /// `high * 4096 + low`, which a 32-bit float stores exactly. The last
    /// six floats hold nine coefficients and the visibility (grayscale,
    /// or blue in colour). A colour bake puts red and green first, one
    /// coefficient per float. `lighting.prt_unpack` reads it back.
    pub fn streams(&self, index: usize) -> Option<Vec<Vec<[f32; 4]>>> {
        let stride = self.transfer.values_per_vertex();
        let streams = self.transfer.streams().len();
        let mut out = vec![Vec::new(); streams];
        for values in self.meshes.get(index)?.chunks_exact(stride) {
            let (visibility, coefficients) = values.split_at(3);
            let mut floats = vec![pack(0.0, 0.0); streams * 4];
            let (last, tail) = match self.transfer {
                Transfer::Gray => (coefficients, &mut floats[2..]),
                Transfer::Color => {
                    for k in 0..9 {
                        floats[k] = pack(coefficients[k], coefficients[9 + k]);
                    }
                    (&coefficients[18..], &mut floats[10..])
                }
            };
            let block: Vec<f32> = last.iter().chain(visibility).copied().collect();
            for (slot, pair) in tail.iter_mut().zip(block.chunks_exact(2)) {
                *slot = pack(pair[0], pair[1]);
            }
            for (stream, quad) in out.iter_mut().zip(floats.chunks_exact(4)) {
                stream.push([quad[0], quad[1], quad[2], quad[3]]);
            }
        }
        Some(out)
    }

    /// Refuse a bake made for another file: the mesh count and every
    /// vertex count must match the import.
    pub fn check(&self, imported: &ImportedScene) -> Result<(), PrtError> {
        if self.meshes.len() != imported.meshes.len() {
            return Err(PrtError(format!(
                "the bake has {} meshes and the model {}; bake it again",
                self.meshes.len(),
                imported.meshes.len()
            )));
        }
        let stride = self.transfer.values_per_vertex();
        for (index, (baked, mesh)) in self.meshes.iter().zip(&imported.meshes).enumerate() {
            if baked.len() != mesh.vertices.len() * stride {
                return Err(PrtError(format!(
                    "mesh {index} has {} baked vertices and {} in the model; bake it again",
                    baked.len() / stride,
                    mesh.vertices.len()
                )));
            }
        }
        Ok(())
    }

    /// The sidecar format: a magic, a version, the mode, the mesh count,
    /// each mesh's vertex count, then every float, all little-endian.
    pub fn write(&self, mut out: impl Write) -> Result<(), PrtError> {
        let stride = self.transfer.values_per_vertex();
        out.write_all(MAGIC)?;
        out.write_all(&VERSION.to_le_bytes())?;
        out.write_all(&(self.transfer as u32).to_le_bytes())?;
        out.write_all(&(self.meshes.len() as u32).to_le_bytes())?;
        for mesh in &self.meshes {
            out.write_all(&((mesh.len() / stride) as u32).to_le_bytes())?;
        }
        let mut bytes = Vec::new();
        for mesh in &self.meshes {
            bytes.clear();
            bytes.extend(mesh.iter().flat_map(|value| value.to_le_bytes()));
            out.write_all(&bytes)?;
        }
        Ok(())
    }

    /// Read what [`Bake::write`] wrote.
    pub fn read(mut input: impl Read) -> Result<Self, PrtError> {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes)?;
        let Some(mut cursor) = bytes.strip_prefix(MAGIC.as_slice()) else {
            return Err(PrtError("not a wxsl PRT bake".to_string()));
        };
        let version = word(&mut cursor)?;
        if version != VERSION {
            return Err(PrtError(format!(
                "bake version {version}, this build reads {VERSION}; bake it again"
            )));
        }
        let transfer = match word(&mut cursor)? {
            0 => Transfer::Gray,
            1 => Transfer::Color,
            other => return Err(PrtError(format!("unknown transfer mode {other}"))),
        };
        let stride = transfer.values_per_vertex();
        let count = word(&mut cursor)? as usize;
        let lengths = (0..count)
            .map(|_| word(&mut cursor))
            .collect::<Result<Vec<_>, _>>()?;
        let mut meshes = Vec::with_capacity(count);
        for length in lengths {
            let size = length as usize * stride * 4;
            if cursor.len() < size {
                return Err(truncated());
            }
            let (data, rest) = cursor.split_at(size);
            cursor = rest;
            meshes.push(
                data.chunks_exact(4)
                    .map(|value| f32::from_le_bytes(value.try_into().unwrap()))
                    .collect(),
            );
        }
        if !cursor.is_empty() {
            return Err(PrtError("trailing bytes after the bake".to_string()));
        }
        Ok(Bake { transfer, meshes })
    }
}

/// Two values as one float: 12-bit codes over ±[`RANGE`].
fn pack(high: f32, low: f32) -> f32 {
    let code = |value: f32| (value.clamp(-RANGE, RANGE) * (2047.0 / RANGE)).round() + 2047.0;
    code(high) * 4096.0 + code(low)
}

fn truncated() -> PrtError {
    PrtError("the bake file is truncated".to_string())
}

/// The next little-endian `u32` of `cursor`.
fn word(cursor: &mut &[u8]) -> Result<u32, PrtError> {
    let (head, rest) = cursor.split_first_chunk::<4>().ok_or_else(truncated)?;
    *cursor = rest;
    Ok(u32::from_le_bytes(*head))
}

/// Rewire every surface output of `graph` to read a `transfer` bake.
///
/// The graph declares the mode's [`Transfer::streams`]. It gains the
/// parameters `prt_sh0`..`prt_sh8` (starting at `environment`) and
/// [`STRENGTH_PARAM`] (starting at 1). The PRT nodes then drive the
/// surface's `diffuse_occlusion` and `specular_occlusion`, blended from 1
/// by the strength. The streams are decoded in the vertex stage, so only
/// the results are interpolated. A graph whose vertex output replaces the
/// normal (a skinned one) decodes against that normal, in world space. The reflection reads the roughness the
/// surface already gets. A surface that already drives either field is
/// refused.
pub fn apply(
    graph: &mut Graph,
    registry: &NodeRegistry,
    environment: &Sh9,
    transfer: Transfer,
) -> Result<(), PrtError> {
    let surfaces = graph.surface_outputs(registry);
    if surfaces.is_empty() {
        return Err(PrtError(format!(
            "`{}` has no surface output",
            graph.name()
        )));
    }
    for surface in &surfaces {
        for field in ["diffuse_occlusion", "specular_occlusion"] {
            if graph.edge_into(&SocketRef::new(*surface, field)).is_some() {
                return Err(PrtError(format!(
                    "`{}` already drives `{field}`",
                    graph.name()
                )));
            }
        }
    }
    let mut builder = Builder { graph, registry };
    let streams: Vec<NodeId> = transfer
        .streams()
        .iter()
        .map(|stream| {
            builder
                .graph
                .declare_attribute(AttributeDecl::vertex(*stream, ValueType::Vec4));
            let node = builder.add_named("input.attribute", stream);
            builder.generic(node, "T", ValueType::Vec4);
            node
        })
        .collect();
    let normal = builder.add("input.world_normal");
    let diffuse_normal = builder.vertex_normal()?.unwrap_or(normal);
    let view = builder.add("input.view_direction");
    let strength = builder.param(STRENGTH_PARAM, Value::F32(1.0));

    let diffuse = builder.add(match transfer {
        Transfer::Gray => "lighting.prt_diffuse_occlusion",
        Transfer::Color => "lighting.prt_diffuse_occlusion_color",
    });
    builder.graph.set_stage(diffuse, StageConstraint::Vertex);
    for (stream, socket) in streams.iter().zip(STREAMS) {
        builder.wire((*stream, "out"), (diffuse, socket))?;
    }
    builder.wire((diffuse_normal, "out"), (diffuse, "normal"))?;
    for (index, coefficient) in environment.iter().enumerate() {
        let param = builder.param(
            &format!("{SH_PARAM_PREFIX}{index}"),
            Value::Vec3(*coefficient),
        );
        builder.wire((param, "out"), (diffuse, &format!("sh{index}")))?;
    }
    let diffuse_blend = builder.blend(ValueType::Vec3, Value::Vec3([1.0; 3]), diffuse, strength)?;

    let visibility = builder.add("lighting.prt_visibility");
    builder.graph.set_stage(visibility, StageConstraint::Vertex);
    let last = *streams.last().expect("every mode has streams");
    builder.wire((last, "out"), (visibility, "last"))?;
    let specular = builder.add("lighting.prt_specular_occlusion");
    builder.wire((visibility, "out"), (specular, "visibility"))?;
    builder.wire((normal, "out"), (specular, "normal"))?;
    builder.wire((view, "out"), (specular, "view_direction"))?;
    let specular_blend = builder.blend(ValueType::F32, Value::F32(1.0), specular, strength)?;

    for surface in surfaces {
        match builder
            .graph
            .edge_into(&SocketRef::new(surface, "roughness"))
            .map(|edge| edge.from.clone())
        {
            Some(from) => builder.wire((from.node, &from.socket), (specular, "roughness"))?,
            None => {
                let roughness = builder
                    .graph
                    .node(surface)
                    .and_then(|node| node.params.get("roughness").cloned());
                if let Some(roughness) = roughness {
                    builder.graph.set_param(specular, "roughness", roughness);
                }
            }
        }
        builder.wire((diffuse_blend, "out"), (surface, "diffuse_occlusion"))?;
        builder.wire((specular_blend, "out"), (surface, "specular_occlusion"))?;
    }
    Ok(())
}

struct Builder<'g, 'r> {
    graph: &'g mut Graph,
    registry: &'r NodeRegistry,
}

impl Builder<'_, '_> {
    fn add(&mut self, def: &str) -> NodeId {
        self.graph
            .add_resolved(self.registry, wxsl_core::Node::new(def))
    }

    fn add_named(&mut self, def: &str, name: &str) -> NodeId {
        self.graph.add_resolved(
            self.registry,
            wxsl_core::Node::new(def).with_setting(SETTING_NAME, name),
        )
    }

    fn generic(&mut self, node: NodeId, param: &str, ty: ValueType) {
        // Every generic here is one this module chose; a refusal is a bug.
        if let Err(error) = self.graph.set_generic(self.registry, node, param, ty) {
            panic!("prt generic {param} = {ty:?}: {error}");
        }
    }

    /// The world normal of a vertex output that replaces the mesh's, in
    /// the vertex stage: `normalize(normal_matrix * normal)`.
    fn vertex_normal(&mut self) -> Result<Option<NodeId>, PrtError> {
        let replaced = self.graph.nodes().find_map(|(id, node)| {
            (node.def == wxsl_core::abi::VERTEX_OUTPUT_ID)
                .then(|| {
                    let socket = SocketRef::new(id, wxsl_core::abi::SOCKET_VERTEX_NORMAL);
                    self.graph.edge_into(&socket).map(|edge| edge.from.clone())
                })
                .flatten()
        });
        let Some(from) = replaced else {
            return Ok(None);
        };
        let matrix = self.add("input.normal_matrix");
        let turn = self.add("math.multiply");
        self.generic(turn, "A", ValueType::Mat3);
        self.generic(turn, "B", ValueType::Vec3);
        self.wire((matrix, "out"), (turn, "a"))?;
        self.wire((from.node, &from.socket), (turn, "b"))?;
        let unit = self.add("math.safe_normalize");
        self.generic(unit, "T", ValueType::Vec3);
        self.wire((turn, "out"), (unit, "v"))?;
        for node in [turn, unit] {
            self.graph.set_stage(node, StageConstraint::Vertex);
        }
        Ok(Some(unit))
    }

    fn param(&mut self, name: &str, value: Value) -> NodeId {
        let node = self.add_named("param.value", name);
        self.generic(node, "T", value.ty());
        self.graph.set_param(node, "value", value);
        node
    }

    fn wire(&mut self, from: (NodeId, &str), to: (NodeId, &str)) -> Result<(), PrtError> {
        self.graph
            .wire(self.registry, from, to)
            .map_err(|error| PrtError(format!("wiring PRT into `{}`: {error}", self.graph.name())))
    }

    /// `mix(unoccluded, value.out, strength)`.
    fn blend(
        &mut self,
        ty: ValueType,
        unoccluded: Value,
        value: NodeId,
        strength: NodeId,
    ) -> Result<NodeId, PrtError> {
        let mix = self.add("math.mix");
        self.generic(mix, "T", ty);
        self.graph.set_param(mix, "a", unoccluded);
        self.wire((value, "out"), (mix, "b"))?;
        self.wire((strength, "out"), (mix, "t"))?;
        Ok(mix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `lighting.prt_unpack` does, on the CPU.
    fn unpack(packed: f32) -> [f32; 2] {
        let high = (packed / 4096.0).floor();
        let low = packed - high * 4096.0;
        [high, low].map(|code| (code - 2047.0) * (RANGE / 2047.0))
    }

    #[test]
    fn packed_pairs_come_back_within_a_code() {
        for (high, low) in [
            (0.0, 0.0),
            (1.5, -0.25),
            (-4.0, 4.0),
            (9.0, -9.0),
            (0.886, 0.001),
        ] {
            let [a, b] = unpack(pack(high, low));
            let step = RANGE / 2047.0;
            assert!(
                (a - high.clamp(-RANGE, RANGE)).abs() <= step * 0.5 + 1e-6,
                "{high} → {a}"
            );
            assert!(
                (b - low.clamp(-RANGE, RANGE)).abs() <= step * 0.5 + 1e-6,
                "{low} → {b}"
            );
        }
        assert_eq!(unpack(pack(0.0, 0.0)), [0.0, 0.0]);
    }

    #[test]
    fn a_bake_round_trips_through_its_file_and_packs_where_the_nodes_read() {
        for transfer in Transfer::ALL {
            let stride = transfer.values_per_vertex();
            let vertex: Vec<f32> = (0..stride).map(|index| index as f32 * 0.1 - 1.0).collect();
            let bake = Bake {
                transfer,
                meshes: vec![vertex.repeat(2), vec![], vertex.clone()],
            };
            let mut bytes = Vec::new();
            bake.write(&mut bytes).unwrap();
            assert_eq!(Bake::read(bytes.as_slice()).unwrap(), bake);
            assert!(Bake::read(&bytes[..bytes.len() - 1]).is_err());

            let streams = bake.streams(2).unwrap();
            assert_eq!(streams.len(), transfer.streams().len());
            let floats: Vec<f32> = streams.iter().flat_map(|stream| stream[0]).collect();
            let near = |a: f32, b: f32| (a - b).abs() < 3e-3;
            // The visibility: the last stream's z low, w high and low.
            let tail = &floats[floats.len() - 2..];
            assert!(near(unpack(tail[0])[1], vertex[0]));
            assert!(near(unpack(tail[1])[0], vertex[1]));
            assert!(near(unpack(tail[1])[1], vertex[2]));
            // The first coefficient of the last block.
            let block = floats.len() - 6;
            let first_of_block = match transfer {
                Transfer::Gray => 3,
                Transfer::Color => 3 + 18,
            };
            assert!(near(unpack(floats[block])[0], vertex[first_of_block]));
            if transfer == Transfer::Color {
                // Red and green of coefficient 4, paired in float 4.
                let [red, green] = unpack(floats[4]);
                assert!(near(red, vertex[3 + 4]) && near(green, vertex[3 + 9 + 4]));
            }
        }
        assert!(Bake::read(&b"not a bake"[..]).is_err());
    }
}
