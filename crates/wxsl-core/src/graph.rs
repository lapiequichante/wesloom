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
use crate::node::{NodeDefinition, NodeRegistry, Socket, Value, ValueType};
use crate::wxsl::WxslIdent;

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
    /// Resolved concrete type for each generic parameter this node's
    /// definition declares, by parameter name. Empty for a node whose
    /// definition has no [`crate::node::GenericParam`]s. A declared
    /// parameter missing here means this instance has not resolved it yet —
    /// see [`crate::node::GenericParam`] and [`Graph::set_generic`].
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "BTreeMap::is_empty")
    )]
    pub generics: BTreeMap<String, ValueType>,
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
            generics: BTreeMap::new(),
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
    ///
    /// A generic socket ([`crate::node::Socket::generic`]) is "the same
    /// type" a little more flexibly: if this node instance has not yet
    /// resolved that parameter, connecting adopts whichever type the other,
    /// already-typed side has — this is what lets one node kind (e.g.
    /// `math.add`, generic over `f32 | vec2f | vec3f | vec4f`) serve every
    /// concrete type, one per instance, instead of `math.add.f32`,
    /// `math.add.vec2f`, … as separate registry entries. Once resolved
    /// (however that happened — an earlier connection, or an explicit
    /// [`Graph::set_generic`]), a further mismatched connection is rejected
    /// exactly like a mismatch between two ordinary concretely-typed
    /// sockets. The resolution also propagates to anything this node was
    /// already wired to through a socket sharing the same parameter, so a
    /// chain of generic nodes connected before any of them touched a
    /// concrete type resolves together, not one hop at a time.
    pub fn connect(
        &mut self,
        registry: &NodeRegistry,
        from: SocketRef,
        to: SocketRef,
    ) -> Result<(), GraphError> {
        let from_def = self.definition(registry, from.node)?;
        let from_socket =
            from_def
                .output(&from.socket)
                .ok_or_else(|| GraphError::UnknownSocket {
                    socket: from.clone(),
                    direction: Direction::Output,
                })?;
        let from_generic = from_socket.generic.clone();
        let produced = self.effective_type(from.node, from_socket);

        let to_def = self.definition(registry, to.node)?;
        let to_socket = to_def
            .input(&to.socket)
            .ok_or_else(|| GraphError::UnknownSocket {
                socket: to.clone(),
                direction: Direction::Input,
            })?;
        let to_generic = to_socket.generic.clone();
        let expected = self.effective_type(to.node, to_socket);

        /// Which side, if either, a successful connection should resolve.
        enum Adopt {
            Neither,
            Source(WxslIdent),
            Target(WxslIdent),
        }
        let (agreed, adopt) = match (produced, expected) {
            (Some(p), Some(e)) if p == e => (Some(p), Adopt::Neither),
            (Some(produced), Some(expected)) => {
                return Err(GraphError::TypeMismatch {
                    from,
                    to,
                    produced,
                    expected,
                })
            }
            (Some(p), None) => (
                Some(p),
                Adopt::Target(to_generic.expect("an unresolved type implies a generic socket")),
            ),
            (None, Some(e)) => (
                Some(e),
                Adopt::Source(from_generic.expect("an unresolved type implies a generic socket")),
            ),
            // Neither side is resolved yet: nothing to check or adopt. The
            // edge is still valid — a chain of unconstrained generics stays
            // polymorphic until something anchors it; `Graph::validate`
            // reports the whole chain as unresolved until it does.
            (None, None) => (None, Adopt::Neither),
        };

        if self.edge_into(&to).is_some() {
            return Err(GraphError::InputAlreadyConnected { socket: to });
        }
        // The new edge points from.node -> to.node, so it closes a cycle iff
        // from.node is already reachable downstream of to.node.
        if from.node == to.node || self.reaches(to.node, from.node) {
            return Err(GraphError::WouldCycle { from, to });
        }

        self.edges.push(Edge {
            from: from.clone(),
            to: to.clone(),
        });
        match adopt {
            Adopt::Neither => {}
            Adopt::Source(param) => self.resolve_generic(
                registry,
                from.node,
                param.as_str(),
                agreed.expect("adopting resolves from an already-agreed type"),
            ),
            Adopt::Target(param) => self.resolve_generic(
                registry,
                to.node,
                param.as_str(),
                agreed.expect("adopting resolves from an already-agreed type"),
            ),
        }
        Ok(())
    }

    /// A socket's effective type: `socket.ty` when it is not generic, or
    /// this node instance's resolution of [`Socket::generic`] otherwise
    /// (`None` if that parameter has not been resolved yet).
    ///
    /// `pub(crate)` rather than a free function on [`Socket`] because
    /// resolution is per graph *node*, not something a bare `Socket` (shared
    /// across every instance of its [`NodeDefinition`]) can answer alone.
    /// `codegen` is the other caller within this crate: once
    /// [`Graph::validate`] has passed, every generic socket codegen touches
    /// resolves to `Some`.
    pub(crate) fn effective_type(&self, node: NodeId, socket: &Socket) -> Option<ValueType> {
        match &socket.generic {
            None => Some(socket.ty),
            Some(param) => self.nodes.get(&node)?.generics.get(param.as_str()).copied(),
        }
    }

    /// Resolve `node`'s generic parameter `param` to `ty`, then propagate
    /// that resolution across any edge already touching another socket that
    /// shares it — on `node` itself (every one of its own sockets sharing
    /// `param` sees the new value immediately, since resolution is stored
    /// once per node) and, transitively, on whatever `node` was already
    /// wired to through such a socket.
    ///
    /// Does not re-check those existing edges against the newly resolved
    /// type: nothing could have connected an incompatible one to a socket
    /// that was still unresolved in the first place — [`Graph::connect`]
    /// only reaches this once every edge already touching `param` agrees —
    /// so there is nothing to reconcile, only more of the graph to inform.
    fn resolve_generic(
        &mut self,
        registry: &NodeRegistry,
        node: NodeId,
        param: &str,
        ty: ValueType,
    ) {
        let mut queue = VecDeque::from([(node, param.to_string())]);
        while let Some((node, param)) = queue.pop_front() {
            let Some(current) = self.nodes.get(&node) else {
                continue;
            };
            if current.generics.get(&param) == Some(&ty) {
                continue; // already settled, by this call or an earlier one
            }
            let Some(def) = registry.get(&current.def).cloned() else {
                continue;
            };
            self.nodes
                .get_mut(&node)
                .expect("looked up above")
                .generics
                .insert(param.clone(), ty);

            let shares_param = |socket: &&Socket| {
                socket
                    .generic
                    .as_ref()
                    .is_some_and(|name| name.as_str() == param)
            };
            for socket in def.inputs.iter().filter(shares_param) {
                let reference = SocketRef::new(node, socket.name.as_str());
                if let Some(edge) = self.edge_into(&reference) {
                    self.enqueue_generic_neighbor(
                        registry,
                        &edge.from,
                        Direction::Output,
                        &mut queue,
                    );
                }
            }
            for socket in def.outputs.iter().filter(shares_param) {
                for edge in self
                    .edges_from(node)
                    .filter(|e| e.from.socket == socket.name.as_str())
                {
                    let to = edge.to.clone();
                    self.enqueue_generic_neighbor(registry, &to, Direction::Input, &mut queue);
                }
            }
        }
    }

    /// If `socket` (on the side named by `direction`) is itself generic,
    /// queue its `(node, parameter)` for [`Graph::resolve_generic`] to visit.
    fn enqueue_generic_neighbor(
        &self,
        registry: &NodeRegistry,
        socket: &SocketRef,
        direction: Direction,
        queue: &mut VecDeque<(NodeId, String)>,
    ) {
        let Ok(def) = self.definition(registry, socket.node) else {
            return;
        };
        let found = match direction {
            Direction::Input => def.input(&socket.socket),
            Direction::Output => def.output(&socket.socket),
        };
        if let Some(param) = found.and_then(|s| s.generic.as_ref()) {
            queue.push_back((socket.node, param.as_str().to_string()));
        }
    }

    /// Explicitly resolve `node`'s generic parameter `param` to `ty`.
    ///
    /// For a node nothing has connected yet (so [`Graph::connect`] never got
    /// a chance to adopt a type for it), or to change what an already-wired
    /// one resolved to. Repinning to a type incompatible with an edge
    /// already touching a socket that shares `param` **disconnects that
    /// edge** rather than rejecting the repin outright — changing what a
    /// node is supposed to invalidate wiring that no longer fits, the same
    /// way it always would have if `math.add.f32` and `math.add.vec3f` were
    /// still two different node kinds.
    ///
    /// Returns the edges that were disconnected as a result, so a caller
    /// (the editor) can report it rather than have wiring vanish silently.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::UnknownDefinition`]/[`GraphError::UnknownNode`]
    /// for a bad `node`, or [`GraphError::InvalidGenericType`] if `param`
    /// does not declare `ty` as one of its allowed types (including if
    /// `param` is not a generic parameter this node's definition declares
    /// at all, reported with an empty allowed list).
    pub fn set_generic(
        &mut self,
        registry: &NodeRegistry,
        node: NodeId,
        param: &str,
        ty: ValueType,
    ) -> Result<Vec<Edge>, GraphError> {
        let def = self.definition(registry, node)?.clone();
        let allowed = def
            .generic(param)
            .map(|declared| declared.allowed.clone())
            .unwrap_or_default();
        if !allowed.contains(&ty) {
            return Err(GraphError::InvalidGenericType {
                node,
                param: param.to_string(),
                ty,
                allowed,
            });
        }

        let shares_param = |socket: &&Socket| {
            socket
                .generic
                .as_ref()
                .is_some_and(|name| name.as_str() == param)
        };
        // Every input this repin would leave mismatched: an existing edge
        // whose *other* end's effective type is no longer `ty`. Collected
        // before mutating anything, since disconnecting is decided against
        // the state before the repin.
        let mut mismatched = Vec::new();
        for socket in def.inputs.iter().filter(shares_param) {
            let reference = SocketRef::new(node, socket.name.as_str());
            if let Some(edge) = self.edge_into(&reference) {
                let other = self
                    .socket_type(registry, &edge.from, Direction::Output)
                    .unwrap_or(None);
                if other != Some(ty) {
                    mismatched.push(reference);
                }
            }
        }
        for socket in def.outputs.iter().filter(shares_param) {
            for edge in self
                .edges_from(node)
                .filter(|edge| edge.from.socket == socket.name.as_str())
            {
                let other = self
                    .socket_type(registry, &edge.to, Direction::Input)
                    .unwrap_or(None);
                if other != Some(ty) {
                    mismatched.push(edge.to.clone());
                }
            }
        }

        let disconnected = mismatched
            .into_iter()
            .filter_map(|input| self.disconnect(&input))
            .collect();

        self.nodes
            .get_mut(&node)
            .expect("checked to exist by `definition` above")
            .generics
            .insert(param.to_string(), ty);
        Ok(disconnected)
    }

    /// This node instance's resolved concrete type for generic parameter
    /// `param`, or `None` if it has not been resolved (or the node has no
    /// such parameter). For the editor: what to show next to a "pick a
    /// type" control, and whether to show one at all.
    pub fn generic_type(&self, node: NodeId, param: &str) -> Option<ValueType> {
        self.nodes.get(&node)?.generics.get(param).copied()
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
    /// WXSL in the stdlib has `@if` blocks of its own, and the render path
    /// flag ([`crate::abi::FEATURE_DEFERRED`]) is bound the same way.
    pub fn effective_macros(&self, registry: &NodeRegistry) -> Result<MacroSet, GraphErrors> {
        let (declared, mut errors) = self.declared_macros(registry);
        let mut effective = MacroSet::new();
        for (name, decl) in &declared {
            effective.set(name.clone(), decl.default);
        }
        for (name, value) in self.macros.iter() {
            if crate::wxsl::WxslIdent::new(name).is_none() {
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
            // Every generic parameter this node's definition declares must
            // be resolved for *some* instance to mean anything concrete —
            // the graph-level analogue of `MissingInput` for a mandatory
            // socket. Once `Graph::connect`/`Graph::set_generic` are the
            // only way in, this only fires for a node nothing has ever
            // touched, or a hand-edited document that bypassed both.
            for param in &def.generics {
                if !node.generics.contains_key(param.name.as_str()) {
                    errors.push(GraphError::UnresolvedGeneric {
                        node: id,
                        param: param.name.as_str().to_string(),
                    });
                }
            }
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
                    // A generic socket that is also unresolved was already
                    // reported, more precisely, by the loop above; restating
                    // it as `MissingInput` too would only be noise.
                    let unresolved_generic = socket
                        .generic
                        .as_ref()
                        .is_some_and(|param| !node.generics.contains_key(param.as_str()));
                    if !unresolved_generic {
                        errors.push(GraphError::MissingInput { socket: reference });
                    }
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
                Some(socket) => {
                    // A generic socket's declared `ty` is only a
                    // placeholder; check against this instance's resolution
                    // instead. If it has none yet, `UnresolvedGeneric`
                    // already reports the underlying problem — there is no
                    // sound expected type to compare the param against.
                    if let Some(expected) = self.effective_type(id, socket) {
                        if expected != value.ty() {
                            errors.push(GraphError::ParamTypeMismatch {
                                socket: SocketRef::new(id, name),
                                supplied: value.ty(),
                                expected,
                            });
                        }
                    }
                }
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
        // `Ok(None)` (an unresolved generic) is deliberately not unwrapped
        // into an error here: `Graph::validate`'s own `UnresolvedGeneric`
        // pass already reports it once per node, and there is nothing sound
        // to compare an unresolved type against anyway.
        let produced = match self.socket_type(registry, &edge.from, Direction::Output) {
            Ok(ty) => ty,
            Err(error) => {
                errors.push(error);
                None
            }
        };
        let expected = match self.socket_type(registry, &edge.to, Direction::Input) {
            Ok(ty) => ty,
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

    /// A socket's effective type, or `None` if it is generic and this node
    /// instance has not resolved that parameter yet.
    ///
    /// `None` is not itself an error here: [`Graph::validate`] reports an
    /// unresolved parameter once per node (see its `UnresolvedGeneric`
    /// pass) rather than once per socket that happens to reference it, and
    /// [`Graph::connect`] treats it as "free to adopt whatever the other
    /// side is" rather than a mismatch.
    fn socket_type(
        &self,
        registry: &NodeRegistry,
        socket: &SocketRef,
        direction: Direction,
    ) -> Result<Option<ValueType>, GraphError> {
        let def = self.definition(registry, socket.node)?;
        let found = match direction {
            Direction::Input => def.input(&socket.socket),
            Direction::Output => def.output(&socket.socket),
        };
        let found = found.ok_or_else(|| GraphError::UnknownSocket {
            socket: socket.clone(),
            direction,
        })?;
        Ok(self.effective_type(socket.node, found))
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
            NodeDefinition::builder("test.generic_add", "Generic add")
                .generic_param(crate::node::GenericParam::new(
                    "T",
                    vec![ValueType::F32, ValueType::Vec3],
                ))
                .input(Socket::new("a", ValueType::F32).generic("T"))
                .input(Socket::new("b", ValueType::F32).generic("T"))
                .output(Socket::new("out", ValueType::F32).generic("T"))
                .expr("{a} + {b}"),
            NodeDefinition::builder("test.generic_negate", "Generic negate")
                .generic_param(crate::node::GenericParam::new(
                    "T",
                    vec![ValueType::F32, ValueType::Vec3],
                ))
                .input(Socket::new("a", ValueType::F32).generic("T"))
                .output(Socket::new("out", ValueType::F32).generic("T"))
                .expr("-{a}"),
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

    // -- generic sockets ------------------------------------------------

    #[test]
    fn a_fresh_generic_node_is_invalid_until_something_resolves_it() {
        let registry = registry();
        let mut graph = Graph::new("g");
        graph.add_node("test.generic_add");
        let errors = graph.validate(&registry).expect_err("nothing resolved T");
        assert!(
            errors
                .0
                .iter()
                .any(|e| matches!(e, GraphError::UnresolvedGeneric { param, .. } if param == "T")),
            "{errors:?}"
        );
    }

    #[test]
    fn connecting_a_concrete_source_resolves_the_generic_target() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let source = graph.add_node("test.vec"); // vec3f out
        let add = graph.add_node("test.generic_add");
        graph
            .wire(&registry, (source, "out"), (add, "a"))
            .expect("vec3f is one of T's allowed types");
        assert_eq!(graph.generic_type(add, "T"), Some(ValueType::Vec3));

        // `b` is still unconnected, but `T` is now vec3f, so it is a
        // `MissingInput`, not still an `UnresolvedGeneric`.
        let errors = graph.validate(&registry).expect_err("b unconnected");
        assert!(matches!(
            errors.0.as_slice(),
            [GraphError::MissingInput { socket }] if socket.socket == "b"
        ));
    }

    #[test]
    fn connecting_a_concrete_target_resolves_the_generic_source() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let add = graph.add_node("test.generic_add");
        let sink = graph.add_node("test.vec");
        graph
            .wire(&registry, (add, "out"), (sink, "v"))
            .expect("vec3f is one of T's allowed types");
        assert_eq!(graph.generic_type(add, "T"), Some(ValueType::Vec3));
    }

    #[test]
    fn a_mismatched_connection_to_an_already_resolved_generic_is_rejected() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let vec_source = graph.add_node("test.vec");
        let f32_source = graph.add_node("test.const");
        let add = graph.add_node("test.generic_add");
        graph
            .wire(&registry, (vec_source, "out"), (add, "a"))
            .expect("resolves T to vec3f");

        let error = graph
            .wire(&registry, (f32_source, "out"), (add, "b"))
            .expect_err("f32 no longer matches T (vec3f)");
        assert!(matches!(error, GraphError::TypeMismatch { .. }));
        // The rejected edge must not have been added, and `a` must still be
        // the only connection.
        assert_eq!(graph.edges().len(), 1);
    }

    #[test]
    fn two_generic_nodes_wired_together_stay_polymorphic_until_anchored() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let first = graph.add_node("test.generic_negate");
        let second = graph.add_node("test.generic_negate");
        graph
            .wire(&registry, (first, "out"), (second, "a"))
            .expect("neither side is resolved yet, so nothing to check");
        assert_eq!(graph.generic_type(first, "T"), None);
        assert_eq!(graph.generic_type(second, "T"), None);

        // Anchoring the far end of the chain resolves both nodes, not just
        // the one a concrete edge touches directly.
        let source = graph.add_node("test.vec");
        graph
            .wire(&registry, (source, "out"), (first, "a"))
            .expect("vec3f is allowed");
        assert_eq!(graph.generic_type(first, "T"), Some(ValueType::Vec3));
        assert_eq!(
            graph.generic_type(second, "T"),
            Some(ValueType::Vec3),
            "resolution should have propagated across the existing edge"
        );
    }

    #[test]
    fn set_generic_rejects_a_type_the_parameter_does_not_allow() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let add = graph.add_node("test.generic_add");
        let error = graph
            .set_generic(&registry, add, "T", ValueType::Vec4)
            .expect_err("vec4f is not in T's allowed set");
        assert!(matches!(
            error,
            GraphError::InvalidGenericType {
                ty: ValueType::Vec4,
                ..
            }
        ));
    }

    #[test]
    fn set_generic_resolves_a_node_nothing_has_connected_yet() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let add = graph.add_node("test.generic_add");
        let disconnected = graph
            .set_generic(&registry, add, "T", ValueType::F32)
            .expect("f32 is allowed");
        assert!(disconnected.is_empty(), "nothing was connected to disturb");
        assert_eq!(graph.generic_type(add, "T"), Some(ValueType::F32));
    }

    #[test]
    fn repinning_disconnects_edges_that_no_longer_fit() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let vec_source = graph.add_node("test.vec");
        let add = graph.add_node("test.generic_add");
        graph
            .wire(&registry, (vec_source, "out"), (add, "a"))
            .expect("resolves T to vec3f");
        assert_eq!(graph.edges().len(), 1);

        let disconnected = graph
            .set_generic(&registry, add, "T", ValueType::F32)
            .expect("f32 is allowed");
        assert_eq!(disconnected.len(), 1, "the now-mismatched edge came back");
        assert_eq!(disconnected[0].from.node, vec_source);
        assert!(graph.edges().is_empty(), "and is gone from the graph");
        assert_eq!(graph.generic_type(add, "T"), Some(ValueType::F32));
    }

    #[test]
    fn a_resolved_generic_socket_checks_its_param_against_the_resolved_type() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let add = graph.add_node("test.generic_add");
        graph
            .set_generic(&registry, add, "T", ValueType::Vec3)
            .expect("vec3f is allowed");
        // `a`'s param is an f32, but T resolved to vec3f.
        graph.set_param(add, "a", Value::F32(1.0));
        graph.set_param(add, "b", Value::Vec3([0.0; 3]));
        let errors = graph.validate(&registry).expect_err("a is the wrong type");
        assert!(matches!(
            errors.0.as_slice(),
            [GraphError::ParamTypeMismatch {
                supplied: ValueType::F32,
                expected: ValueType::Vec3,
                ..
            }]
        ));
    }

    #[test]
    fn a_fully_resolved_and_fed_generic_node_validates_and_compiles() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let add = graph.add_node("test.generic_add");
        graph.set_param(add, "a", Value::Vec3([1.0, 2.0, 3.0]));
        graph.set_param(add, "b", Value::Vec3([4.0, 5.0, 6.0]));
        graph
            .set_generic(&registry, add, "T", ValueType::Vec3)
            .expect("vec3f is allowed");
        graph.validate(&registry).expect("fully resolved and fed");
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
