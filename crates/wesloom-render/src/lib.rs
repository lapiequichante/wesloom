//! `wesloom-render`: the wgpu renderer.
//!
//! Owns everything that talks to `wgpu`: device/surface setup, the pipeline
//! abstraction, and the shader variant cache that lets a single node graph
//! back both a forward and a deferred pipeline without the caller doing
//! anything special to "switch".
//!
//! See `docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md`.
//!
//! This crate is currently scaffolding; see the module docs below for what
//! each placeholder is expected to hold.

pub mod path {
    //! [`RenderPath`], the enum a pipeline is built for (forward, deferred,
    //! future paths). Selecting a `RenderPath` is what drives which shader
    //! variant gets requested from a compiled node graph.
    //!
    //! Placeholder: see `docs/adr/0005-render-pipeline-abstraction-and-shader-switching.md`.
}

pub mod pipeline {
    //! The pipeline trait(s) shared by forward and deferred implementations,
    //! and the concrete forward/deferred pipelines themselves.
    //!
    //! Placeholder.
}

pub mod variants {
    //! The shader variant cache: keyed by (graph hash, [`path::RenderPath`],
    //! active feature set) so a graph is only ever recompiled to WGSL once
    //! per variant, and switching pipelines at runtime reuses cached WGSL.
    //!
    //! Placeholder.
}
