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
//! This crate is currently scaffolding: the modules below are placeholders
//! for the real data model, added incrementally per their referenced ADRs.

#![forbid(unsafe_code)]

pub mod node {
    //! Node and socket definitions: types, ports, and the trait a node kind
    //! implements to describe its interface and emit WESL.
    //!
    //! Placeholder: node/socket trait definitions land here.
    //! See `docs/adr/0003-wesl-as-the-shading-language.md`.
}

pub mod graph {
    //! The graph itself: nodes, connections, validation, and traversal.
    //!
    //! Placeholder: the node graph data structure lands here.
}

pub mod codegen {
    //! Turns a validated graph into WESL source ready for `wesl`/`wesl-cli`
    //! to resolve and compile down to WGSL.
    //!
    //! Placeholder: graph -> WESL source generation lands here.
    //! See `docs/adr/0003-wesl-as-the-shading-language.md`.
}
