//! Device-free frame descriptions and planning (ADR 0048).
//!
//! Owns neutral pass/resource descriptions, graph scheduling, pipeline
//! document compilation, presets, effects, capability checks and frame
//! environment data. GPU allocation and recording belong to the backend.

#![warn(missing_docs)]

pub mod effect;
pub mod environment;
pub mod graph;
pub mod pass;
pub mod pipeline;
pub mod pipeline_doc;
pub mod setup;
pub mod types;

/// Generated fixed host-shared C/C++ layouts (ADR 0050).
pub const HOST_HEADER: &str = include_str!(concat!(env!("OUT_DIR"), "/wxsl_host.h"));
