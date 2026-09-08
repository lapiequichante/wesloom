//! [`Material`]: a graph compiled to WESL, ready to be asked for a variant.
//!
//! A material is path-agnostic. It holds one WESL module with both fragment
//! entry points in it, and the macro values it was generated with; which
//! entry point survives is decided later, when a pipeline asks
//! [`crate::variants::ShaderVariants`] for a variant on its own render path.
//!
//! Macro variables can be overridden per material without touching the graph
//! ([`Material::from_graph_with_macros`]), which is what a "toggle this at
//! runtime" UI wants: the graph keeps the values it was authored with, and
//! the override lives with the material instance.

use wesloom_core::codegen::{self, CodegenOptions, GeneratedShader};
use wesloom_core::graph::Graph;
use wesloom_core::macros::MacroSet;
use wesloom_core::node::NodeRegistry;

use crate::error::RenderError;

/// A graph compiled to WESL.
#[derive(Clone, Debug)]
pub struct Material {
    /// Name, taken from the graph. Used in shader labels.
    pub name: String,
    /// The generated module and the macro values behind it.
    pub shader: GeneratedShader,
}

impl Material {
    /// Compile `graph` with the macro values the graph and the ABI ask for.
    pub fn from_graph(graph: &Graph, registry: &NodeRegistry) -> Result<Self, RenderError> {
        Self::from_graph_with_macros(graph, registry, &MacroSet::new())
    }

    /// Compile `graph`, with `overrides` taking precedence over the macro
    /// values the graph pins.
    ///
    /// Use this for macros the user is flipping at runtime. The precedence
    /// order, weakest first, is: the ABI's defaults, each node's declared
    /// default, what the graph pins, then `overrides`.
    pub fn from_graph_with_macros(
        graph: &Graph,
        registry: &NodeRegistry,
        overrides: &MacroSet,
    ) -> Result<Self, RenderError> {
        let options = CodegenOptions {
            override_macros: overrides.clone(),
            ..CodegenOptions::default()
        };
        Ok(Material {
            name: graph.name().to_string(),
            shader: codegen::generate(graph, registry, &options)?,
        })
    }

    /// The macro values this material was compiled with.
    pub fn macros(&self) -> &MacroSet {
        &self.shader.macros
    }

    /// The generated WESL source, for `--dump-wesl`-style tooling.
    pub fn wesl(&self) -> &str {
        &self.shader.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wesloom_core::abi;
    use wesloom_core::macros::MacroValue;
    use wesloom_core::node::{NodeDefinition, Socket, ValueType};

    fn registry() -> NodeRegistry {
        let mut registry = NodeRegistry::new();
        registry.register(abi::surface_output_def());
        registry.register(
            NodeDefinition::builder("test.macro_reader", "Macro reader")
                .macro_var(wesloom_core::macros::MacroDef::new(
                    "WESLOOM_TEST_LEVEL",
                    MacroValue::Int(2),
                    "test",
                ))
                .output(Socket::new("out", ValueType::F32))
                .expr("f32(WESLOOM_TEST_LEVEL)"),
        );
        registry
    }

    #[test]
    fn abi_macro_defaults_reach_the_material() {
        let registry = registry();
        let mut graph = Graph::new("defaults");
        graph.add_node(abi::SURFACE_OUTPUT_ID);
        let material = Material::from_graph(&graph, &registry).unwrap();
        assert_eq!(
            material.macros().get(abi::FEATURE_TONEMAP),
            Some(MacroValue::Flag(true))
        );
    }

    #[test]
    fn overrides_beat_the_graph_and_change_the_source() {
        let registry = registry();
        let mut graph = Graph::new("overrides");
        let reader = graph.add_node("test.macro_reader");
        let output = graph.add_node(abi::SURFACE_OUTPUT_ID);
        graph
            .wire(&registry, (reader, "out"), (output, "roughness"))
            .unwrap();
        graph.set_macro("WESLOOM_TEST_LEVEL", MacroValue::Int(3));
        graph.set_macro(abi::FEATURE_TONEMAP, MacroValue::Flag(false));

        let plain = Material::from_graph(&graph, &registry).unwrap();
        assert_eq!(
            plain.macros().get("WESLOOM_TEST_LEVEL"),
            Some(MacroValue::Int(3))
        );
        assert_eq!(
            plain.macros().get(abi::FEATURE_TONEMAP),
            Some(MacroValue::Flag(false))
        );

        let mut overrides = MacroSet::new();
        overrides.set("WESLOOM_TEST_LEVEL", MacroValue::Int(9));
        overrides.set(abi::FEATURE_TONEMAP, MacroValue::Flag(true));
        let overridden = Material::from_graph_with_macros(&graph, &registry, &overrides).unwrap();
        assert_eq!(
            overridden.macros().get("WESLOOM_TEST_LEVEL"),
            Some(MacroValue::Int(9))
        );
        assert_eq!(
            overridden.macros().get(abi::FEATURE_TONEMAP),
            Some(MacroValue::Flag(true))
        );
        // Different macro values must not collide in the variant cache.
        assert_ne!(plain.shader.variant_key(), overridden.shader.variant_key());
    }
}
