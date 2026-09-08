//! `wxsl-editor`: the visual node editor.
//!
//! This crate exists so that consumers who only want the headless graph
//! model and/or the renderer (`wxsl-core`, `wxsl-render`) never build
//! or link a GUI toolkit. Nothing in `wxsl-core` or `wxsl-render` may
//! depend on this crate, or on the `editor` feature of the `wxsl` facade
//! crate — the dependency arrow only ever points this way.
//!
//! See `docs/adr/0004-node-editor-is-an-optional-additive-ui-layer.md`.
//!
//! # Status: scaffolding
//!
//! There is no UI here yet — these are module stubs. The graph model it will
//! drive *is* implemented, though, and it was built with an editor in mind;
//! the entry points a UI needs are:
//!
//! | Need | API |
//! |---|---|
//! | Populate a node palette, grouped | [`wxsl_core::node::NodeRegistry::categories`] and `iter` |
//! | Draw a node's ports, with types and docs | [`wxsl_core::node::NodeDefinition`]'s `inputs`/`outputs` |
//! | Inline widgets for unconnected inputs | [`wxsl_core::node::Socket::default`] and the node's `params` |
//! | Reject a bad link while dragging it | [`wxsl_core::graph::Graph::connect`] — type-checks and refuses cycles, changing nothing on error |
//! | Underline every problem at once | [`wxsl_core::graph::Graph::validate`] returns all errors, not the first |
//! | Canvas positions that survive a save | [`wxsl_core::graph::Node::position`] |
//! | A panel of the graph's macro variables | [`wxsl_core::graph::Graph::declared_macros`] and `set_macro` |
//! | Live preview | compile with `wxsl-render` at the application level; this crate stays wgpu-free (ADR 0004) |

pub mod canvas {
    //! The pan/zoom node canvas widget and node/link drawing.
    //!
    //! Placeholder. A canvas needs, from `wxsl_core::graph`: the node
    //! positions to lay out, [`wxsl_core::graph::Graph::edges`] to draw
    //! links, and `connect`/`disconnect` to edit them.
}

pub mod widgets {
    //! Per-socket-type inline editing widgets (color pickers, sliders, …).
    //!
    //! Placeholder. The set of widgets needed is exactly
    //! [`wxsl_core::node::ValueType`]'s variants, and what a widget edits
    //! is a [`wxsl_core::node::Value`] in the node's `params`.
}
