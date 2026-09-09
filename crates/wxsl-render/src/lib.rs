//! `wxsl-render`: the wgpu renderer.
//!
//! Owns everything that talks to `wgpu`: device setup, the pipeline
//! abstraction, and the shader variant cache that lets a single node graph
//! back both a forward and a deferred pipeline without the caller doing
//! anything special to "switch".
//!
//! See `docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md`.
//!
//! # How a graph becomes a frame
//!
//! ```text
//!   Graph ──codegen──> Material ──variants──> ShaderVariant ──> Pipeline ──> frame
//!   (core)             (one WXSL module,      (WGSL + wgpu       (forward or
//!                       both paths in it)      module, per path)  deferred)
//! ```
//!
//! [`material::Material`] is path-agnostic: it holds one WXSL module whose
//! two fragment entry points are gated by conditional translation.
//! [`variants::ShaderVariants`] compiles that module per
//! (macro set, [`path::RenderPath`]) and caches the result, and
//! [`renderer::Renderer`] picks the pipeline to feed it to. Switching path at
//! runtime is [`renderer::Renderer::set_path`] — no recompilation the second
//! time, no second graph, ever.
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

pub mod error;
pub mod gpu;
pub mod library;
pub mod material;
pub mod mesh;
pub mod path;
pub mod pipeline;
pub mod renderer;
pub mod scene;
pub mod ui;
pub mod variants;

pub use error::RenderError;
pub use gpu::{GpuContext, OffscreenTarget};
pub use library::ShaderLibrary;
pub use material::Material;
pub use mesh::{Mesh, MeshKind, Vertex};
pub use path::RenderPath;
pub use pipeline::{Pipeline, TargetConfig};
pub use renderer::{RenderRequest, Renderer};
pub use scene::{Camera, Light, Scene, SceneBindings};
pub use variants::{ShaderVariant, ShaderVariants};
