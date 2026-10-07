//! Shared pipeline_doc (ADR 0048).

pub use wxsl_frame::pipeline_doc::*;

use crate::effect::EffectRegistry;
use crate::graph::RenderGraph;
use crate::pipeline::PipelineConfig;
use wxsl_core::{graph::Graph, node::NodeRegistry};

/// Compile a shared document into a wgpu-recordable graph.
pub fn compile(
    document: &Graph,
    registry: &NodeRegistry,
    effects: &EffectRegistry,
    config: &PipelineConfig,
) -> Result<RenderGraph, PipelineError> {
    wxsl_frame::pipeline_doc::compile(document, registry, effects, config).map(Into::into)
}
