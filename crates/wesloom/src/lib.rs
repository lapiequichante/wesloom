//! `wesloom`: WESL shader node graphs on wgpu, with an optional visual node
//! editor.
//!
//! This is the facade most consumers should depend on directly; it
//! re-exports the workspace's other crates behind feature flags so a given
//! build only pays for what it uses:
//!
//! | Feature | Default | Pulls in | Use when |
//! |---|---|---|---|
//! | `render` | yes | [`wesloom_render`] | you need wgpu pipelines (forward/deferred) driven by a graph |
//! | `stdlib` | yes | [`wesloom_stdlib`] | you want the built-in library of base nodes (math, color, lighting, SDFs, …) |
//! | `editor` | no | [`wesloom_editor`] (implies `render`) | you're building a UI that lets users edit graphs visually |
//!
//! [`wesloom_core`] (the graph model) is always available; it has no wgpu
//! or GUI dependency regardless of which features are enabled. See
//! `docs/architecture.md` for the full crate graph and
//! `docs/adr/0002-cargo-workspace-crate-boundaries.md` for why the split
//! exists.
//!
//! This crate is currently scaffolding: it just re-exports its
//! (also-scaffolding) dependencies.

pub use wesloom_core as core;

#[cfg(feature = "render")]
pub use wesloom_render as render;

#[cfg(feature = "editor")]
pub use wesloom_editor as editor;

#[cfg(feature = "stdlib")]
pub use wesloom_stdlib as stdlib;
