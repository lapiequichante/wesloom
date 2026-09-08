//! `wesloom-editor`: the visual node editor.
//!
//! This crate exists so that consumers who only want the headless graph
//! model and/or the renderer (`wesloom-core`, `wesloom-render`) never build
//! or link a GUI toolkit. Nothing in `wesloom-core` or `wesloom-render` may
//! depend on this crate, or on the `editor` feature of the `wesloom` facade
//! crate — the dependency arrow only ever points this way.
//!
//! See `docs/adr/0004-node-editor-is-an-optional-additive-ui-layer.md`.
//!
//! This crate is currently scaffolding.

pub mod canvas {
    //! The pan/zoom node canvas widget and node/link drawing.
    //!
    //! Placeholder.
}

pub mod widgets {
    //! Per-socket-type inline editing widgets (color pickers, sliders, …).
    //!
    //! Placeholder.
}
