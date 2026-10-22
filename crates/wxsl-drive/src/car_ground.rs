//! The car's occlusion on the road under it: a grid laid under the car
//! and baked against it by `wxsl-prt --ground` (`me_car_ground.glb` and
//! its `me_car_ground.gray.prt`, ADR 0092), drawn in a pass of its own
//! that multiplies what is already there (`pipeline`). Its material is
//! one node, `drive.car_ground` (`shaders/car_ground.wxsl`): the bake lit
//! by the car's probe, through the car's axes, which the vertex stage
//! turns into world space. Its vertices move towards the eye by
//! `neon::DEPTH_BIAS`, so a road a little off flat does not cut it.

use std::error::Error;
use std::path::Path;

use glam::Mat4;
use wxsl::core::abi;
use wxsl::core::graph::{AttributeDecl, Graph, Node, NodeId, StageConstraint};
use wxsl::core::material::MaterialConfig;
use wxsl::core::node::{NodeRegistry, ValueType};
use wxsl::core::scene::Tags;
use wxsl::core::Value;
use wxsl::render::mesh::AttributeValues;
use wxsl::render::{DrawItem, GpuContext, Material, MaterialBindings, Mesh, Renderer};
use wxsl_prt::{Bake, Transfer};

/// The tag the grid's draws carry: drawn by the ground pass alone.
pub const GROUND_TAG: &str = "car_ground";
/// The material parameter scaling the occlusion, 0 to 1.
const STRENGTH: &str = "strength";

pub struct CarGround {
    mesh: Mesh,
    material: Material,
    bindings: MaterialBindings,
}

impl CarGround {
    /// The grid beside the car at `car` (`<car>_ground.glb` and its
    /// grayscale bake), or `None` if either is missing.
    pub fn load(
        gpu: &GpuContext,
        renderer: &mut Renderer,
        registry: &NodeRegistry,
        car: &Path,
    ) -> Result<Option<Self>, Box<dyn Error>> {
        let stem = car.file_stem().unwrap_or_default().to_string_lossy();
        let path = car.with_file_name(format!("{stem}_ground.glb"));
        let file = path.with_file_name(format!("{stem}_ground.{}.prt", Transfer::Gray.name()));
        if !path.exists() || !file.exists() {
            return Ok(None);
        }
        let imported = wxsl::asset::import_gltf(&path)?;
        let bake = Bake::read(std::io::BufReader::new(std::fs::File::open(&file)?))
            .and_then(|bake| bake.check(&imported).map(|()| bake))
            .map_err(|error| format!("{}: {error}", file.display()))?;
        let [geometry] = imported.meshes.as_slice() else {
            return Err(format!("{}: a ground is one mesh", path.display()).into());
        };
        let mut data = wxsl::render::gltf::mesh_data(geometry);
        let streams = bake
            .streams(0)
            .ok_or_else(|| format!("{}: bake and grid disagree", file.display()))?;
        for (name, values) in bake.transfer.streams().iter().zip(streams) {
            data.attributes
                .insert(name.to_string(), AttributeValues::Vec4(values));
        }
        let graph = graph(registry)?;
        graph
            .validate(registry)
            .map_err(|error| format!("{}: {error}", graph.name()))?;
        let mut config = MaterialConfig::default().with_tags(Tags::from_iter([GROUND_TAG]));
        config.cast_shadow = false;
        let material = Material::with_lighting(&graph, registry, &config, renderer.lighting())?;
        let bindings = renderer.material_bindings(&gpu.device, &material);
        println!("  ground: {}", file.display());
        Ok(Some(CarGround {
            mesh: Mesh::upload(&gpu.device, "drive car ground", &data),
            material,
            bindings,
        }))
    }

    /// Scale the occlusion by `strength`, 0 to 1.
    pub fn set_strength(&mut self, gpu: &GpuContext, strength: f32) -> Result<(), Box<dyn Error>> {
        self.bindings.set(STRENGTH, Value::F32(strength))?;
        self.bindings.upload(&gpu.device, &gpu.queue)?;
        Ok(())
    }

    /// Its draw at `transform`, the car's file space to the world.
    pub fn draw(&self, transform: Mat4) -> DrawItem<'_> {
        DrawItem::new(&self.mesh, &self.material)
            .with_bindings(&self.bindings)
            .with_transform(transform)
    }
}

/// `drive.car_ground` fed the bake's streams decoded in the vertex stage
/// (`drive.car_ground_transfer`), the car's axes in world space (turned
/// there too), the grid's UVs and the strength,
/// into the fragment output. The surface output stays at its defaults: the
/// colour written is the node's alone.
fn graph(registry: &NodeRegistry) -> Result<Graph, Box<dyn Error>> {
    let mut graph = Graph::new("drive car ground");
    let wire = |graph: &mut Graph, from: (NodeId, &str), to: (NodeId, &str)| {
        graph
            .wire(registry, from, to)
            .map_err(|error| format!("drive car ground: {error}"))
    };
    let node = graph.add(Node::new("drive.car_ground"));
    graph.set_stage(node, StageConstraint::Fragment);
    graph.add_node(abi::SURFACE_OUTPUT_ID);
    let output = graph.add_node(abi::FRAGMENT_OUTPUT_ID);
    // Packed codes do not survive interpolation: decoded per vertex.
    let transfer = graph.add(Node::new("drive.car_ground_transfer"));
    graph.set_stage(transfer, StageConstraint::Vertex);
    for socket in ["low", "high", "last"] {
        wire(&mut graph, (transfer, socket), (node, socket))?;
    }
    for stream in Transfer::Gray.streams() {
        graph.declare_attribute(AttributeDecl::vertex(*stream, ValueType::Vec4));
        let input = graph.add(Node::new("input.attribute").with_setting("name", *stream));
        graph
            .set_generic(registry, input, "T", ValueType::Vec4)
            .map_err(|error| format!("drive car ground: {error}"))?;
        wire(&mut graph, (input, "out"), (transfer, stream))?;
    }
    let matrix = graph.add_node(abi::context_node_id("normal_matrix"));
    let lift = graph.add(
        Node::new("drive.toward_eye").with_param("distance", Value::F32(crate::neon::DEPTH_BIAS)),
    );
    graph.set_stage(lift, StageConstraint::Vertex);
    wire(&mut graph, (matrix, "out"), (lift, "normal_matrix"))?;
    let view = graph.add_node(abi::context_node_id("view_direction"));
    wire(&mut graph, (view, "out"), (lift, "view"))?;
    let vertex = graph.add_node(abi::VERTEX_OUTPUT_ID);
    wire(
        &mut graph,
        (lift, "out"),
        (vertex, abi::SOCKET_POSITION_OFFSET),
    )?;
    for (socket, axis) in [
        ("right", [1.0, 0.0, 0.0]),
        ("up", [0.0, 1.0, 0.0]),
        ("back", [0.0, 0.0, 1.0]),
    ] {
        let turn = graph.add(Node::new("math.multiply"));
        for (generic, ty) in [("A", ValueType::Mat3), ("B", ValueType::Vec3)] {
            graph
                .set_generic(registry, turn, generic, ty)
                .map_err(|error| format!("drive car ground: {error}"))?;
        }
        graph.set_param(turn, "b", Value::Vec3(axis));
        graph.set_stage(turn, StageConstraint::Vertex);
        wire(&mut graph, (matrix, "out"), (turn, "a"))?;
        wire(&mut graph, (turn, "out"), (node, socket))?;
    }
    let uv = graph.add_node(abi::context_node_id("uv"));
    wire(&mut graph, (uv, "out"), (node, "uv"))?;
    let strength = graph.add(
        Node::new("param.value")
            .with_setting("name", STRENGTH)
            .with_param("value", Value::F32(1.0)),
    );
    wire(&mut graph, (strength, "out"), (node, STRENGTH))?;
    wire(
        &mut graph,
        (node, "out"),
        (output, abi::SOCKET_FRAGMENT_COLOR),
    )?;
    Ok(graph)
}
