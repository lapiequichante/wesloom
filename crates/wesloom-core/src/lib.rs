//! `wesloom-core`: the shader node graph model.
//!
//! This crate owns the data model shared by every other crate in the
//! workspace: node/socket definitions, the graph itself, and the code that
//! turns a graph into composable WESL source. It intentionally depends on
//! neither `wgpu` (runtime rendering, see `wesloom-render`) nor any GUI
//! toolkit (visual editing, see `wesloom-editor`), so that anything built on
//! top of `wesloom-core` alone stays headless and dependency-light.
//!
//! See `docs/architecture.md` and `docs/adr/0002-cargo-workspace-crate-boundaries.md`
//! for the reasoning behind this split, and `docs/adr/0003-wesl-as-the-shading-language.md`
//! for why graphs compile to WESL rather than straight to WGSL.
//!
//! # The shape of a material graph
//!
//! ```text
//!  input.* nodes            math / color / lighting nodes           output.surface
//!  (read the per-fragment   (arithmetic as inline WESL, anything    (the Surface
//!   SurfaceContext)          real as an imported WESL function)      struct)
//! ```
//!
//! A graph describes a *surface*, not a whole shader: [`codegen`] wraps it in
//! the entry points and imports of the shader ABI ([`abi`]), emitting one WESL
//! module whose forward and deferred fragment entries are selected by
//! conditional translation. The same graph therefore drives either render
//! path with no path-specific authoring
//! ([ADR 0005](../../docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md)).
//!
//! # Example
//!
//! Build a two-node graph and compile it to WESL. `wesloom-stdlib` supplies
//! the registry in a real program; here it is spelled out to show what a node
//! definition is.
//!
//! ```
//! use wesloom_core::abi;
//! use wesloom_core::graph::{Graph, Node};
//! use wesloom_core::node::{NodeDefinition, NodeRegistry, Socket, Value, ValueType};
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
//! let shader = wesloom_core::codegen::generate(&graph, &registry, &Default::default())?;
//! assert!(shader.source.contains("surface.roughness = n2_out;"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod abi;
pub mod codegen;
pub mod error;
pub mod graph;
pub mod macros;
pub mod node;
pub mod wesl;

pub use error::{CodegenError, GraphError, GraphErrors};
pub use graph::{Edge, Graph, Node, NodeId, SocketRef};
pub use macros::{MacroDef, MacroKind, MacroSet, MacroValue};
pub use node::{NodeDefinition, NodeRegistry, Socket, Value, ValueType, WeslFunction};
