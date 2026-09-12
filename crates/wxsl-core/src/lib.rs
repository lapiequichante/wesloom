//! `wxsl-core`: the shader node graph model.
//!
//! This crate owns the data model shared by every other crate in the
//! workspace: node/socket definitions, the graph itself, and the code that
//! turns a graph into composable WXSL source. It intentionally depends on
//! neither `wgpu` (runtime rendering, see `wxsl-render`) nor any GUI
//! toolkit (visual editing, see `wxsl-editor`), so that anything built on
//! top of `wxsl-core` alone stays headless and dependency-light.
//!
//! See `docs/architecture.md` and `docs/adr/0002-cargo-workspace-crate-boundaries.md`
//! for the reasoning behind this split, and `docs/adr/0003-wesl-as-the-shading-language.md`
//! for why graphs compile to WXSL rather than straight to WGSL.
//!
//! # The shape of a material graph
//!
//! ```text
//!  input.* nodes            math / color / lighting nodes           output.surface
//!  (read the per-fragment   (arithmetic as inline WXSL, anything    (the Surface
//!   SurfaceContext)          real as an imported WXSL function)      struct)
//! ```
//!
//! A graph describes a *surface*, not a whole shader: [`codegen`] wraps it in
//! the entry points and imports of the shader ABI ([`abi`]), emitting one
//! WXSL module per [`abi::MaterialStage`]. The same graph therefore drives
//! every pipeline with no pipeline-specific authoring
//! ([ADR 0005](../../docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md),
//! [ADR 0022](../../docs/adr/0022-material-stages-replace-the-render-path-enum.md)).
//!
//! A graph also *declares what it needs from outside itself* — uniform
//! parameters, textures, samplers, and a block the application supplies —
//! and [`resources::MaterialInterface`] is what codegen emits and the
//! renderer binds
//! ([ADR 0023](../../docs/adr/0023-a-material-declares-its-resources.md)).
//!
//! # Example
//!
//! Build a two-node graph and compile it to WXSL. `wxsl-stdlib` supplies
//! the registry in a real program; here it is spelled out to show what a node
//! definition is.
//!
//! ```
//! use wxsl_core::abi;
//! use wxsl_core::graph::{Graph, Node};
//! use wxsl_core::node::{NodeDefinition, NodeRegistry, Socket, Value, ValueType};
//!
//! let mut registry = NodeRegistry::new();
//! registry.register(abi::surface_output_def());
//! registry.register_all(abi::context_node_defs());
//! registry.register(
//!     NodeDefinition::builder("math.multiply.f32", "Multiply")
//!         .input(Socket::new("a", ValueType::F32).with_splat_default(1.0))
//!         .input(Socket::new("b", ValueType::F32).with_splat_default(1.0))
//!         .output(Socket::new("out", ValueType::F32))
//!         .expr("{a} * {b}"),
//! );
//!
//! let mut graph = Graph::new("pulsing roughness");
//! let time = graph.add_node("input.time");
//! let scale = graph.add(Node::new("math.multiply.f32").with_param("b", Value::F32(0.25)));
//! let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
//! graph.wire(&registry, (time, "out"), (scale, "a"))?;
//! graph.wire(&registry, (scale, "out"), (output, "roughness"))?;
//!
//! let shader = wxsl_core::codegen::generate(&graph, &registry, &Default::default())?;
//! assert!(shader.source.contains("surface.roughness = n2_out;"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod abi;
pub mod codegen;
pub mod error;
pub mod graph;
pub mod lighting;
pub mod macros;
pub mod node;
pub mod resources;
pub mod scene;
pub mod wxsl;

pub use error::{CodegenError, GraphError, GraphErrors};
pub use graph::{Edge, Graph, Node, NodeId, SocketRef};
pub use macros::{MacroDef, MacroKind, MacroSet, MacroValue};
pub use node::{NodeDefinition, NodeRegistry, Socket, Value, ValueType, WxslFunction};
pub use resources::{BufferLayout, FieldLayout, MaterialInterface, ResourceBinding, UserBlock};
pub use scene::{Instance, MaterialEntry, MeshEntry, MeshSource, Scene, TagExpr, Tags};
