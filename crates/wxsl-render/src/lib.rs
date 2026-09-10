//! `wxsl-render`: the wgpu renderer.
//!
//! Owns everything that talks to `wgpu`: device setup, the render graph that
//! turns a list of passes into a frame, and the shader variant cache that
//! lets a single node graph back both a forward and a deferred pipeline
//! without the caller doing anything special to "switch".
//!
//! See `docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md`
//! and `docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md`.
//!
//! # How a graph becomes a frame
//!
//! ```text
//!   Graph ──codegen──> Material ──variants──> ShaderVariant ─┐
//!   (core)             (one WXSL module,      (WGSL + wgpu   │
//!                       both paths in it)      module)       │
//!                                                            v
//!   PassDesc… ──schedule──> Schedule ──record──>  pipelines ──> frame
//!   (a pipeline, as data)   (order + textures)
//! ```
//!
//! [`material::Material`] is path-agnostic: it holds one WXSL module whose
//! two fragment entry points are gated by conditional translation.
//! [`variants::ShaderVariants`] compiles that module per
//! (macro set, [`path::RenderPath`]) and caches the result. A *pipeline* is
//! no longer a Rust struct but a [`graph::RenderGraph`] — a list of
//! [`pass::PassDesc`]s — which [`graph::Schedule`] orders and allocates and
//! [`renderer::Renderer`] runs. Switching pipeline at runtime is
//! [`renderer::Renderer::set_path`]; supplying your own pass list is
//! [`renderer::Renderer::set_graph`].
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
pub mod path;
pub mod pipeline;
pub mod renderer;
pub mod ui;
pub mod variants;

// Re-exported so a dependant can name a `wgpu::Device` or a `glam::Mat4`
// without having to pin the same versions itself — and so that "which wgpu
// does the renderer use" has one answer rather than a lockfile search.
pub use glam;
pub use wgpu;

pub use draw::{DrawItem, DrawList};
pub use environment::{Camera, Environment, FrameBindings, InstanceTransform, Light};
pub use error::RenderError;
pub use gpu::{DeviceCaps, GpuContext, OffscreenTarget};
pub use graph::{PassBinding, RenderGraph, ResourcePool, Schedule};
pub use library::ShaderLibrary;
pub use material::Material;
pub use mesh::{Mesh, MeshData, MeshKind, Vertex};
pub use pass::{
    Attachment, DepthAttachment, DrawSource, PassDesc, PassKind, PassState, Read, ResourceDesc,
    ResourceId,
};
pub use path::RenderPath;
pub use pipeline::{PipelineCache, TargetConfig};
pub use renderer::{single_draw, RenderRequest, Renderer};
pub use variants::{ShaderVariant, ShaderVariants};
