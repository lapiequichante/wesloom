//! `wxsl-render`: the wgpu renderer.
//!
//! Owns everything that talks to `wgpu`: device setup, the render graph that
//! turns a list of passes into a frame, and the shader variant cache that
//! lets a single node graph back both a forward and a deferred pipeline
//! without the caller doing anything special to "switch".
//!
//! See `docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md`,
//! `docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md` and
//! `docs/adr/0022-material-stages-replace-the-render-path-enum.md`.
//!
//! # How a graph becomes a frame
//!
//! ```text
//!   Graph ──codegen──> Material ──variants──> ShaderVariant ─┐
//!   (core)             (one module            (WGSL + wgpu   │
//!                       per stage)             module)       │
//!                                                            v
//!   PassDesc… ──schedule──> Schedule ──record──>  pipelines ──> frame
//!   (a pipeline, as data)   (order + textures)
//! ```
//!
//! [`material::Material`] is authored once and compiled per *stage*: it
//! holds one generated WXSL module per [`wxsl_core::abi::MaterialStage`],
//! each with the entry point that stage calls for — a final colour, a
//! G-buffer, or no fragment stage at all.
//! [`variants::ShaderVariants`] compiles those per (macro set, stage) and
//! caches the result.
//!
//! A *pipeline* is not a Rust struct but a [`graph::RenderGraph`] — a list
//! of [`pass::PassDesc`]s, each naming the stage it draws with — which
//! [`graph::Schedule`] orders and allocates and [`renderer::Renderer`]
//! runs. Switching pipeline at runtime is
//! [`renderer::Renderer::set_pipeline`], or
//! [`renderer::Renderer::request_pipeline`] to swap without a stutter;
//! supplying your own pass list is [`renderer::Renderer::set_graph`].
//!
//! # What is a scene, and what is not
//!
//! A *scene* — meshes, instances, materials, tags — is pure data and lives
//! in `wxsl_core::scene`. What this crate takes is a [`draw::DrawList`],
//! because batching, culling and sorting belong to the application.
//! [`environment::Environment`] is the other half of a frame: camera,
//! lights, ambient.
//!
//! # This crate ships no shaders
//!
//! It compiles WXSL but contains none: the shader ABI and the node library
//! are `wxsl-stdlib`'s, and the dependency arrow only points *into*
//! `wxsl-core` ([ADR 0002](../../docs/adr/0002-cargo-workspace-crate-boundaries.md)).
//! The application supplies the modules through
//! [`library::ShaderLibrary`], which is also how it can override an ABI
//! module or add hand-written WXSL of its own.

#![warn(missing_docs)]

pub mod bindings;
pub mod draw;
pub mod environment;
pub mod error;
#[cfg(feature = "gltf")]
pub mod gltf;
pub mod gpu;
pub mod graph;
pub mod library;
pub mod material;
pub mod mesh;
pub mod pass;
pub mod pipeline;
pub mod renderer;
pub mod swap;
pub mod ui;
pub mod variants;

// Re-exported so a dependant can name a `wgpu::Device` or a `glam::Mat4`
// without having to pin the same versions itself — and so that "which wgpu
// does the renderer use" has one answer rather than a lockfile search.
pub use glam;
pub use wgpu;

pub use bindings::{BindingLayouts, MaterialBindings};
pub use draw::{DrawItem, DrawList, InstanceAttributes};
pub use environment::{
    Camera, Environment, FrameBindings, InstanceRowSet, InstanceRows, InstanceTransform, Light,
};
pub use error::RenderError;
pub use gpu::{DeviceCaps, GpuContext, OffscreenTarget};
pub use graph::{PassBinding, RenderGraph, ResourcePool, Schedule};
pub use library::ShaderLibrary;
pub use material::Material;
pub use mesh::{AttributeValues, Mesh, MeshData, MeshKind, Vertex};
pub use pass::{
    Attachment, DepthAttachment, DrawSource, PassDesc, PassKind, PassState, Read, ResourceDesc,
    ResourceId,
};
pub use pipeline::{PipelineCache, StockPipeline, TargetConfig};
pub use renderer::{single_draw, RenderRequest, Renderer};
pub use swap::SwapProgress;
pub use variants::{ShaderVariant, ShaderVariants};
