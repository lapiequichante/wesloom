//! The graph itself: nodes, connections, validation and traversal.
//!
//! A [`Graph`] is a typed, directed acyclic graph of [`Node`]s, each an
//! instance of a [`crate::node::NodeDefinition`] looked up by id in a
//! [`NodeRegistry`]. Two invariants are load-bearing, because everything
//! downstream (codegen, the editor, the renderer's variant cache) assumes
//! them:
//!
//! * **Typed.** An edge may only join sockets of the same
//!   [`crate::node::ValueType`]; there is no implicit conversion.
//!   [`Graph::connect`] rejects a mismatch instead of hoping WGSL will.
//! * **Acyclic.** [`Graph::connect`] rejects an edge that would close a
//!   cycle, so a graph built through the API is acyclic by construction.
//!   [`Graph::validate`] re-checks it anyway, since a deserialized graph did
//!   not come through `connect`.
//!
//! Macro variables ([`crate::macros`]) are stored on the graph, which is what
//! makes them editable in the serialized node format: a node definition
//! declares a macro and its default, and the graph pins the value it wants.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use crate::error::{Direction, GraphError, GraphErrors};
use crate::macros::{MacroDef, MacroSet, MacroValue};
use crate::node::{NodeDefinition, NodeRegistry, Value};

/// Identifier of a node within one graph.
///
/// Stable across edits (removing a node never renumbers the others) so a
/// serialized graph's edges keep meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct NodeId(pub u32);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// A reference to one socket on one node.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SocketRef {
    /// The node the socket belongs to.
    pub node: NodeId,
    /// The socket's name.
    pub socket: String,
}

impl SocketRef {
    /// Reference socket `socket` on `node`.
    pub fn new(node: NodeId, socket: impl Into<String>) -> Self {
        SocketRef {
            node,
            socket: socket.into(),
        }
    }
}

impl fmt::Display for SocketRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.node, self.socket)
    }
}

/// A connection from one node's output to another node's input.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Edge {
    /// Producing output socket.
    pub from: SocketRef,
    /// Consuming input socket.
    pub to: SocketRef,
}

/// A node instance: which definition it is, plus the values pinned on its
/// unconnected inputs.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Node {
    /// Registry id of this node's definition.
    pub def: String,
    /// Values for unconnected inputs, by socket name. A parameter for a
    /// connected input is ignored (the edge wins), but kept, so unplugging an
    /// edge restores the value the user last typed.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "BTreeMap::is_empty")
    )]
    pub params: BTreeMap<String, Value>,
    /// Optional display name, overriding the definition's label.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub label: Option<String>,
    /// Editor canvas position. Meaningless to codegen, round-tripped so the
    /// layout survives a save/load.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub position: Option<[f32; 2]>,
}

impl Node {
    /// A node instance of definition `def` with no parameters pinned.
    pub fn new(def: impl Into<String>) -> Self {
        Node {
            def: def.into(),
            params: BTreeMap::new(),
            label: None,
            position: None,
        }
    }

    /// Pin `value` on the input named `socket`.
    pub fn with_param(mut self, socket: impl Into<String>, value: Value) -> Self {
        self.params.insert(socket.into(), value);
        self
    }

    /// Place the node on the editor canvas.
    pub fn with_position(mut self, position: [f32; 2]) -> Self {
        self.position = Some(position);
        self
    }

    /// Give the node a display name.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// A typed, acyclic shader node graph.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(from = "wire::WireGraph", into = "wire::WireGraph")
)]
pub struct Graph {
    name: String,
    nodes: BTreeMap<NodeId, Node>,
    edges: Vec<Edge>,
    macros: MacroSet,
    next_id: u32,
}

impl Graph {
    /// An empty graph.
    pub fn new(name: impl Into<String>) -> Self {
        Graph {
            name: name.into(),
            nodes: BTreeMap::new(),
            edges: Vec::new(),
            macros: MacroSet::new(),
            next_id: 1,
        }
    }

    /// The graph's name. Used to label generated shader modules.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Rename the graph.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// Insert `node`, returning its fresh id.
    pub fn add(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        self.nodes.insert(id, node);
        id
    }

    /// Insert a node of definition `def` with no parameters.
    pub fn add_node(&mut self, def: impl Into<String>) -> NodeId {
        self.add(Node::new(def))
    }

    /// Remove a node and every edge touching it.
    ///
    /// Returns the removed node, or `None` if there was no such node.
    pub fn remove_node(&mut self, id: NodeId) -> Option<Node> {
        let node = self.nodes.remove(&id)?;
        self.edges
            .retain(|edge| edge.from.node != id && edge.to.node != id);
        Some(node)
    }

    /// Look up a node.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// Look up a node for mutation (to pin a parameter, or move it).
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(&id)
    }

    /// Iterate over `(id, node)` pairs in id order.
    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes.iter().map(|(id, node)| (*id, node))
    }

    /// Number of nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Every edge, in insertion order.
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Pin `value` on the input socket `socket` of `node`.
    ///
    /// Returns the previous value. Does nothing and returns `None` if the
    /// node does not exist — validation reports the dangling reference.
    pub fn set_param(
        &mut self,
        node: NodeId,
        socket: impl Into<String>,
        value: Value,
    ) -> Option<Value> {
        self.nodes
            .get_mut(&node)
            .and_then(|n| n.params.insert(socket.into(), value))
    }

    /// The macro values this graph pins.
    pub fn macros(&self) -> &MacroSet {
        &self.macros
    }

    /// Pin a macro variable, overriding whatever default the node definitions
    /// declare. This is the graph-format knob for macro variables.
    pub fn set_macro(&mut self, name: impl Into<String>, value: MacroValue) -> Option<MacroValue> {
        self.macros.set(name, value)
    }

    /// Unpin a macro variable, so it falls back to its declared default.
    pub fn unset_macro(&mut self, name: &str) -> Option<MacroValue> {
        self.macros.unset(name)
    }

    /// Replace the whole pinned macro set.
    pub fn set_macros(&mut self, macros: MacroSet) {
        self.macros = macros;
    }

    /// Connect an output socket to an input socket.
    ///
    /// Enforces the graph's invariants: both sockets must exist and have the
    /// same type, the input must be free, and the edge must not close a
    /// cycle. On error nothing is changed.
    pub fn connect(
        &mut self,
        registry: &NodeRegistry,
        from: SocketRef,
        to: SocketRef,
    ) -> Result<(), GraphError> {
        let from_def = self.definition(registry, from.node)?;
        let produced = from_def
            .output(&from.socket)
            .ok_or_else(|| GraphError::UnknownSocket {
                socket: from.clone(),
                direction: Direction::Output,
            })?
            .ty;

        let to_def = self.definition(registry, to.node)?;
        let expected = to_def
            .input(&to.socket)
            .ok_or_else(|| GraphError::UnknownSocket {
                socket: to.clone(),
                direction: Direction::Input,
            })?
            .ty;

        if produced != expected {
            return Err(GraphError::TypeMismatch {
                from,
                to,
                produced,
                expected,
            });
        }
        if self.edge_into(&to).is_some() {
            return Err(GraphError::InputAlreadyConnected { socket: to });
        }
        // The new edge points from.node -> to.node, so it closes a cycle iff
        // from.node is already reachable downstream of to.node.
        if from.node == to.node || self.reaches(to.node, from.node) {
            return Err(GraphError::WouldCycle { from, to });
        }

        self.edges.push(Edge { from, to });
        Ok(())
    }

    /// Convenience wrapper over [`Graph::connect`] taking `(node, socket)`
    /// pairs, for graphs built in code.
    pub fn wire(
        &mut self,
        registry: &NodeRegistry,
        from: (NodeId, &str),
        to: (NodeId, &str),
    ) -> Result<(), GraphError> {
        self.connect(
            registry,
            SocketRef::new(from.0, from.1),
            SocketRef::new(to.0, to.1),
        )
    }

    /// Remove the edge feeding `input`, if any, and return it.
    pub fn disconnect(&mut self, input: &SocketRef) -> Option<Edge> {
        let index = self.edges.iter().position(|edge| &edge.to == input)?;
        Some(self.edges.remove(index))
    }

    /// The edge feeding `input`, if any.
    pub fn edge_into(&self, input: &SocketRef) -> Option<&Edge> {
        self.edges.iter().find(|edge| &edge.to == input)
    }

    /// The first edge leaving the output socket `output`, if any.
    ///
    /// Codegen uses this to skip binding outputs nobody reads.
    pub fn edge_from(&self, output: &SocketRef) -> Option<&Edge> {
        self.edges.iter().find(|edge| &edge.from == output)
    }

    /// Every edge leaving `node`.
    pub fn edges_from(&self, node: NodeId) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |edge| edge.from.node == node)
    }

    /// Every edge entering `node`.
    pub fn edges_into(&self, node: NodeId) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |edge| edge.to.node == node)
    }

    /// The nodes `root` depends on, including `root` itself.
    ///
    /// Codegen uses this to emit only what the output node actually needs, so
    /// a work-in-progress branch left dangling in the editor costs nothing in
    /// the compiled shader.
    pub fn dependencies_of(&self, root: NodeId) -> BTreeSet<NodeId> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if !seen.insert(node) {
                continue;
            }
            for edge in self.edges_into(node) {
                stack.push(edge.from.node);
            }
        }
        seen
    }

    /// Nodes in dependency order: every node appears after the nodes feeding
    /// it. Restricted to `subset` when given.
    ///
    /// Fails with [`GraphError::Cycle`] if the graph has a cycle, which only
    /// a graph that was not built through [`Graph::connect`] can.
    pub fn topological_order(
        &self,
        subset: Option<&BTreeSet<NodeId>>,
    ) -> Result<Vec<NodeId>, GraphError> {
        let included = |id: NodeId| subset.is_none_or(|set| set.contains(&id));
        let mut indegree: BTreeMap<NodeId, usize> = self
            .nodes
            .keys()
            .copied()
            .filter(|id| included(*id))
            .map(|id| (id, 0))
            .collect();
        for edge in &self.edges {
            if !included(edge.from.node) || !included(edge.to.node) {
                continue;
            }
            if let Some(count) = indegree.get_mut(&edge.to.node) {
                *count += 1;
            }
        }

        let mut queue: VecDeque<NodeId> = indegree
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut order = Vec::with_capacity(indegree.len());
        while let Some(node) = queue.pop_front() {
            order.push(node);
            for edge in self.edges_from(node) {
                if !included(edge.to.node) {
                    continue;
                }
                if let Some(count) = indegree.get_mut(&edge.to.node) {
                    *count -= 1;
                    if *count == 0 {
                        queue.push_back(edge.to.node);
                    }
                }
            }
        }

        if order.len() != indegree.len() {
            let placed: BTreeSet<NodeId> = order.iter().copied().collect();
            let nodes = indegree
                .keys()
                .copied()
                .filter(|id| !placed.contains(id))
                .collect();
            return Err(GraphError::Cycle { nodes });
        }
        Ok(order)
    }

    /// The macros declared by the graph's nodes, with the defaults they ask
    /// for, keyed by name.
    ///
    /// A macro two nodes declare with different defaults is reported as
    /// [`GraphError::ConflictingMacroDefault`] unless the graph pins it.
    pub fn declared_macros(
        &self,
        registry: &NodeRegistry,
    ) -> (BTreeMap<String, MacroDef>, Vec<GraphError>) {
        let mut declared: BTreeMap<String, MacroDef> = BTreeMap::new();
        let mut errors = Vec::new();
        for (_, node) in self.nodes() {
            let Some(def) = registry.get(&node.def) else {
                continue;
            };
            for decl in &def.macros {
                let name = decl.name.as_str().to_string();
                match declared.get(&name) {
                    None => {
                        declared.insert(name, decl.clone());
                    }
                    Some(existing) if existing.default != decl.default => {
                        if !self.macros.contains(&name) {
                            errors.push(GraphError::ConflictingMacroDefault {
                                name,
                                first: existing.default,
                                second: decl.default,
                            });
                        }
                    }
                    Some(_) => {}
                }
            }
        }
        (declared, errors)
    }

    /// The macro values a shader compiled from this graph should be built
    /// with: every declared default, overlaid with the graph's own pins.
    ///
    /// Names the graph pins that no node declares are kept — hand-written
    /// WESL in the stdlib has `@if` blocks of its own, and the render path
    /// flag ([`crate::abi::FEATURE_DEFERRED`]) is bound the same way.
    pub fn effective_macros(&self, registry: &NodeRegistry) -> Result<MacroSet, GraphErrors> {
        let (declared, mut errors) = self.declared_macros(registry);
        let mut effective = MacroSet::new();
        for (name, decl) in &declared {
            effective.set(name.clone(), decl.default);
        }
        for (name, value) in self.macros.iter() {
            if crate::wesl::WeslIdent::new(name).is_none() {
                errors.push(GraphError::InvalidMacroName {
                    name: name.to_string(),
                });
                continue;
            }
            if let Some(decl) = declared.get(name) {
                if decl.default.kind() != value.kind() {
                    errors.push(GraphError::MacroKindMismatch {
                        name: name.to_string(),
                        supplied: value,
                        declared: decl.default,
                    });
                    continue;
                }
            }
            effective.set(name.to_string(), value);
        }
        if errors.is_empty() {
            Ok(effective)
        } else {
            Err(GraphErrors(errors))
        }
    }

    /// The node ids whose definition is the surface output.
    ///
    /// A compilable graph has exactly one; the plural return lets both
    /// "none" and "several" be reported precisely.
    pub fn surface_outputs(&self, registry: &NodeRegistry) -> Vec<NodeId> {
        self.nodes()
            .filter(|(_, node)| {
                registry
                    .get(&node.def)
                    .is_some_and(|def| def.is_surface_output())
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// Check every invariant against `registry`, reporting all problems found.
    pub fn validate(&self, registry: &NodeRegistry) -> Result<(), GraphErrors> {
        let mut errors = Vec::new();

        for (id, node) in self.nodes() {
            let Some(def) = registry.get(&node.def) else {
                errors.push(GraphError::UnknownDefinition {
                    node: id,
                    def: node.def.clone(),
                });
                continue;
            };
            self.check_params(id, node, def, &mut errors);
        }

        let mut connected_inputs: BTreeSet<SocketRef> = BTreeSet::new();
        for edge in &self.edges {
            self.check_edge(registry, edge, &mut connected_inputs, &mut errors);
        }

        // Required inputs must be fed by an edge, a parameter or a default.
        for (id, node) in self.nodes() {
            let Some(def) = registry.get(&node.def) else {
                continue;
            };
            for socket in &def.inputs {
                let name = socket.name.as_str();
                let reference = SocketRef::new(id, name);
                let fed = connected_inputs.contains(&reference)
                    || node.params.contains_key(name)
                    || socket.default.is_some()
                    || socket.optional;
                if !fed {
                    errors.push(GraphError::MissingInput { socket: reference });
                }
            }
        }

        if let Err(error) = self.topological_order(None) {
            errors.push(error);
        }
        if let Err(GraphErrors(macro_errors)) = self.effective_macros(registry) {
            errors.extend(macro_errors);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(GraphErrors(errors))
        }
    }

    fn check_params(
        &self,
        id: NodeId,
        node: &Node,
        def: &NodeDefinition,
        errors: &mut Vec<GraphError>,
    ) {
        for (name, value) in &node.params {
            match def.input(name) {
                None => errors.push(GraphError::UnknownParam {
                    node: id,
                    param: name.clone(),
                }),
                Some(socket) if socket.ty != value.ty() => {
                    errors.push(GraphError::ParamTypeMismatch {
                        socket: SocketRef::new(id, name),
                        supplied: value.ty(),
                        expected: socket.ty,
                    })
                }
                Some(_) => {}
            }
        }
    }

    fn check_edge(
        &self,
        registry: &NodeRegistry,
        edge: &Edge,
        connected_inputs: &mut BTreeSet<SocketRef>,
        errors: &mut Vec<GraphError>,
    ) {
        let produced = match self.socket_type(registry, &edge.from, Direction::Output) {
            Ok(ty) => Some(ty),
            Err(error) => {
                errors.push(error);
                None
            }
        };
        let expected = match self.socket_type(registry, &edge.to, Direction::Input) {
            Ok(ty) => Some(ty),
            Err(error) => {
                errors.push(error);
                None
            }
        };
        if let (Some(produced), Some(expected)) = (produced, expected) {
            if produced != expected {
                errors.push(GraphError::TypeMismatch {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    produced,
                    expected,
                });
            }
        }
        if !connected_inputs.insert(edge.to.clone()) {
            errors.push(GraphError::InputAlreadyConnected {
                socket: edge.to.clone(),
            });
        }
    }

    fn socket_type(
        &self,
        registry: &NodeRegistry,
        socket: &SocketRef,
        direction: Direction,
    ) -> Result<crate::node::ValueType, GraphError> {
        let def = self.definition(registry, socket.node)?;
        let found = match direction {
            Direction::Input => def.input(&socket.socket),
            Direction::Output => def.output(&socket.socket),
        };
        found
            .map(|s| s.ty)
            .ok_or_else(|| GraphError::UnknownSocket {
                socket: socket.clone(),
                direction,
            })
    }

    /// The definition of `id`, or the error explaining why there is none.
    pub fn definition<'r>(
        &self,
        registry: &'r NodeRegistry,
        id: NodeId,
    ) -> Result<&'r NodeDefinition, GraphError> {
        let node = self.nodes.get(&id).ok_or(GraphError::UnknownNode(id))?;
        registry
            .get(&node.def)
            .map(|def| def.as_ref())
            .ok_or_else(|| GraphError::UnknownDefinition {
                node: id,
                def: node.def.clone(),
            })
    }

    /// Whether `target` is reachable from `start` by following edges forward.
    fn reaches(&self, start: NodeId, target: NodeId) -> bool {
        let mut seen = BTreeSet::new();
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            if node == target {
                return true;
            }
            if !seen.insert(node) {
                continue;
            }
            for edge in self.edges_from(node) {
                stack.push(edge.to.node);
            }
        }
        false
    }
}

#[cfg(feature = "serde")]
mod wire {
    //! The on-disk shape of a [`super::Graph`].
    //!
    //! Kept separate from the in-memory type so the file format stays a
    //! deliberate, readable thing (a list of nodes with ids, not a map keyed
    //! by stringified integers) and so `next_id` — an implementation detail —
    //! is recomputed on load rather than trusted from the file.

    use std::collections::BTreeMap;

    use serde::{Deserialize, Serialize};

    use super::{Edge, Graph, Node, NodeId};
    use crate::macros::MacroSet;

    #[derive(Serialize, Deserialize)]
    pub(super) struct WireNode {
        pub id: NodeId,
        #[serde(flatten)]
        pub node: Node,
    }

    #[derive(Serialize, Deserialize)]
    pub(super) struct WireGraph {
        #[serde(default)]
        pub name: String,
        #[serde(default, skip_serializing_if = "MacroSet::is_empty")]
        pub macros: MacroSet,
        #[serde(default)]
        pub nodes: Vec<WireNode>,
        #[serde(default)]
        pub edges: Vec<Edge>,
    }

    impl From<Graph> for WireGraph {
        fn from(graph: Graph) -> Self {
            WireGraph {
                name: graph.name,
                macros: graph.macros,
                nodes: graph
                    .nodes
                    .into_iter()
                    .map(|(id, node)| WireNode { id, node })
                    .collect(),
                edges: graph.edges,
            }
        }
    }

    impl From<WireGraph> for Graph {
        fn from(wire: WireGraph) -> Self {
            let nodes: BTreeMap<NodeId, Node> = wire
                .nodes
                .into_iter()
                .map(|entry| (entry.id, entry.node))
                .collect();
            let next_id = nodes.keys().map(|id| id.0).max().unwrap_or(0) + 1;
            Graph {
                name: wire.name,
                nodes,
                edges: wire.edges,
                macros: wire.macros,
                next_id,
            }
        }
    }
}

// Without the `serde` feature there is no wire format, but `Graph`'s
// `serde(from/into)` attributes are also gone, so the module can be empty.
#[cfg(not(feature = "serde"))]
mod wire {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Socket, ValueType};

    fn registry() -> NodeRegistry {
        let mut registry = NodeRegistry::new();
        registry.register_all([
            NodeDefinition::builder("test.const", "Const")
                .output(Socket::new("out", ValueType::F32))
                .expr("1.0"),
            NodeDefinition::builder("test.add", "Add")
                .input(Socket::new("a", ValueType::F32).with_splat_default(0.0))
                .input(Socket::new("b", ValueType::F32).with_splat_default(0.0))
                .output(Socket::new("out", ValueType::F32))
                .expr("{a} + {b}"),
            NodeDefinition::builder("test.vec", "Vec")
                .input(Socket::new("v", ValueType::Vec3).with_splat_default(0.0))
                .output(Socket::new("out", ValueType::Vec3))
                .expr("{v}"),
            NodeDefinition::builder("test.required", "Required")
                .input(Socket::new("must", ValueType::F32))
                .output(Socket::new("out", ValueType::F32))
                .expr("{must}"),
        ]);
        registry
    }

    #[test]
    fn connect_type_checks_both_ends() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let a = graph.add_node("test.const");
        let v = graph.add_node("test.vec");
        let error = graph
            .wire(&registry, (a, "out"), (v, "v"))
            .expect_err("f32 must not connect to vec3f");
        assert!(matches!(error, GraphError::TypeMismatch { .. }));
        assert!(graph.edges().is_empty(), "a rejected edge is not added");
    }

    #[test]
    fn connect_rejects_unknown_sockets() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let a = graph.add_node("test.const");
        let b = graph.add_node("test.add");
        assert!(matches!(
            graph.wire(&registry, (a, "nope"), (b, "a")),
            Err(GraphError::UnknownSocket {
                direction: Direction::Output,
                ..
            })
        ));
        assert!(matches!(
            graph.wire(&registry, (a, "out"), (b, "nope")),
            Err(GraphError::UnknownSocket {
                direction: Direction::Input,
                ..
            })
        ));
    }

    #[test]
    fn inputs_take_exactly_one_edge() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let a = graph.add_node("test.const");
        let b = graph.add_node("test.const");
        let add = graph.add_node("test.add");
        graph.wire(&registry, (a, "out"), (add, "a")).unwrap();
        assert!(matches!(
            graph.wire(&registry, (b, "out"), (add, "a")),
            Err(GraphError::InputAlreadyConnected { .. })
        ));
        // Unplugging frees the input again.
        graph.disconnect(&SocketRef::new(add, "a")).unwrap();
        graph.wire(&registry, (b, "out"), (add, "a")).unwrap();
    }

    #[test]
    fn cycles_are_rejected_at_connect_time() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let first = graph.add_node("test.add");
        let second = graph.add_node("test.add");
        graph
            .wire(&registry, (first, "out"), (second, "a"))
            .unwrap();
        assert!(matches!(
            graph.wire(&registry, (second, "out"), (first, "a")),
            Err(GraphError::WouldCycle { .. })
        ));
        assert!(matches!(
            graph.wire(&registry, (first, "out"), (first, "b")),
            Err(GraphError::WouldCycle { .. })
        ));
        assert_eq!(graph.edges().len(), 1);
        graph.validate(&registry).expect("still a valid DAG");
    }

    #[test]
    fn topological_order_respects_dependencies() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let a = graph.add_node("test.const");
        let b = graph.add_node("test.add");
        let c = graph.add_node("test.add");
        graph.wire(&registry, (a, "out"), (b, "a")).unwrap();
        graph.wire(&registry, (b, "out"), (c, "a")).unwrap();
        let order = graph.topological_order(None).unwrap();
        let position = |id: NodeId| order.iter().position(|x| *x == id).unwrap();
        assert!(position(a) < position(b));
        assert!(position(b) < position(c));

        // Restricted to a subset, unrelated nodes are left out.
        let subset = graph.dependencies_of(b);
        let order = graph.topological_order(Some(&subset)).unwrap();
        assert_eq!(order, vec![a, b]);
    }

    #[test]
    fn removing_a_node_removes_its_edges() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let a = graph.add_node("test.const");
        let b = graph.add_node("test.add");
        graph.wire(&registry, (a, "out"), (b, "a")).unwrap();
        graph.remove_node(a).unwrap();
        assert!(graph.edges().is_empty());
        assert!(graph.node(a).is_none());
        // Ids are never reused, so old edges cannot be resurrected.
        let c = graph.add_node("test.const");
        assert_ne!(c, a);
    }

    #[test]
    fn validate_reports_every_problem_at_once() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let missing = graph.add(Node::new("test.required"));
        let unknown_def = graph.add(Node::new("test.does_not_exist"));
        let bad_param = graph.add(Node::new("test.add").with_param("a", Value::Vec3([0.0; 3])));
        let unknown_param = graph.add(Node::new("test.add").with_param("nope", Value::F32(1.0)));

        let errors = graph.validate(&registry).expect_err("graph is invalid");
        assert_eq!(errors.len(), 4, "{errors}");
        assert!(errors.0.iter().any(|e| matches!(
            e,
            GraphError::MissingInput { socket } if socket.node == missing
        )));
        assert!(errors.0.iter().any(
            |e| matches!(e, GraphError::UnknownDefinition { node, .. } if *node == unknown_def)
        ));
        assert!(errors.0.iter().any(|e| matches!(
            e,
            GraphError::ParamTypeMismatch { socket, .. } if socket.node == bad_param
        )));
        assert!(errors
            .0
            .iter()
            .any(|e| matches!(e, GraphError::UnknownParam { node, .. } if *node == unknown_param)));
    }

    #[test]
    fn a_pinned_parameter_satisfies_a_required_input() {
        let registry = registry();
        let mut graph = Graph::new("g");
        graph.add(Node::new("test.required").with_param("must", Value::F32(0.25)));
        graph.validate(&registry).unwrap();
    }
}
