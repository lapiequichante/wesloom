//! Device-free frame descriptions and planning (ADR 0048).
//!
//! Owns neutral pass/resource descriptions, graph scheduling, pipeline
//! document compilation, presets, effects, capability checks and frame
//! environment data. GPU allocation and recording belong to the backend.
//!
//! # Stability
//!
//! This crate is unstable as a whole ([ADR 0078](../../../docs/adr/0078-three-api-tiers-pinned-by-public-api-snapshots.md)). Rust hosts reach its
//! stable names through `wxsl-render`'s root re-exports; C and C++ hosts reach
//! it through `wxsl-ffi`'s C ABI.

#![warn(missing_docs)]

pub mod effect;
pub mod environment;
pub mod graph;
pub mod ies;
pub mod light_grid;
pub mod pass;
pub mod pipeline;
pub mod pipeline_doc;
pub mod profile;
pub mod setup;
pub mod shadow_cache;
pub mod types;

/// Generated fixed host-shared C/C++ layouts (ADR 0050).
pub const HOST_HEADER: &str = include_str!(concat!(env!("OUT_DIR"), "/wxsl_host.h"));
