//! Stage analysis: which shader stage every node runs in, and where the
//! two stages hand values over.
//!
//! Before this analysis existed, a node's stage was decided by *wiring*:
//! the vertex stage was reachable only through the vertex and interpolant
//! terminals, a node feeding two terminals was simply compiled twice, and
//! every cross-stage value was a hand-declared interpolant. That was a
//! deliberate first answer ([ADR 0025]) — varyings are scarce, and the
//! compiler should not spend one without the author asking. What it made
//! impossible was the thing shader authors actually want: write the graph
//! once, and let the *compiler* place it — computing what it can per
//! vertex, and paying an interpolant only when a value is genuinely
//! shared across the stage boundary.
//!
//! This module is that compiler. For every node reachable from a
//! terminal it answers:
//!
//! * **which stage it runs in** — an explicit [`StageConstraint`] wins;
//!   otherwise the earliest stage that can produce it and satisfies every
//!   consumer, which for a node both stages read is the vertex stage;
//! * **where the stage cut sits** — a vertex-stage value a fragment
//!   consumer reads becomes a *synthesized interpolant*: the same
//!   mechanism a hand-wired `output.varying` drives (ADR 0027), declared
//!   by the analysis instead of the author. One inter-stage location, from
//!   the same accountant, honouring the same budget;
//! * **what was deliberately duplicated** — a shared node whose type
//!   cannot ride an interpolant (a matrix, a bool) or whose interpolant
//!   did not fit the budget is computed in both stages, which is what the
//!   graph did before this module existed and is never an error.
//!
//! The eligibility rules ADR 0025 and 0027 wrote as graph checks — a
//! vertex-only node may not end up running in the fragment stage, a
//! computed interpolant may not be read by the stage that computes it —
//! are stated here, where the assignment happens: an `Auto` node is
//! never mis-placed, because the analysis only chooses stages the node
//! can run in, and an explicit constraint naming a stage the node cannot
//! run in is [`GraphError::WrongStage`], naming the node and the reason.
//!
//! [ADR 0025]: ../../../docs/adr/0025-a-material-graph-spans-shader-stages.md

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{GraphError, GraphErrors};
use crate::graph::{
    AttributeFrequency, Graph, GraphOutputs, NodeId, ShaderStage, SocketRef, StageConstraint,
};
use crate::node::{NodeRegistry, ValueType};
use crate::resources::GeometryInterface;
use crate::wxsl::WxslIdent;

/// Where every node runs, and how the stages exchange values.
#[derive(Clone, Debug, Default)]
pub struct StagePlan {
    /// The stage each analysed node is computed in. Terminals are in
    /// here too, at their own stage.
    pub stage_of: BTreeMap<NodeId, ShaderStage>,
    /// The stage cuts: a vertex-stage output socket that fragment
    /// consumers read, the name of the synthesized interpolant it rides,
    /// and the value type that interpolant carries. In socket order,
    /// which is the order the interpolants claim locations in.
    pub cuts: BTreeMap<SocketRef, (WxslIdent, ValueType)>,
    /// Shared nodes the plan computed *twice* — once per stage — because
    /// no interpolant was spent: the type cannot ride one, or the budget
    /// was already spent. What the graph always did, kept visible rather
    /// than implicit.
    pub duplicated: BTreeSet<NodeId>,
}

impl StagePlan {
    /// Whether anything in this plan differs from "compile per terminal,
    /// duplicate across" — a graph with an empty plan generates exactly
    /// what it did before stage analysis existed.
    pub fn is_empty(&self) -> bool {
        self.cuts.is_empty() && self.duplicated.is_empty()
    }
}

/// Analyse `graph`'s stage placement against `outputs`.
///
/// Pure, device-free, and run by `Graph::validate` as well as by codegen,
/// so the editor's problem panel and the compiler cannot disagree about
/// where a node runs.
pub fn analyze(
    graph: &Graph,
    registry: &NodeRegistry,
    outputs: &GraphOutputs,
) -> Result<StagePlan, GraphErrors> {
    let mut errors = Vec::new();

    // The terminals, at the stages they belong to. These are the roots
    // consumer stages flow back from.
    let fragment_roots: Vec<NodeId> = [Some(outputs.surface), outputs.discard]
        .into_iter()
        .flatten()
        .collect();
    let vertex_roots: Vec<NodeId> = outputs
        .vertex
        .into_iter()
        .chain(outputs.varyings.iter().map(|(_, node)| *node))
        .collect();
    let mut stage_of: BTreeMap<NodeId, ShaderStage> = BTreeMap::new();
    for root in &fragment_roots {
        stage_of.insert(*root, ShaderStage::Fragment);
    }
    for root in &vertex_roots {
        stage_of.insert(*root, ShaderStage::Vertex);
    }

    // Which stages can compile each node at all. `Auto` never has to
    // violate these; only an explicit constraint can, and that is the
    // error that names the author's decision.
    let can_run_in = |graph: &Graph, registry: &NodeRegistry, node: NodeId| -> (bool, bool) {
        let Some(instance) = graph.node(node) else {
            return (true, true);
        };
        let Some(def) = registry.get(&instance.def) else {
            return (true, true);
        };
        if def.is_vertex_only() {
            return (true, false);
        }
        if let Some(name) = graph.attribute_read_name(registry, node) {
            if let Some(decl) = graph.attribute(name.as_str()) {
                if decl.frequency == AttributeFrequency::Computed {
                    return (false, true);
                }
            }
        }
        (true, true)
    };

    // Consumers place producers: walk the graph backwards from the
    // terminals, so every consumer's stage is known before the node it
    // reads is placed.
    let mut order = graph
        .topological_order(None)
        .map_err(|error| GraphErrors(vec![error]))?;
    order.reverse();
    // Nodes both stages need — the cut/duplicate decision's raw material.
    let mut shared: BTreeSet<NodeId> = BTreeSet::new();

    for node in order {
        if stage_of.contains_key(&node) {
            continue;
        }
        let mut vertex_need = false;
        let mut fragment_need = false;
        for edge in graph.edges().iter().filter(|edge| edge.from.node == node) {
            match stage_of.get(&edge.to.node) {
                Some(ShaderStage::Vertex) => vertex_need = true,
                Some(ShaderStage::Fragment) => fragment_need = true,
                // A consumer outside the analysed set (nothing terminal
                // reaches it) places no demand on this node.
                None => {}
            }
        }
        let (can_vertex, _) = can_run_in(graph, registry, node);
        let stage = match graph.stage(node) {
            StageConstraint::Vertex => ShaderStage::Vertex,
            StageConstraint::Fragment => ShaderStage::Fragment,
            StageConstraint::Auto => {
                if vertex_need && can_vertex {
                    // Compute once, per vertex — even when the fragment
                    // stage reads it too; the cut below hands the value
                    // over when it can, and duplicates it when it cannot.
                    ShaderStage::Vertex
                } else if fragment_need {
                    // Whether or not the fragment stage can run it — an
                    // impossible placement is named below, not silently
                    // re-chosen here.
                    ShaderStage::Fragment
                } else {
                    ShaderStage::Vertex
                }
            }
        };
        if vertex_need && fragment_need {
            shared.insert(node);
        }
        stage_of.insert(node, stage);
    }

    // The stage cuts: for every vertex-stage socket a fragment consumer
    // reads, an interpolant — unless the type cannot ride one, or the
    // inter-stage budget cannot pay for one, in which case the node is
    // computed in both stages, which is always legal. An *explicit*
    // vertex constraint gets the error instead of the silent duplicate:
    // the author asked for the vertex stage, and quietly not doing that
    // is not an answer.
    let interpolable = |graph: &Graph, socket: &SocketRef| -> Option<ValueType> {
        let instance = graph.node(socket.node)?;
        let def = registry.get(&instance.def)?;
        let out = def
            .outputs
            .iter()
            .find(|out| out.name.as_str() == socket.socket)?;
        let ty = graph.effective_type(socket.node, out)?;
        crate::abi::INTERPOLANT_TYPES.contains(&ty).then_some(ty)
    };
    let mut cut_sockets: BTreeSet<SocketRef> = BTreeSet::new();
    for edge in graph.edges() {
        if stage_of.get(&edge.from.node) != Some(&ShaderStage::Vertex) {
            continue;
        }
        if stage_of.get(&edge.to.node) == Some(&ShaderStage::Fragment) {
            cut_sockets.insert(edge.from.clone());
        }
    }

    // Locations the declared interpolants already spend, so a cut cannot
    // quietly overrun the budget the accountant guards.
    let mut spend = GeometryInterface::new(
        graph.declared_attributes(AttributeFrequency::Vertex),
        graph.declared_attributes(AttributeFrequency::Instance),
        graph.declared_attributes(AttributeFrequency::Computed),
    )
    .varyings_used();
    let mut cuts: BTreeMap<SocketRef, (WxslIdent, ValueType)> = BTreeMap::new();
    let mut duplicated: BTreeSet<NodeId> = BTreeSet::new();
    let mut next_name = 0usize;
    for socket in cut_sockets {
        let node = socket.node;
        let explicit = graph.stage(node) == StageConstraint::Vertex;
        let Some(ty) = interpolable(graph, &socket) else {
            if explicit {
                errors.push(wrong_stage(
                    graph,
                    node,
                    fragment_roots.first().copied(),
                    format!(
                        "socket `{}` is not a float or a float vector, so it cannot ride \
                     an interpolant to the fragment stage",
                        socket.socket
                    ),
                ));
            } else {
                duplicated.insert(node);
            }
            continue;
        };
        if spend >= crate::abi::MAX_VARYING_LOCATIONS {
            if explicit {
                errors.push(wrong_stage(
                    graph,
                    node,
                    fragment_roots.first().copied(),
                    format!(
                        "the inter-stage locations are spent ({}), and this vertex \
                         value cannot reach the fragment stage — free one, or let the \
                         node be computed in both",
                        crate::abi::MAX_VARYING_LOCATIONS,
                    ),
                ));
            } else {
                duplicated.insert(node);
            }
            continue;
        }
        // Names skip anything the geometry already declares, so a
        // synthesized interpolant can never collide with a real one in
        // the attributes struct both are read through.
        let mut name = format!("auto{next_name}");
        while graph.attribute(&name).is_some() {
            next_name += 1;
            name = format!("auto{next_name}");
        }
        let Some(ident) = WxslIdent::new(&name) else {
            continue;
        };
        cuts.insert(socket, (ident, ty));
        next_name += 1;
        spend += 1;
    }

    // Now that the cuts are known, the stage rules: a node runs in the
    // vertex stage when it was placed there or when a vertex terminal
    // made a duplicate of it, and in the fragment stage when it was
    // placed there or when a fragment consumer read it *without a cut*.
    let cut_nodes: BTreeSet<NodeId> = cuts.keys().map(|socket| socket.node).collect();
    for (&node, &stage) in &stage_of {
        if fragment_roots.contains(&node) || vertex_roots.contains(&node) {
            continue;
        }
        let vertex_need = graph.edges().iter().any(|edge| {
            edge.from.node == node && stage_of.get(&edge.to.node) == Some(&ShaderStage::Vertex)
        });
        let (can_vertex, can_fragment) = can_run_in(graph, registry, node);
        let runs_in_vertex =
            stage == ShaderStage::Vertex || (stage == ShaderStage::Fragment && vertex_need);
        let fragment_need = shared.contains(&node);
        let runs_in_fragment = stage == ShaderStage::Fragment
            || (stage == ShaderStage::Vertex && fragment_need && !cut_nodes.contains(&node));
        if runs_in_vertex && !can_vertex {
            // Name the interpolant when there is one — the reader node's
            // own identity is rarely what the author is looking for.
            let reason = match graph
                .attribute_read_name(registry, node)
                .map(|name| name.as_str().to_string())
            {
                Some(name) => format!(
                    "`{name}` is an interpolant the vertex stage computes, so only \
                     the fragment stage can read it"
                ),
                None => "it reads a computed interpolant, which only the fragment \
                         stage can read"
                    .to_string(),
            };
            errors.push(wrong_stage(
                graph,
                node,
                vertex_roots.first().copied(),
                reason,
            ));
        }
        if runs_in_fragment && !can_fragment {
            errors.push(wrong_stage(
                graph,
                node,
                fragment_roots.first().copied(),
                "it reads object space, which only the vertex stage has".to_string(),
            ));
        }
    }
    // Duplicated = shared, and no interpolant spent on any of its read
    // sockets.
    for node in &shared {
        if !cut_nodes.contains(node) {
            duplicated.insert(*node);
        }
    }

    if !errors.is_empty() {
        return Err(GraphErrors(errors));
    }
    Ok(StagePlan {
        stage_of,
        cuts,
        duplicated,
    })
}

/// The stage error, with the node and the terminal it was on its way to.
fn wrong_stage(graph: &Graph, node: NodeId, output: Option<NodeId>, reason: String) -> GraphError {
    GraphError::WrongStage {
        node,
        def: graph
            .node(node)
            .map(|instance| instance.def.clone())
            .unwrap_or_default(),
        output: output.unwrap_or(node),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi;
    use crate::graph::Node;
    use crate::node::{GenericParam, NodeBody, NodeDefinition, SettingDef, Socket};

    fn registry() -> NodeRegistry {
        let mut registry = NodeRegistry::new();
        registry.register(abi::surface_output_def());
        registry.register(abi::vertex_output_def());
        registry.register(abi::discard_output_def());
        registry.register(abi::varying_output_def());
        registry.register_all(abi::context_node_defs());
        registry.register_all(abi::vertex_context_node_defs());
        registry.register_all([
            NodeDefinition::builder("test.f32", "F32")
                .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
                .output(Socket::new("out", ValueType::F32))
                .expr("{a}"),
            NodeDefinition::builder("test.vec3", "Vec3")
                .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
                .output(Socket::new("out", ValueType::Vec3))
                .expr("vec3f({a})"),
            NodeDefinition::builder("test.bool", "Bool")
                .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
                .output(Socket::new("out", ValueType::Bool))
                .expr("{a} > 0.0"),
            NodeDefinition::builder("test.f32_from_bool", "Bool to f32")
                .input(Socket::new("a", ValueType::Bool))
                .output(Socket::new("out", ValueType::F32))
                .expr("select(0.0, 1.0, {a})"),
            NodeDefinition::builder("input.attribute", "Attribute")
                .setting(SettingDef::new(
                    crate::node::SETTING_NAME,
                    "name",
                    "Which attribute.",
                ))
                .generic_param(GenericParam::new("T", ValueType::ALL.to_vec()))
                .output(Socket::new("out", ValueType::F32).generic("T"))
                .declaration(NodeBody::AttributeRead),
        ]);
        registry
    }

    /// A graph whose one value feeds the vertex offset *and* the surface —
    /// the shared-subtree shape the old rules always duplicated.
    fn shared_graph(registry: &NodeRegistry) -> (Graph, NodeId) {
        let mut graph = Graph::new("shared");
        let value = graph.add(Node::new("test.vec3"));
        let surface = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        let vertex = graph.add(Node::new(abi::VERTEX_OUTPUT_ID));
        graph
            .wire(registry, (value, "out"), (surface, "base_color"))
            .expect("vec3 into base_color");
        graph
            .wire(
                registry,
                (value, "out"),
                (vertex, abi::SOCKET_POSITION_OFFSET),
            )
            .expect("vec3 into the offset");
        (graph, value)
    }

    #[test]
    fn a_node_both_stages_read_is_computed_once_in_the_vertex_stage() {
        let registry = registry();
        let (graph, value) = shared_graph(&registry);
        let outputs = graph.outputs(&registry).expect("complete");
        let plan = analyze(&graph, &registry, &outputs).expect("the plan");

        assert_eq!(plan.stage_of.get(&value), Some(&ShaderStage::Vertex));
        // One cut, riding a synthesized interpolant, for the one socket.
        assert_eq!(plan.cuts.len(), 1);
        let (name, ty) = plan.cuts.values().next().expect("the cut");
        assert_eq!(name.as_str(), "auto0");
        assert_eq!(*ty, ValueType::Vec3);
        assert!(plan.duplicated.is_empty());
    }

    #[test]
    fn an_empty_plan_is_the_common_graph() {
        let registry = registry();
        let mut graph = Graph::new("plain");
        let value = graph.add(Node::new("test.vec3"));
        let surface = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        graph
            .wire(&registry, (value, "out"), (surface, "base_color"))
            .expect("wired");
        let outputs = graph.outputs(&registry).expect("complete");
        let plan = analyze(&graph, &registry, &outputs).expect("the plan");
        assert!(plan.is_empty(), "fragment-only graphs cut nothing");
    }

    #[test]
    fn an_explicit_fragment_constraint_pins_the_old_duplication() {
        let registry = registry();
        let (mut graph, value) = shared_graph(&registry);
        graph.set_stage(value, StageConstraint::Fragment);
        let outputs = graph.outputs(&registry).expect("complete");
        let plan = analyze(&graph, &registry, &outputs).expect("the plan");

        assert_eq!(plan.stage_of.get(&value), Some(&ShaderStage::Fragment));
        assert!(plan.cuts.is_empty(), "no interpolant was spent");
        assert!(plan.duplicated.contains(&value), "both stages compute it");
    }

    #[test]
    fn an_explicit_vertex_constraint_pulls_a_fragment_node_up() {
        let registry = registry();
        let mut graph = Graph::new("hoisted");
        let value = graph.add(Node::new("test.f32").with_stage(StageConstraint::Vertex));
        let surface = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        graph
            .wire(&registry, (value, "out"), (surface, "roughness"))
            .expect("f32 into roughness");
        let outputs = graph.outputs(&registry).expect("complete");
        let plan = analyze(&graph, &registry, &outputs).expect("the plan");

        assert_eq!(plan.stage_of.get(&value), Some(&ShaderStage::Vertex));
        assert_eq!(plan.cuts.len(), 1, "the value rides an interpolant down");
        assert!(plan.duplicated.is_empty());
    }

    #[test]
    fn a_type_that_cannot_interpolate_is_duplicated_not_cut() {
        let registry = registry();
        let mut graph = Graph::new("boolean");
        // A bool the discard test reads *and* a vertex-side chain reads:
        // both stages need it, and a bool cannot ride an interpolant.
        let value = graph.add(Node::new("test.bool"));
        let discard = graph.add(Node::new(abi::DISCARD_OUTPUT_ID));
        let as_f32 = graph.add(Node::new("test.f32_from_bool"));
        let as_vec3 = graph.add(Node::new("test.vec3"));
        let vertex = graph.add(Node::new(abi::VERTEX_OUTPUT_ID));
        // An independent surface, so the graph is complete without giving
        // the bool a second consumer stage through the surface.
        let surface = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        let other = graph.add(Node::new("test.vec3"));
        graph
            .wire(&registry, (other, "out"), (surface, "base_color"))
            .expect("vec3 into base_color");
        graph
            .wire(&registry, (value, "out"), (discard, abi::SOCKET_DISCARD))
            .expect("bool into discard");
        graph
            .wire(&registry, (value, "out"), (as_f32, "a"))
            .expect("bool into the converter");
        graph
            .wire(&registry, (as_f32, "out"), (as_vec3, "a"))
            .expect("f32 into the vec3");
        graph
            .wire(
                &registry,
                (as_vec3, "out"),
                (vertex, abi::SOCKET_POSITION_OFFSET),
            )
            .expect("vec3 into the offset");
        let outputs = graph.outputs(&registry).expect("complete");
        let plan = analyze(&graph, &registry, &outputs).expect("the plan");

        assert!(plan.cuts.is_empty(), "a bool cannot ride an interpolant");
        assert!(plan.duplicated.contains(&value));
    }

    #[test]
    fn a_vertex_only_node_shared_across_the_boundary_is_cut_not_rejected() {
        // What used to be `GraphError::WrongStage` unconditionally —
        // `input.object_position` feeding the surface — is now legal: the
        // value is computed per vertex and interpolated down, which is
        // exactly what the stage boundary is for.
        let registry = registry();
        let mut graph = Graph::new("object space");
        let position = graph.add(Node::new("input.object_position"));
        let surface = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        let vertex = graph.add(Node::new(abi::VERTEX_OUTPUT_ID));
        graph
            .wire(&registry, (position, "out"), (surface, "base_color"))
            .expect("vec3 into base_color");
        graph
            .wire(
                &registry,
                (position, "out"),
                (vertex, abi::SOCKET_POSITION_OFFSET),
            )
            .expect("vec3 into the offset");
        let outputs = graph.outputs(&registry).expect("complete");
        let plan = analyze(&graph, &registry, &outputs).expect("the plan");
        assert_eq!(plan.cuts.len(), 1);
    }

    #[test]
    fn a_vertex_only_node_pinned_to_the_fragment_stage_is_reported() {
        let registry = registry();
        let mut graph = Graph::new("misplaced");
        let position =
            graph.add(Node::new("input.object_position").with_stage(StageConstraint::Fragment));
        let surface = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        graph
            .wire(&registry, (position, "out"), (surface, "base_color"))
            .expect("vec3 into base_color");
        let outputs = graph.outputs(&registry).expect("complete");
        let errors = analyze(&graph, &registry, &outputs).expect_err("the constraint is illegal");
        assert!(
            errors
                .0
                .iter()
                .any(|error| matches!(error, GraphError::WrongStage { .. })),
            "{errors:?}"
        );
    }

    #[test]
    fn an_interpolant_read_in_the_stage_that_computes_it_is_reported() {
        // The mirror rule ADR 0027 wrote, now stated by the analysis: a
        // computed attribute read cannot be part of any vertex-stage
        // partition, which is what feeding the vertex output would make
        // it.
        let registry = registry();
        let mut graph = Graph::new("read too early");
        graph.declare_attribute(crate::graph::AttributeDecl::computed(
            "phase",
            ValueType::Vec3,
        ));
        let reader = graph.add(Node::new("input.attribute").with_setting("name", "phase"));
        graph
            .set_generic(&registry, reader, "T", ValueType::Vec3)
            .expect("allowed");
        let surface = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        let vertex = graph.add(Node::new(abi::VERTEX_OUTPUT_ID));
        graph
            .wire(&registry, (reader, "out"), (surface, "base_color"))
            .expect("vec3 into base_color");
        graph
            .wire(
                &registry,
                (reader, "out"),
                (vertex, abi::SOCKET_POSITION_OFFSET),
            )
            .expect("vec3 into the offset — the mistake");
        let outputs = graph.outputs(&registry).expect("complete");
        let errors =
            analyze(&graph, &registry, &outputs).expect_err("the vertex stage cannot read it");
        assert!(
            errors
                .0
                .iter()
                .any(|error| matches!(error, GraphError::WrongStage { .. })),
            "{errors:?}"
        );
    }

    #[test]
    fn a_cut_that_does_not_fit_the_budget_is_duplicated() {
        let registry = registry();
        let mut graph = Graph::new("no room");
        // Spend every location the declared interpolants can reach, so
        // the shared value has none left.
        let room = crate::abi::MAX_VARYING_LOCATIONS - abi::VERTEX_OUT_FIELDS.len();
        for index in 0..room {
            let name = format!("spent{index}");
            graph.declare_attribute(crate::graph::AttributeDecl::computed(name, ValueType::F32));
        }
        let value = graph.add(Node::new("test.vec3"));
        let surface = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        let vertex = graph.add(Node::new(abi::VERTEX_OUTPUT_ID));
        graph
            .wire(&registry, (value, "out"), (surface, "base_color"))
            .expect("vec3 into base_color");
        graph
            .wire(
                &registry,
                (value, "out"),
                (vertex, abi::SOCKET_POSITION_OFFSET),
            )
            .expect("vec3 into the offset");
        let outputs = graph.outputs(&registry).expect("complete");
        let plan = analyze(&graph, &registry, &outputs).expect("the plan");
        assert!(plan.cuts.is_empty(), "the budget was spent");
        assert!(plan.duplicated.contains(&value));
    }

    #[test]
    fn validate_runs_the_analysis_so_the_editor_sees_what_the_compiler_sees() {
        let registry = registry();
        let mut graph = Graph::new("reported at edit time");
        // A constraint that names a stage the node cannot run in.
        let bogus =
            graph.add(Node::new("input.object_position").with_stage(StageConstraint::Fragment));
        let surface = graph.add(Node::new(abi::SURFACE_OUTPUT_ID));
        graph
            .wire(&registry, (bogus, "out"), (surface, "base_color"))
            .expect("wired");
        let errors = graph
            .validate(&registry)
            .expect_err("reported at validate time");
        assert!(
            errors
                .0
                .iter()
                .any(|error| matches!(error, GraphError::WrongStage { .. })),
            "{errors:?}"
        );
    }
}
