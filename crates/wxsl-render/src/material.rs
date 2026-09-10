//! [`Material`]: a graph compiled to WXSL, once per stage.
//!
//! A material is stage-agnostic in the only sense that matters: it is
//! *authored* once. What it holds is one generated module per
//! [`MaterialStage`] — a forward-lit one, a G-buffer one, a depth-only one
//! — because a stage differs in which entry point it emits and, from M5,
//! in which part of the graph it needs
//! ([ADR 0022](../../../docs/adr/0022-material-stages-replace-the-render-path-enum.md)).
//!
//! Generating every stage up front is cheap: codegen is string building,
//! and the expensive half — WXSL to WGSL to a `wgpu` module — stays lazy in
//! [`crate::variants::ShaderVariants`], which compiles a stage the first
//! time a pass asks for it and never again.
//!
//! Macro variables can be overridden per material without touching the
//! graph ([`Material::from_graph_with_macros`]), which is what a "toggle
//! this at runtime" UI wants: the graph keeps the values it was authored
//! with, and the override lives with the material instance.

use wxsl_core::abi::MaterialStage;
use wxsl_core::codegen::{self, CodegenOptions, GeneratedShader};
use wxsl_core::graph::Graph;
use wxsl_core::macros::MacroSet;
use wxsl_core::node::NodeRegistry;
use wxsl_core::resources::{BufferLayout, FieldLayout, MaterialInterface, VertexAttributeBinding};

use crate::error::RenderError;

/// A graph compiled to WXSL, one module per stage.
#[derive(Clone, Debug)]
pub struct Material {
    /// Name, taken from the graph. Used in shader labels.
    pub name: String,
    /// One generated module per stage, indexed by
    /// [`MaterialStage::index`].
    stages: Vec<GeneratedShader>,
    /// [`MaterialInterface::signature`], built once.
    ///
    /// Every draw needs it — it is what the bind-group-layout and
    /// pipeline caches are keyed on — and rebuilding a string per draw
    /// per frame is an allocation for something that cannot change while
    /// the material exists.
    signature: String,
    /// `interface().geometry.instance().signature()`, built once.
    ///
    /// Consulted per draw per pass — it is what the frame's instance
    /// buffers are grouped by — and, like [`Material::signature`], it
    /// cannot change while the material exists.
    instance_signature: String,
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
        let mut stages = Vec::with_capacity(MaterialStage::ALL.len());
        for stage in MaterialStage::ALL {
            let options = CodegenOptions {
                stage: *stage,
                override_macros: overrides.clone(),
                ..CodegenOptions::default()
            };
            stages.push(codegen::generate(graph, registry, &options)?);
        }
        Ok(Material {
            name: graph.name().to_string(),
            signature: stages[0].interface.signature(),
            instance_signature: stages[0].interface.geometry.instance().signature(),
            stages,
        })
    }

    /// The generated module for `stage`.
    pub fn shader(&self, stage: MaterialStage) -> &GeneratedShader {
        // Indexed, not looked up: `stages` is built from
        // `MaterialStage::ALL` and a stage is an index into the same table,
        // so the two cannot drift apart.
        &self.stages[stage.index()]
    }

    /// What must be bound before this material can draw: its uniform
    /// parameters, its textures and samplers, and the block it expects
    /// the application to supply.
    ///
    /// The same for every stage, and a test says so — a stage changes
    /// which entry point is emitted, never what the graph declares. That
    /// stops being true when M5 partitions the graph per stage, at which
    /// point this becomes per-stage and the pipeline layouts follow it.
    pub fn interface(&self) -> &MaterialInterface {
        &self.stages[0].interface
    }

    /// Which instance row shape this material reads, as
    /// [`BufferLayout::signature`].
    pub fn instance_signature(&self) -> &str {
        &self.instance_signature
    }

    /// The layout of one row of the instance buffer this material reads:
    /// the ABI's transform prefix, then whatever per-instance attributes
    /// the graph declared.
    ///
    /// Never empty — every instance has a transform — so this is also the
    /// stride, and the key the frame's instance buffers are grouped by.
    pub fn instance_layout(&self) -> &BufferLayout {
        self.interface().geometry.instance()
    }

    /// Just the declared part of that row.
    pub fn instance_attributes(&self) -> &[FieldLayout] {
        self.interface().geometry.instance_attributes()
    }

    /// The per-vertex streams the mesh this is drawn on must carry.
    pub fn vertex_attributes(&self) -> &[VertexAttributeBinding] {
        self.interface().geometry.vertex()
    }

    /// The shape of what this material declares, as the bind-group and
    /// pipeline caches key on it. A *shape*, never a value: two materials
    /// with the same parameters at different settings share it, which is
    /// what makes changing a parameter free.
    pub fn signature(&self) -> &str {
        &self.signature
    }

    /// The macro values this material was compiled with.
    ///
    /// The same for every stage — a stage changes the entry point, never
    /// the knobs.
    pub fn macros(&self) -> &MacroSet {
        &self.stages[0].macros
    }

    /// The generated WXSL source for `stage`, for `--dump-wxsl`-style
    /// tooling.
    pub fn wxsl(&self, stage: MaterialStage) -> &str {
        &self.shader(stage).source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxsl_core::abi;
    use wxsl_core::macros::MacroValue;
    use wxsl_core::node::{NodeDefinition, Socket, ValueType};

    fn registry() -> NodeRegistry {
        let mut registry = NodeRegistry::new();
        registry.register(abi::surface_output_def());
        registry.register(
            NodeDefinition::builder("test.macro_reader", "Macro reader")
                .macro_var(wxsl_core::macros::MacroDef::new(
                    "WXSL_TEST_LEVEL",
                    MacroValue::Int(2),
                    "test",
                ))
                .output(Socket::new("out", ValueType::F32))
                .expr("f32(WXSL_TEST_LEVEL)"),
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
    fn every_stage_gets_its_own_module_with_its_own_identity() {
        let registry = registry();
        let mut graph = Graph::new("stages");
        graph.add_node(abi::SURFACE_OUTPUT_ID);
        let material = Material::from_graph(&graph, &registry).unwrap();

        let mut keys: Vec<u64> = MaterialStage::ALL
            .iter()
            .map(|stage| material.shader(*stage).variant_key())
            .collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(
            keys.len(),
            count,
            "two stages share a variant key, so one would be served the other's shader"
        );
        assert!(material
            .wxsl(MaterialStage::GBUFFER)
            .contains("fn fs_gbuffer"));
        assert!(!material
            .wxsl(MaterialStage::DEPTH_ONLY)
            .contains("@fragment"));
    }

    #[test]
    fn every_stage_declares_the_same_interface() {
        // A stage picks an entry point, not a set of bindings, so all
        // three modules of one material are bound the same way — which is
        // what lets one `MaterialBindings` serve a depth prepass and the
        // shading pass after it. M5 is where this stops holding.
        let registry = registry();
        let mut graph = Graph::new("interface");
        graph.add_node(abi::SURFACE_OUTPUT_ID);
        let material = Material::from_graph(&graph, &registry).unwrap();
        for stage in MaterialStage::ALL {
            assert_eq!(
                material.shader(*stage).interface,
                *material.interface(),
                "{stage} declares a different interface"
            );
        }
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
        graph.set_macro("WXSL_TEST_LEVEL", MacroValue::Int(3));
        graph.set_macro(abi::FEATURE_TONEMAP, MacroValue::Flag(false));

        let plain = Material::from_graph(&graph, &registry).unwrap();
        assert_eq!(
            plain.macros().get("WXSL_TEST_LEVEL"),
            Some(MacroValue::Int(3))
        );
        assert_eq!(
            plain.macros().get(abi::FEATURE_TONEMAP),
            Some(MacroValue::Flag(false))
        );

        let mut overrides = MacroSet::new();
        overrides.set("WXSL_TEST_LEVEL", MacroValue::Int(9));
        overrides.set(abi::FEATURE_TONEMAP, MacroValue::Flag(true));
        let overridden = Material::from_graph_with_macros(&graph, &registry, &overrides).unwrap();
        assert_eq!(
            overridden.macros().get("WXSL_TEST_LEVEL"),
            Some(MacroValue::Int(9))
        );
        assert_eq!(
            overridden.macros().get(abi::FEATURE_TONEMAP),
            Some(MacroValue::Flag(true))
        );
        // Different macro values must not collide in the variant cache, on
        // any stage.
        for stage in MaterialStage::ALL {
            assert_ne!(
                plain.shader(*stage).variant_key(),
                overridden.shader(*stage).variant_key()
            );
        }
    }
}
