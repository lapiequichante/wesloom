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

use crate::abi;
use crate::error::{Direction, GraphError, GraphErrors};
use crate::macros::{MacroDef, MacroSet, MacroValue};
use crate::node::{self, NodeBody, NodeDefinition, NodeRegistry, Socket, Value, ValueType};
use crate::resources::{
    BufferLayout, GeometryInterface, MaterialInterface, ResourceBinding, UserBlock,
};
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
    /// String-valued settings this instance pins, by setting name. A
    /// setting the definition declares but this map does not hold falls
    /// back to [`crate::node::SettingDef::default`] — read them through
    /// [`Graph::setting`] rather than here.
    ///
    /// Unlike [`Node::label`], these *are* read by codegen: a
    /// `param.value`'s `name` is the uniform's identity. See
    /// [`crate::node::SettingDef`].
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "BTreeMap::is_empty")
    )]
    pub settings: BTreeMap<String, String>,
    /// Optional display name, overriding the definition's label.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub label: Option<String>,
    /// Optional title-strip colour, as linear RGB.
    ///
    /// A property of *this* node, not of its kind: two `math.add` nodes in
    /// one graph can be coloured differently, which is what makes colour
    /// useful for grouping a graph by what its parts are *for* rather than
    /// by what they are made of. `None` means the editor's default, which
    /// is the same for every node. Meaningless to codegen, round-tripped so
    /// it survives a save like [`Node::position`].
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub color: Option<[f32; 3]>,
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
            settings: BTreeMap::new(),
            label: None,
            color: None,
            position: None,
        }
    }

    /// Pin `value` on the input named `socket`.
    pub fn with_param(mut self, socket: impl Into<String>, value: Value) -> Self {
        self.params.insert(socket.into(), value);
        self
    }

    /// Give the node a title-strip colour of its own.
    pub fn with_color(mut self, color: [f32; 3]) -> Self {
        self.color = Some(color);
        self
    }

    /// Place the node on the editor canvas.
    pub fn with_position(mut self, position: [f32; 2]) -> Self {
        self.position = Some(position);
        self
    }

    /// Pin a string-valued setting. See [`crate::node::SettingDef`].
    pub fn with_setting(mut self, setting: impl Into<String>, value: impl Into<String>) -> Self {
        self.settings.insert(setting.into(), value.into());
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
    user_block: Option<UserBlockDecl>,
    attributes: Vec<AttributeDecl>,
    next_id: u32,
}

/// A graph's terminal nodes, which are also the roots codegen partitions
/// from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphOutputs {
    /// The surface output. Always present in a valid graph.
    pub surface: NodeId,
    /// The vertex output, if the graph has one.
    pub vertex: Option<NodeId>,
    /// The discard output, if the graph has one.
    pub discard: Option<NodeId>,
    /// One terminal per declared interpolant the graph writes, by name,
    /// in name order.
    ///
    /// A `Vec` and not an `Option`, because unlike the other three there
    /// is one of these *per declaration*: each is its own root, its own
    /// partition and its own inter-stage location.
    pub varyings: Vec<(WxslIdent, NodeId)>,
}

/// Which shader stage a partition of the graph is compiled into.
///
/// Not the same thing as a [`crate::abi::MaterialStage`], which is a whole
/// *pipeline* stage: a `forward_lit` material stage has both of these in
/// it. This is the axis a node's availability is decided on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ShaderStage {
    /// Runs once per vertex.
    Vertex,
    /// Runs once per fragment.
    Fragment,
}

/// How often an attribute's value changes: once per vertex, or once per
/// drawn instance.
///
/// The frequency decides the *backing* — a vertex buffer or a row of the
/// instance storage buffer — and nothing else. It is a property of the
/// declaration rather than of the node that reads one, so moving an
/// attribute from one to the other rewires no graph
/// ([ADR 0024](../../../docs/adr/0024-a-material-declares-the-geometry-it-requires.md)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum AttributeFrequency {
    /// One value per vertex, from a vertex buffer of its own.
    #[default]
    Vertex,
    /// One value per instance, from a field appended to the frame's
    /// instance storage buffer.
    Instance,
    /// One value the graph's *own* vertex stage computes, interpolated to
    /// the fragment stage.
    ///
    /// The only frequency the geometry does not supply, and the reason
    /// the reading node is the same either way: from the fragment side a
    /// value the mesh carried and one the vertex partition computed
    /// arrive identically, as a `@location` that was interpolated. What
    /// differs is who writes it — `abi::VARYING_OUTPUT_ID`, a terminal of
    /// its own — and that a vertex-stage node may not read one, because
    /// that is the stage computing it.
    Computed,
}

impl AttributeFrequency {
    /// Every frequency, for a UI offering a choice.
    pub const ALL: &'static [AttributeFrequency] = &[
        AttributeFrequency::Vertex,
        AttributeFrequency::Instance,
        AttributeFrequency::Computed,
    ];

    /// The name used in the serialized graph and in the editor.
    pub fn name(self) -> &'static str {
        match self {
            AttributeFrequency::Vertex => "vertex",
            AttributeFrequency::Instance => "instance",
            AttributeFrequency::Computed => "computed",
        }
    }

    /// Parse a frequency from its [`AttributeFrequency::name`].
    pub fn parse(text: &str) -> Option<Self> {
        AttributeFrequency::ALL
            .iter()
            .copied()
            .find(|frequency| frequency.name().eq_ignore_ascii_case(text.trim()))
    }
}

impl core::fmt::Display for AttributeFrequency {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.pad(self.name())
    }
}

/// One attribute a graph declares it requires of the geometry it is drawn
/// on.
///
/// A graph-level declaration, like [`UserBlockDecl`] and for the same
/// reason: it is a *requirement on somebody else*, so it has to be
/// readable without walking the nodes, and it has to stay stated even
/// while the branch that reads it is half-wired.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AttributeDecl {
    /// Its name: what a reading node names, what a mesh's stream is called,
    /// and the field name in the instance row. A valid WXSL identifier.
    pub name: String,
    /// Its type.
    pub ty: ValueType,
    /// Whether it comes per vertex or per instance.
    #[cfg_attr(feature = "serde", serde(default))]
    pub frequency: AttributeFrequency,
}

impl AttributeDecl {
    /// A per-vertex attribute.
    pub fn vertex(name: impl Into<String>, ty: ValueType) -> Self {
        AttributeDecl {
            name: name.into(),
            ty,
            frequency: AttributeFrequency::Vertex,
        }
    }

    /// A per-instance attribute.
    pub fn instance(name: impl Into<String>, ty: ValueType) -> Self {
        AttributeDecl {
            name: name.into(),
            ty,
            frequency: AttributeFrequency::Instance,
        }
    }

    /// An interpolant the graph computes in its own vertex stage.
    pub fn computed(name: impl Into<String>, ty: ValueType) -> Self {
        AttributeDecl {
            name: name.into(),
            ty,
            frequency: AttributeFrequency::Computed,
        }
    }
}

/// Why `ty` cannot be a per-vertex attribute, or `None` if it can.
///
/// Narrower than what a buffer can hold, because a vertex buffer is not a
/// buffer the shader indexes: `wgpu::VertexFormat` has float, integer and
/// normalized entries but no matrix, and an integer vertex attribute needs
/// a `@interpolate(flat)` discipline of its own. Floats and float vectors
/// are what geometry actually carries.
fn vertex_attribute_reason(ty: ValueType) -> Option<String> {
    match ty {
        ValueType::F32 | ValueType::Vec2 | ValueType::Vec3 | ValueType::Vec4 => None,
        other => Some(format!(
            "{other} cannot be a vertex buffer format; a per-vertex attribute is \
             `f32`, `vec2f`, `vec3f` or `vec4f`",
        )),
    }
}

/// The uniform block a graph declares it expects the *application* to
/// supply, in `abi::GROUP_USER`.
///
/// A graph-level declaration rather than something inferred from the nodes
/// that read it, and that is the whole point: the application already has a
/// struct with a layout of its own, and a block inferred from the two
/// fields this graph happens to read would put them at the wrong offsets.
/// The graph states the block in full, `wxsl-core` lays it out, and the
/// application binds a buffer matching what it is told
/// ([ADR 0023](../../../docs/adr/0023-a-material-declares-its-resources.md)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct UserBlockDecl {
    /// The variable name the block is bound as, and what a reading node
    /// names. Must be a valid WXSL identifier.
    pub name: String,
    /// Its fields, in declaration order — which is *not* the order they end
    /// up in the buffer. See [`crate::resources::BufferLayout`].
    pub fields: Vec<UserField>,
}

/// One field of a [`UserBlockDecl`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct UserField {
    /// The field's name.
    pub name: String,
    /// Its type. A resource type is rejected by [`Graph::validate`]: this
    /// is one uniform buffer, not a bind group the graph designs.
    pub ty: ValueType,
}

impl UserField {
    /// Declare a field.
    pub fn new(name: impl Into<String>, ty: ValueType) -> Self {
        UserField {
            name: name.into(),
            ty,
        }
    }
}

/// Why a declared name cannot be used, or `None` if it can.
///
/// The generated module owns two shapes of name: the handful in
/// [`abi::RESERVED_NAMES`], and `n<id>_<socket>`, which is how every node
/// output is bound. A texture called `n1_out` would shadow one.
fn reserved_reason(name: &str) -> Option<String> {
    if abi::RESERVED_NAMES.contains(&name) {
        return Some(format!(
            "is a name the generated module uses for itself (reserved: {})",
            abi::RESERVED_NAMES.join(", ")
        ));
    }
    let rest = name.strip_prefix('n')?;
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    (digits > 0 && rest[digits..].starts_with('_'))
        .then(|| "looks like a generated node binding (`n<number>_<socket>`)".to_string())
}

/// A socket's effective type given a specific resolution map, rather than
/// necessarily the graph's *current* one for its node.
///
/// Split out from [`Graph::effective_type`] so [`Graph::set_generic`] can ask
/// "what would this socket become if `param` were repinned to `ty`, before
/// actually repinning it" — against a hypothetical map with that one change
/// — to decide whether a downstream combined socket's connection would
/// break, the same way it already does for a socket [`Socket::generic`]
/// governs directly.
fn effective_type_of(socket: &Socket, generics: &BTreeMap<String, ValueType>) -> Option<ValueType> {
    if let Some(param) = &socket.generic {
        return generics.get(param.as_str()).copied();
    }
    if let Some(combined) = &socket.combine {
        let a = *generics.get(combined.a.as_str())?;
        let b = *generics.get(combined.b.as_str())?;
        return combined.rule.apply(a, b);
    }
    Some(socket.ty)
}

impl Graph {
    /// An empty graph.
    pub fn new(name: impl Into<String>) -> Self {
        Graph {
            name: name.into(),
            nodes: BTreeMap::new(),
            edges: Vec::new(),
            macros: MacroSet::new(),
            user_block: None,
            attributes: Vec::new(),
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

    /// Add `node`, resolved and fed: every generic parameter its definition
    /// declares takes its [`NodeDefinition::default_generics`] type, and
    /// every input with no default of its own gets a value pinned.
    ///
    /// This is what an editor should use to place a node, and
    /// [`Graph::add`] is what a deserializer and a test fixture use. A node
    /// dropped on a canvas with nothing resolved reports
    /// [`GraphError::UnresolvedGeneric`] and shows inputs with no value to
    /// edit, which is a node complaining before it has been used; picking a
    /// default type is a guess, but it is a guess anything else overrides —
    /// connecting a wire retypes it ([`Graph::plan_retype`]) and so does
    /// picking a type ([`Graph::set_generic`]).
    pub fn add_resolved(&mut self, registry: &NodeRegistry, node: Node) -> NodeId {
        let id = self.add(node);
        if let Ok(def) = self.definition(registry, id) {
            let defaults = def.default_generics();
            if let Some(instance) = self.nodes.get_mut(&id) {
                for (param, ty) in defaults {
                    instance.generics.entry(param).or_insert(ty);
                }
            }
        }
        self.retype_params(registry, id);
        id
    }

    /// Remove a node and every edge touching it.
    ///
    /// A surviving node on the other end of one of those edges forgets a
    /// generic resolution the same way [`Graph::disconnect`] would, if that
    /// was the edge keeping it constrained — see
    /// [`Graph::refresh_generics`].
    ///
    /// Returns the removed node, or `None` if there was no such node.
    pub fn remove_node(&mut self, registry: &NodeRegistry, id: NodeId) -> Option<Node> {
        let node = self.nodes.remove(&id)?;
        let surviving: Vec<NodeId> = self
            .edges
            .iter()
            .filter(|edge| edge.from.node == id || edge.to.node == id)
            .filter_map(|edge| match (edge.from.node == id, edge.to.node == id) {
                (false, _) => Some(edge.from.node),
                (_, false) => Some(edge.to.node),
                // An edge from `id` to itself: both ends are gone.
                (true, true) => None,
            })
            .collect();
        self.edges
            .retain(|edge| edge.from.node != id && edge.to.node != id);
        for survivor in surviving {
            self.refresh_generics(registry, survivor);
        }
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

    /// The block this graph expects the application to supply, if it
    /// declares one. See [`UserBlockDecl`].
    pub fn user_block(&self) -> Option<&UserBlockDecl> {
        self.user_block.as_ref()
    }

    /// Declare the block the application must supply, replacing any
    /// previous declaration.
    pub fn set_user_block(&mut self, block: UserBlockDecl) {
        self.user_block = Some(block);
    }

    /// Stop expecting a block from the application.
    pub fn clear_user_block(&mut self) -> Option<UserBlockDecl> {
        self.user_block.take()
    }

    /// The effective value of `node`'s setting `name`: what the instance
    /// pinned, or the definition's default.
    ///
    /// `None` means the definition declares no such setting at all, which
    /// is a different thing from a setting left empty.
    pub fn setting<'a>(
        &'a self,
        registry: &'a NodeRegistry,
        node: NodeId,
        name: &str,
    ) -> Option<&'a str> {
        let instance = self.nodes.get(&node)?;
        let declared = registry.get(&instance.def)?.setting(name)?;
        Some(
            instance
                .settings
                .get(name)
                .map(String::as_str)
                .unwrap_or(declared.default.as_str()),
        )
    }

    /// What this graph requires of the geometry it is drawn on.
    ///
    /// Declared here rather than inferred from the `input.attribute` nodes
    /// that read them, for the reason [`UserBlockDecl`] is: the
    /// declaration is a *contract with the geometry*, and a contract that
    /// appeared and vanished as a branch was wired up would be no contract
    /// at all. It is also what says whether a name is per-vertex or per-
    /// instance, which the reading node deliberately does not.
    pub fn attributes(&self) -> &[AttributeDecl] {
        &self.attributes
    }

    /// Look one up by name.
    pub fn attribute(&self, name: &str) -> Option<&AttributeDecl> {
        let name = name.trim();
        self.attributes.iter().find(|decl| decl.name.trim() == name)
    }

    /// Declare an attribute, replacing any declaration of the same name.
    pub fn declare_attribute(&mut self, decl: AttributeDecl) {
        let name = decl.name.trim().to_string();
        self.attributes.retain(|held| held.name.trim() != name);
        self.attributes.push(decl);
    }

    /// Undeclare `name`, returning what was there.
    ///
    /// A node still reading it becomes invalid, which is
    /// [`GraphError::UnknownAttribute`] and exactly the report wanted:
    /// removing a declaration should say what it broke rather than
    /// silently deleting the nodes.
    pub fn remove_attribute(&mut self, name: &str) -> Option<AttributeDecl> {
        let name = name.trim();
        let index = self
            .attributes
            .iter()
            .position(|decl| decl.name.trim() == name)?;
        Some(self.attributes.remove(index))
    }

    /// Replace the whole declared set.
    pub fn set_attributes(&mut self, attributes: Vec<AttributeDecl>) {
        self.attributes = attributes;
    }

    /// Pin `node`'s setting `name`. Unlike [`Graph::setting`] this does not
    /// check the definition declares it — [`Graph::validate`] reports a
    /// setting that matches nothing.
    pub fn set_setting(&mut self, node: NodeId, name: impl Into<String>, value: impl Into<String>) {
        if let Some(instance) = self.nodes.get_mut(&node) {
            instance.settings.insert(name.into(), value.into());
        }
    }

    /// What this graph needs bound before it can draw: its uniform
    /// parameters, its textures and samplers, and the block it expects the
    /// application to supply.
    ///
    /// Computed over the nodes the surface output actually depends on, so
    /// a parked branch declares nothing — the same reachability codegen
    /// uses when it decides what to emit, and it has to be the same or the
    /// bind group and the shader would disagree about what is bound.
    ///
    /// A graph with no output node, or more than one, has no interface:
    /// [`crate::codegen::generate`] is where that is reported, and this
    /// answers an empty interface rather than duplicating the error.
    pub fn interface(&self, registry: &NodeRegistry) -> MaterialInterface {
        let Some(outputs) = self.outputs(registry) else {
            return MaterialInterface::default();
        };
        self.interface_of(registry, &self.reachable_from(&outputs))
    }

    /// Every node any of `outputs` depends on.
    ///
    /// The union across every terminal, not one stage's partition.
    /// The interface is per *material*: a bind group layout that narrowed
    /// per stage would mean a depth pass and a shading pass wanted
    /// different bind groups for the same object, and a pipeline layout
    /// may be a superset of what its shader uses anyway.
    pub fn reachable_from(&self, outputs: &GraphOutputs) -> BTreeSet<NodeId> {
        let mut reachable = self.dependencies_of(outputs.surface);
        let extras = [outputs.vertex, outputs.discard]
            .into_iter()
            .flatten()
            .chain(outputs.varyings.iter().map(|(_, node)| *node));
        for extra in extras {
            reachable.extend(self.dependencies_of(extra));
        }
        reachable
    }

    /// [`Graph::interface`] over an already-computed reachable set — what
    /// codegen calls, so the two cannot disagree about what "reachable"
    /// meant.
    pub fn interface_of(
        &self,
        registry: &NodeRegistry,
        reachable: &BTreeSet<NodeId>,
    ) -> MaterialInterface {
        let mut params: Vec<(WxslIdent, ValueType)> = Vec::new();
        let mut defaults: BTreeMap<String, Value> = BTreeMap::new();
        let mut resources: Vec<(WxslIdent, ValueType)> = Vec::new();
        let mut reads_user = false;

        for &id in reachable {
            let Some(node) = self.nodes.get(&id) else {
                continue;
            };
            let Some(def) = registry.get(&node.def) else {
                continue;
            };
            match def.body {
                NodeBody::Param => {
                    let (Some(name), Some(ty)) = (
                        self.declared_name(registry, id, node::SETTING_NAME),
                        def.outputs
                            .first()
                            .and_then(|socket| self.effective_type(id, socket)),
                    ) else {
                        continue;
                    };
                    // The `value` socket is the parameter's starting
                    // value: a pinned literal, never an edge (see
                    // `Socket::constant`). First declaration wins, as it
                    // does for the type.
                    if !params.iter().any(|(existing, _)| *existing == name) {
                        if let Some(value) = def
                            .input(node::SOCKET_VALUE)
                            .and_then(|socket| {
                                node.params
                                    .get(node::SOCKET_VALUE)
                                    .copied()
                                    .or_else(|| socket.default_for(ty))
                            })
                            .filter(|value| value.ty() == ty)
                        {
                            defaults.insert(name.as_str().to_string(), value);
                        }
                        params.push((name, ty));
                    }
                }
                NodeBody::Resource => {
                    let (Some(name), Some(socket)) = (
                        self.declared_name(registry, id, node::SETTING_NAME),
                        def.outputs.first(),
                    ) else {
                        continue;
                    };
                    if !resources.iter().any(|(existing, _)| *existing == name) {
                        resources.push((name, socket.ty));
                    }
                }
                NodeBody::UserRead => reads_user = true,
                _ => {}
            }
        }

        // Name order, so adding a texture cannot renumber the ones already
        // there — a binding index that moves is a bind group that has to
        // be rebuilt for no reason.
        resources.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        let resources = resources
            .into_iter()
            .enumerate()
            .map(|(index, (name, ty))| ResourceBinding {
                name,
                ty,
                binding: abi::MATERIAL_RESOURCE_BINDING_BASE + index as u32,
            })
            .collect();

        // Declared, not inferred, and *not* narrowed to what is
        // reachable either — unlike a parameter. The instance buffer is
        // uploaded once for the whole frame and serves every stage, so a
        // stride that depended on which nodes a stage happens to reach
        // would be a different buffer per pass. Narrowing per stage is
        // M5's partitioning, with a general mechanism.
        let geometry = GeometryInterface::new(
            self.declared_attributes(AttributeFrequency::Vertex),
            self.declared_attributes(AttributeFrequency::Instance),
            self.declared_attributes(AttributeFrequency::Computed),
        );

        // Declared, not inferred: the whole block is bound whether the
        // graph reads one field of it or all of them, because the
        // application's buffer has the layout it has.
        let user = reads_user
            .then_some(self.user_block.as_ref())
            .flatten()
            .and_then(|decl| {
                let name = WxslIdent::new(&decl.name)?;
                let fields = decl
                    .fields
                    .iter()
                    .filter_map(|field| Some((WxslIdent::new(&field.name)?, field.ty)));
                Some(UserBlock {
                    struct_name: UserBlock::struct_name_for(&name),
                    name,
                    layout: BufferLayout::uniform(fields),
                })
            });

        MaterialInterface {
            params: BufferLayout::uniform(params),
            defaults,
            resources,
            user,
            geometry,
        }
    }

    /// The declared attributes of one frequency, as the layout computer
    /// wants them. Malformed names are skipped; `Graph::validate` reports
    /// them.
    fn declared_attributes(&self, frequency: AttributeFrequency) -> Vec<(WxslIdent, ValueType)> {
        self.attributes
            .iter()
            .filter(|decl| decl.frequency == frequency)
            .filter_map(|decl| Some((WxslIdent::new(decl.name.trim())?, decl.ty)))
            .collect()
    }

    /// The identifier a declaring node's `setting` names, or `None` when it
    /// is empty or not a usable WXSL name — both of which
    /// [`Graph::validate`] reports, so this can stay quiet.
    fn declared_name(
        &self,
        registry: &NodeRegistry,
        node: NodeId,
        setting: &str,
    ) -> Option<WxslIdent> {
        WxslIdent::new(self.setting(registry, node, setting)?.trim())
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
        let produced = self.effective_type(from.node, from_socket);

        let to_def = self.definition(registry, to.node)?;
        let to_socket = to_def
            .input(&to.socket)
            .ok_or_else(|| GraphError::UnknownSocket {
                socket: to.clone(),
                direction: Direction::Input,
            })?;
        if to_socket.constant {
            // Not a value flowing in but a property of the node: a
            // `param.value`'s default is read on the CPU before any shader
            // runs, so there is nothing an edge into it could mean.
            return Err(GraphError::ConstantInput { socket: to });
        }
        let expected = self.effective_type(to.node, to_socket);

        // The generic parameters this connection has to change for the two
        // ends to agree, if any. A resolution is a *default* until wiring
        // backs it — it came from `NodeDefinition::default_generics`, from
        // an explicit pick, or from `Graph::seedable_companions` — so
        // dragging a wire onto a socket retypes the node where it can
        // rather than being refused, which is the mirror image of
        // `Graph::set_generic` disconnecting edges that no longer fit
        // instead of refusing the repin. `Graph::plan_retype` is where
        // "where it can" is decided: it refuses to break an edge that
        // already exists, and a conflict with one of those is still a
        // `TypeMismatch`.
        //
        // The target is tried first: dropping a wire onto an input is a
        // statement about that input, and the source is usually the
        // established side.
        let mut plan: Vec<(NodeId, String, ValueType)> = Vec::new();
        let mut retype = |graph: &Self, node, def, socket, wanted| {
            graph
                .plan_retype(registry, node, def, socket, wanted)
                .map(|changes| {
                    plan.extend(changes.into_iter().map(|(param, ty)| (node, param, ty)));
                })
        };
        match (produced, expected) {
            (Some(p), Some(e)) if p == e => {}
            (Some(p), Some(e)) => {
                if retype(self, to.node, to_def, to_socket, p)
                    .or_else(|| retype(self, from.node, from_def, from_socket, e))
                    .is_none()
                {
                    return Err(GraphError::TypeMismatch {
                        from,
                        to,
                        produced: p,
                        expected: e,
                    });
                }
            }
            // One side has no type yet, which is not a mismatch: it is a
            // combined socket whose parameters are unresolved, or a generic
            // one nothing has pinned. Retyping the unresolved side to match
            // is the adoption that makes a socket "take whatever it
            // receives"; where there is nothing to adopt into, the edge is
            // still valid and stays unresolved until whatever it derives
            // from resolves.
            (Some(p), None) => {
                retype(self, to.node, to_def, to_socket, p);
            }
            (None, Some(e)) => {
                retype(self, from.node, from_def, from_socket, e);
            }
            // Neither side is resolved: nothing to check or adopt. A chain
            // of unconstrained generics stays polymorphic until something
            // anchors it; `Graph::validate` reports the whole chain as
            // unresolved until it does.
            (None, None) => {}
        }

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
        for (node, param, ty) in plan {
            self.resolve_generic(registry, node, &param, ty);
        }
        Ok(())
    }

    /// A socket's effective type: `socket.ty` when it is neither generic nor
    /// combined, this node instance's resolution of [`Socket::generic`], or
    /// [`Socket::combine`]'s rule applied to two resolutions — `None` if
    /// whichever of those applies has not been resolved yet (or, for a
    /// combined socket, both are resolved but do not combine).
    ///
    /// A method rather than a free function on [`Socket`] because resolution
    /// is per graph *node*, not something a bare `Socket` (shared across
    /// every instance of its [`NodeDefinition`]) can answer alone. `codegen`
    /// is the other caller within this crate: once [`Graph::validate`] has
    /// passed, every generic or combined socket codegen touches resolves to
    /// `Some`. Public for the editor, which needs the same answer to colour
    /// a port or label an inspector row by what a socket actually carries,
    /// not the placeholder `socket.ty` shown before any instance resolved it.
    pub fn effective_type(&self, node: NodeId, socket: &Socket) -> Option<ValueType> {
        effective_type_of(socket, &self.nodes.get(&node)?.generics)
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

            for companion in self.seedable_companions(&def, node, &param, ty) {
                queue.push_back((node, companion));
            }
            self.retype_params(registry, node);
        }
    }

    /// The change to `node`'s generic parameters that would make `socket`
    /// carry `wanted`, or `None` if there is no such change.
    ///
    /// This is what makes "plug an `f32` into a socket showing `vec3f`"
    /// retype the node instead of being refused. A socket's type is either
    /// fixed (nothing to retype), one parameter (set it), or two combined
    /// by a [`TypeRule`] (set whichever of them makes the rule give
    /// `wanted` — one alone where that suffices, both otherwise).
    ///
    /// A plan is only returned when it breaks nothing: every parameter it
    /// touches must allow the type, every [`Socket::combine`] socket on the
    /// node must still derive a type, and every edge already touching the
    /// node must still join two sockets of the same type. That last
    /// condition is the whole difference between retyping and vandalism —
    /// the node adapts where it can, and a genuine conflict with wiring
    /// that already exists is still [`GraphError::TypeMismatch`], not a
    /// wire silently dropped.
    fn plan_retype(
        &self,
        registry: &NodeRegistry,
        node: NodeId,
        def: &NodeDefinition,
        socket: &Socket,
        wanted: ValueType,
    ) -> Option<Vec<(String, ValueType)>> {
        let candidates: Vec<Vec<String>> = if let Some(param) = &socket.generic {
            vec![vec![param.as_str().to_string()]]
        } else if let Some(combined) = &socket.combine {
            let (a, b) = (
                combined.a.as_str().to_string(),
                combined.b.as_str().to_string(),
            );
            vec![vec![a.clone()], vec![b.clone()], vec![a, b]]
        } else {
            return None;
        };

        let current = self
            .nodes
            .get(&node)
            .map(|instance| instance.generics.clone())
            .unwrap_or_default();
        candidates.into_iter().find_map(|params| {
            if !params.iter().all(|param| {
                def.generic(param)
                    .is_some_and(|declared| declared.allowed.contains(&wanted))
            }) {
                return None;
            }
            let mut hypothetical = current.clone();
            for param in &params {
                hypothetical.insert(param.clone(), wanted);
            }
            if effective_type_of(socket, &hypothetical) != Some(wanted) {
                return None;
            }
            self.hypothesis_holds(registry, node, def, &hypothetical)
                .then(|| params.into_iter().map(|param| (param, wanted)).collect())
        })
    }

    /// Whether `hypothetical` is a resolution `node` could actually take:
    /// no combined socket is left with two types its rule cannot combine,
    /// and every edge already touching the node still joins two sockets of
    /// the same type.
    ///
    /// Neither check treats "not resolved yet" as a conflict. A combined
    /// socket with only one of its two parameters known is simply not
    /// derived yet, and an edge whose *other* end is unresolved is one
    /// [`Graph::resolve_generic`] propagates into — which is how a chain of
    /// generic nodes resolves together.
    fn hypothesis_holds(
        &self,
        registry: &NodeRegistry,
        node: NodeId,
        def: &NodeDefinition,
        hypothetical: &BTreeMap<String, ValueType>,
    ) -> bool {
        for socket in def.inputs.iter().chain(&def.outputs) {
            let Some(combined) = &socket.combine else {
                continue;
            };
            let (Some(&a), Some(&b)) = (
                hypothetical.get(combined.a.as_str()),
                hypothetical.get(combined.b.as_str()),
            ) else {
                continue;
            };
            if combined.rule.apply(a, b).is_none() {
                return false;
            }
        }
        for edge in &self.edges {
            let (mine, theirs, direction) = if edge.to.node == node {
                (def.input(&edge.to.socket), &edge.from, Direction::Output)
            } else if edge.from.node == node {
                (def.output(&edge.from.socket), &edge.to, Direction::Input)
            } else {
                continue;
            };
            let Some(mine) = mine else { continue };
            let (Some(becomes), Ok(Some(other))) = (
                effective_type_of(mine, hypothetical),
                self.socket_type(registry, theirs, direction),
            ) else {
                continue;
            };
            if becomes != other {
                return false;
            }
        }
        true
    }

    /// Bring `node`'s pinned parameters in line with its resolved types:
    /// convert any whose type no longer matches its socket, and pin one on
    /// every unconnected input that would otherwise have no value at all.
    ///
    /// Both halves exist so that resolving a type leaves a node *usable*
    /// rather than merely typed. A stale parameter is
    /// [`GraphError::ParamTypeMismatch`] — picking `vec3f` on a
    /// `math.add` whose operands were pinned to `0.5` has to carry them to
    /// `vec3f(0.5)`, not report an error — and a generic input has no fixed
    /// default to fall back on ([`Socket::default`] is always `None`
    /// there), so without a pinned value it is
    /// [`GraphError::MissingInput`] and the inspector has nothing to show.
    /// Only inputs nothing feeds get a value; a connected one keeps
    /// whatever the author last typed, converted, for when it is unplugged.
    fn retype_params(&mut self, registry: &NodeRegistry, node: NodeId) {
        let Ok(def) = self.definition(registry, node).cloned() else {
            return;
        };
        let mut updates: Vec<(String, Value)> = Vec::new();
        for socket in &def.inputs {
            let Some(ty) = self.effective_type(node, socket) else {
                continue;
            };
            let name = socket.name.as_str();
            let pinned = self
                .nodes
                .get(&node)
                .and_then(|n| n.params.get(name))
                .copied();
            match pinned {
                Some(value) if value.ty() != ty => {
                    if let Some(converted) = value.converted_to(ty) {
                        updates.push((name.to_string(), converted));
                    }
                }
                Some(_) => {}
                None => {
                    let fed = socket.default_for(ty).is_some()
                        || socket.optional
                        || self.edge_into(&SocketRef::new(node, name)).is_some();
                    if !fed {
                        if let Some(value) = ty.splat(0.0).or_else(|| ty.zero()) {
                            updates.push((name.to_string(), value));
                        }
                    }
                }
            }
        }
        if let Some(instance) = self.nodes.get_mut(&node) {
            for (name, value) in updates {
                instance.params.insert(name, value);
            }
        }
    }

    /// The other parameters on `node` that resolving `param` to `ty` should
    /// carry along: for every [`Socket::combine`] socket reading `param`,
    /// the parameter on its other side.
    ///
    /// This is what keeps a two-parameter node usable from one connection.
    /// `math.multiply` declares `A` and `B` so that `f32 * vec3f` is
    /// expressible at all, but the overwhelmingly common case is two
    /// operands of the *same* type — so wiring a `vec3f` into `a` resolves
    /// `B` to `vec3f` as well, and the node is immediately complete instead
    /// of sitting on an unresolved `B` until the second operand is touched
    /// too. It is a default, not a constraint: pinning `B` afterwards
    /// ([`Graph::set_generic`]) or wiring something else into `b` changes it
    /// like any other resolution.
    ///
    /// A companion is only seeded when nothing else has a say — it is
    /// unresolved, `ty` is one of the types it allows, and *no* remaining
    /// edge touches a socket that reads it. That last condition is what
    /// keeps a guess from overriding evidence: if `b` is already wired to a
    /// chain of not-yet-anchored generic nodes, the type that chain
    /// eventually adopts decides `B`, not `A`.
    fn seedable_companions(
        &self,
        def: &NodeDefinition,
        node: NodeId,
        param: &str,
        ty: ValueType,
    ) -> Vec<String> {
        let mut companions: Vec<String> = Vec::new();
        for socket in def.inputs.iter().chain(&def.outputs) {
            let Some(combined) = &socket.combine else {
                continue;
            };
            let other = match (combined.a.as_str(), combined.b.as_str()) {
                (a, b) if a == param => b,
                (a, b) if b == param => a,
                _ => continue,
            };
            if other == param
                || companions.iter().any(|seen| seen == other)
                || self.generic_type(node, other).is_some()
                || !def
                    .generic(other)
                    .is_some_and(|declared| declared.allowed.contains(&ty))
                || self.param_has_an_edge(def, node, other)
            {
                continue;
            }
            companions.push(other.to_string());
        }
        companions
    }

    /// Whether any edge on `node` touches a socket whose effective type
    /// reads generic parameter `param` — the test for "something other than
    /// a guess has a say in what this parameter is".
    ///
    /// A [`Socket::combine`] socket counts: an edge on `multiply`'s output
    /// constrains `A` and `B` jointly (whatever they become, the rule
    /// applied to them has to stay equal to the other end's type) even
    /// though it cannot resolve either one by itself.
    fn param_has_an_edge(&self, def: &NodeDefinition, node: NodeId, param: &str) -> bool {
        let reads_param = |socket: &&Socket| {
            socket
                .referenced_params()
                .any(|name| name.as_str() == param)
        };
        def.inputs.iter().filter(reads_param).any(|socket| {
            self.edge_into(&SocketRef::new(node, socket.name.as_str()))
                .is_some()
        }) || def.outputs.iter().filter(reads_param).any(|socket| {
            self.edge_from(&SocketRef::new(node, socket.name.as_str()))
                .is_some()
        })
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

        // Every socket this repin touches: directly (`Socket::generic` names
        // `param`) or as one of the two operands a combined socket derives
        // from (`Socket::combine` names `param` as either one) — repinning
        // `A` can just as well break `multiply`'s output as it can break
        // `a` itself.
        let references_param = |socket: &&Socket| {
            socket
                .referenced_params()
                .any(|name| name.as_str() == param)
        };
        // What every referencing socket would resolve to *after* the repin,
        // not before — a hypothetical map with only `param` changed, so a
        // combined socket's other parameter (still whatever it already was)
        // is combined against the *new* value being pinned.
        let mut hypothetical = self
            .nodes
            .get(&node)
            .map(|instance| instance.generics.clone())
            .unwrap_or_default();
        hypothetical.insert(param.to_string(), ty);

        // Every input this repin would leave mismatched: an existing edge
        // whose *other* end's effective type is no longer what this socket
        // would become. Collected before mutating anything, since
        // disconnecting is decided against the state before the repin.
        let mut mismatched = Vec::new();
        for socket in def.inputs.iter().filter(references_param) {
            let reference = SocketRef::new(node, socket.name.as_str());
            if let Some(edge) = self.edge_into(&reference) {
                let other = self
                    .socket_type(registry, &edge.from, Direction::Output)
                    .unwrap_or(None);
                if other != effective_type_of(socket, &hypothetical) {
                    mismatched.push(reference);
                }
            }
        }
        for socket in def.outputs.iter().filter(references_param) {
            for edge in self
                .edges_from(node)
                .filter(|edge| edge.from.socket == socket.name.as_str())
            {
                let other = self
                    .socket_type(registry, &edge.to, Direction::Input)
                    .unwrap_or(None);
                if other != effective_type_of(socket, &hypothetical) {
                    mismatched.push(edge.to.clone());
                }
            }
        }

        let disconnected = mismatched
            .into_iter()
            .filter_map(|input| self.disconnect(registry, &input))
            .collect();

        self.nodes
            .get_mut(&node)
            .expect("checked to exist by `definition` above")
            .generics
            .insert(param.to_string(), ty);
        // Picking one of `multiply`'s two operand types by hand should leave
        // the node usable, exactly as wiring one of them does — see
        // `Graph::seedable_companions` and `Graph::retype_params`.
        for companion in self.seedable_companions(&def, node, param, ty) {
            self.resolve_generic(registry, node, &companion, ty);
        }
        self.retype_params(registry, node);
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
    ///
    /// If that edge was the only thing keeping one of its two ends'
    /// [`Socket::generic`] parameters constrained, that resolution is
    /// forgotten too — otherwise a node that adopted, say, `f32` from a
    /// since-removed wire would refuse a `vec3f` one next, even with
    /// nothing at all connected to tell it not to. See
    /// [`Graph::refresh_generics`] for exactly what "constrained"
    /// means and the one case (a chain of generic nodes) it deliberately
    /// does not chase through.
    pub fn disconnect(&mut self, registry: &NodeRegistry, input: &SocketRef) -> Option<Edge> {
        let index = self.edges.iter().position(|edge| &edge.to == input)?;
        let edge = self.edges.remove(index);
        self.refresh_generics(registry, edge.to.node);
        self.refresh_generics(registry, edge.from.node);
        Some(edge)
    }

    /// Bring `node`'s generic resolutions back in line with its remaining
    /// edges, after one was removed: forget every parameter no remaining
    /// edge has a say in — falling back to
    /// [`NodeDefinition::default_generics`] rather than to nothing — then
    /// re-seed the companions of those that survived.
    ///
    /// Forgetting is what stops a node that adopted, say, `vec3f` from a
    /// since-removed wire from staying `vec3f` with nothing connected to
    /// say so; unwiring a node completely returns it to the type it had
    /// when it was placed, not to no type at all — an unresolved parameter
    /// is an error, and a node nobody has broken should not report one.
    /// Retyping on the next connection is what makes that safe, so the
    /// default is never a trap (see [`Graph::plan_retype`]). Re-seeding is
    /// what stops the
    /// forgetting from undoing a companion resolution that is still earned:
    /// unplugging `multiply`'s output leaves `a` wired and `A` resolved, and
    /// `B` — seeded from `A` in the first place — should simply be seeded
    /// again rather than left blank. See [`Graph::seedable_companions`].
    ///
    /// Deliberately local rather than transitive: two generic nodes wired
    /// together, anchored by a third concrete one, keep whatever they
    /// resolved to if the edge removed is between the *two generic* nodes
    /// (each still touches the other), even though neither is connected to
    /// the concrete anchor anymore once that edge specifically is the one
    /// that goes. A full re-derivation would need a reachability search over
    /// every remaining edge on every disconnect; this covers the case that
    /// actually gets reported (a node with no remaining connections at all
    /// stuck on a stale resolution) without paying for that.
    fn refresh_generics(&mut self, registry: &NodeRegistry, node: NodeId) {
        let Ok(def) = self.definition(registry, node).cloned() else {
            return;
        };
        let kept: Vec<(String, ValueType)> = def
            .generics
            .iter()
            .filter(|param| self.param_has_an_edge(&def, node, param.name.as_str()))
            .filter_map(|param| {
                self.generic_type(node, param.name.as_str())
                    .map(|ty| (param.name.as_str().to_string(), ty))
            })
            .collect();
        let Some(instance) = self.nodes.get_mut(&node) else {
            return;
        };
        instance
            .generics
            .retain(|name, _| kept.iter().any(|(survivor, _)| survivor == name));
        for (param, ty) in kept {
            for companion in self.seedable_companions(&def, node, &param, ty) {
                self.resolve_generic(registry, node, &companion, ty);
            }
        }
        // Whatever neither an edge nor a seed accounts for falls back to the
        // default, so a node the author has merely unplugged is never left
        // reporting an unresolved parameter. Ordered after the re-seeding so
        // a default never pre-empts a type something still asks for.
        let defaults = def.default_generics();
        if let Some(instance) = self.nodes.get_mut(&node) {
            for (param, ty) in defaults {
                instance.generics.entry(param).or_insert(ty);
            }
        }
        self.retype_params(registry, node);
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
    /// values are bound the same way.
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
        self.terminals(registry, NodeDefinition::is_surface_output)
    }

    /// Every vertex-output node. At most one is valid.
    pub fn vertex_outputs(&self, registry: &NodeRegistry) -> Vec<NodeId> {
        self.terminals(registry, NodeDefinition::is_vertex_output)
    }

    /// Every discard-output node. At most one is valid.
    pub fn discard_outputs(&self, registry: &NodeRegistry) -> Vec<NodeId> {
        self.terminals(registry, NodeDefinition::is_discard_output)
    }

    /// Every node writing a declared interpolant, whether or not the name
    /// it points at exists.
    pub fn varying_outputs(&self, registry: &NodeRegistry) -> Vec<NodeId> {
        self.terminals(registry, NodeDefinition::is_varying_output)
    }

    fn terminals(
        &self,
        registry: &NodeRegistry,
        is_kind: fn(&NodeDefinition) -> bool,
    ) -> Vec<NodeId> {
        self.nodes()
            .filter(|(_, node)| registry.get(&node.def).is_some_and(|def| is_kind(def)))
            .map(|(id, _)| id)
            .collect()
    }

    /// The graph's three terminals: the surface, and the optional vertex
    /// and discard outputs.
    ///
    /// What partitioning starts from. Each is a separate root, and a node
    /// reachable from two of them is compiled into both — which is the
    /// whole of what "a graph spans shader stages" means
    /// ([ADR 0025](../../../docs/adr/0025-a-material-graph-spans-shader-stages.md)).
    ///
    /// Answers `None` when the surface output is missing or duplicated;
    /// [`crate::codegen::generate`] is where that is reported.
    pub fn outputs(&self, registry: &NodeRegistry) -> Option<GraphOutputs> {
        let surfaces = self.surface_outputs(registry);
        let &surface = surfaces.first().filter(|_| surfaces.len() == 1)?;
        let one = |found: Vec<NodeId>| found.first().copied().filter(|_| found.len() == 1);
        // In name order, and only for names the graph actually
        // declared as computed: a writer naming something else is
        // reported by `check_outputs` rather than silently compiled.
        let mut varyings: Vec<(WxslIdent, NodeId)> = self
            .varying_outputs(registry)
            .into_iter()
            .filter_map(|node| {
                let name = self.declared_name(registry, node, node::SETTING_NAME)?;
                let decl = self.attribute(name.as_str())?;
                (decl.frequency == AttributeFrequency::Computed).then_some((name, node))
            })
            .collect();
        varyings.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        varyings.dedup_by(|(a, _), (b, _)| a == b);
        Some(GraphOutputs {
            surface,
            vertex: one(self.vertex_outputs(registry)),
            discard: one(self.discard_outputs(registry)),
            varyings,
        })
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
            // A combined socket (`Socket::combine`) whose two parameters
            // are both resolved, but to types its rule does not combine — e.g.
            // `multiply` fed `vec2f` and `vec3f`. Skipped when either
            // parameter is still unresolved: that is already reported above,
            // more precisely, and there is nothing sound to combine yet.
            for socket in def.inputs.iter().chain(&def.outputs) {
                let Some(combined) = &socket.combine else {
                    continue;
                };
                let (Some(&a_ty), Some(&b_ty)) = (
                    node.generics.get(combined.a.as_str()),
                    node.generics.get(combined.b.as_str()),
                ) else {
                    continue;
                };
                if combined.rule.apply(a_ty, b_ty).is_none() {
                    errors.push(GraphError::IncompatibleGenerics {
                        node: id,
                        socket: socket.name.as_str().to_string(),
                        rule: combined.rule,
                        a: (combined.a.as_str().to_string(), a_ty),
                        b: (combined.b.as_str().to_string(), b_ty),
                    });
                }
            }
        }

        self.check_declarations(registry, &mut errors);

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
                    || self
                        .effective_type(id, socket)
                        .and_then(|ty| socket.default_for(ty))
                        .is_some()
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

    /// Check everything a graph *declares* rather than computes: node
    /// settings, the names they carry, and the application block.
    ///
    /// Over every node rather than only the reachable ones, like the rest
    /// of validation — an unreachable node with a broken declaration is
    /// still broken, and it will be reachable the moment it is wired up.
    /// A freshly dropped declaring node is valid, because its setting
    /// carries a usable default.
    fn check_declarations(&self, registry: &NodeRegistry, errors: &mut Vec<GraphError>) {
        self.check_user_block(errors);
        self.check_attributes(errors);
        self.check_outputs(registry, errors);
        // One name is one binding, so two nodes naming the same parameter
        // are the same parameter — which is a feature, as long as they
        // agree about its type.
        let mut declared: BTreeMap<String, ValueType> = BTreeMap::new();

        for (id, instance) in self.nodes() {
            let Some(def) = registry.get(&instance.def) else {
                continue;
            };
            for name in instance.settings.keys() {
                if def.setting(name).is_none() {
                    errors.push(GraphError::UnknownSetting {
                        node: id,
                        setting: name.clone(),
                    });
                }
            }
            let setting_name = match def.body {
                NodeBody::Param | NodeBody::Resource | NodeBody::AttributeRead => {
                    node::SETTING_NAME
                }
                NodeBody::UserRead => node::SETTING_FIELD,
                _ => continue,
            };
            let raw = self
                .setting(registry, id, setting_name)
                .unwrap_or_default()
                .trim()
                .to_string();
            let Some(name) = WxslIdent::new(&raw) else {
                let reason = if raw.is_empty() {
                    "is empty; a declaration has to be called something"
                } else {
                    "is not a valid WXSL identifier"
                };
                errors.push(GraphError::InvalidSetting {
                    node: id,
                    setting: setting_name.to_string(),
                    value: raw,
                    reason: reason.to_string(),
                });
                continue;
            };
            // A texture becomes a module-scope `var` of that exact name, so
            // it can collide with what the generated module already uses.
            // A parameter is a struct field and cannot, but rejecting both
            // the same way is one rule to remember instead of two.
            if let Some(reason) = reserved_reason(name.as_str()) {
                errors.push(GraphError::InvalidSetting {
                    node: id,
                    setting: setting_name.to_string(),
                    value: name.as_str().to_string(),
                    reason,
                });
                continue;
            }

            let Some(socket) = def.outputs.first() else {
                continue;
            };
            match def.body {
                NodeBody::Param | NodeBody::Resource => {
                    let Some(ty) = self.effective_type(id, socket) else {
                        // An unresolved generic, already reported above.
                        continue;
                    };
                    match declared.get(name.as_str()) {
                        Some(&first) if first != ty => {
                            errors.push(GraphError::ConflictingDeclaration {
                                name: name.as_str().to_string(),
                                first,
                                second: ty,
                            });
                        }
                        _ => {
                            declared.insert(name.as_str().to_string(), ty);
                        }
                    }
                }
                NodeBody::UserRead => {
                    let Some(block) = &self.user_block else {
                        errors.push(GraphError::NoUserBlock { node: id });
                        continue;
                    };
                    let Some(field) = block
                        .fields
                        .iter()
                        .find(|field| field.name.trim() == name.as_str())
                    else {
                        errors.push(GraphError::UnknownUserField {
                            node: id,
                            field: name.as_str().to_string(),
                            declared: block
                                .fields
                                .iter()
                                .map(|field| field.name.clone())
                                .collect(),
                        });
                        continue;
                    };
                    // The one socket whose type comes from the *document*
                    // rather than from the definition or an edge, so it is
                    // the one place a resolved generic can be wrong rather
                    // than merely missing.
                    if let Some(resolved) = self.effective_type(id, socket) {
                        if resolved != field.ty {
                            errors.push(GraphError::UserFieldTypeMismatch {
                                node: id,
                                field: name.as_str().to_string(),
                                resolved,
                                declared: field.ty,
                            });
                        }
                    }
                }
                NodeBody::AttributeRead => {
                    let Some(decl) = self.attribute(name.as_str()) else {
                        errors.push(GraphError::UnknownAttribute {
                            node: id,
                            attribute: name.as_str().to_string(),
                            declared: self
                                .attributes
                                .iter()
                                .map(|decl| decl.name.clone())
                                .collect(),
                        });
                        continue;
                    };
                    // Like a user-block field, and unlike a parameter:
                    // the type is the *document's*, so a resolved generic
                    // here can be wrong rather than merely missing.
                    if let Some(resolved) = self.effective_type(id, socket) {
                        if resolved != decl.ty {
                            errors.push(GraphError::AttributeTypeMismatch {
                                node: id,
                                attribute: name.as_str().to_string(),
                                resolved,
                                declared: decl.ty,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// One terminal of each kind at most, and nothing wired into a
    /// terminal that cannot compile in that terminal's stage.
    ///
    /// The second half is the only typing rule partitioning needs. A
    /// missing surface output is [`crate::codegen::generate`]'s to
    /// report, because a graph mid-edit legitimately has none.
    fn check_outputs(&self, registry: &NodeRegistry, errors: &mut Vec<GraphError>) {
        for (kind, found) in [
            (abi::SURFACE_OUTPUT_ID, self.surface_outputs(registry)),
            (abi::VERTEX_OUTPUT_ID, self.vertex_outputs(registry)),
            (abi::DISCARD_OUTPUT_ID, self.discard_outputs(registry)),
        ] {
            if found.len() > 1 {
                errors.push(GraphError::DuplicateOutput {
                    kind: kind.to_string(),
                    nodes: found,
                });
            }
        }
        self.check_varying_outputs(registry, errors);

        // A vertex-only node under a fragment terminal is the mistake
        // this catches: `object_position` has no meaning by the time the
        // surface is shaded, and the generated fragment function has no
        // struct to read it from.
        let fragment_roots = self
            .surface_outputs(registry)
            .into_iter()
            .chain(self.discard_outputs(registry));
        for root in fragment_roots {
            for id in self.dependencies_of(root) {
                let Some(node) = self.nodes.get(&id) else {
                    continue;
                };
                let Some(def) = registry.get(&node.def) else {
                    continue;
                };
                if def.is_vertex_only() {
                    errors.push(GraphError::WrongStage {
                        node: id,
                        def: def.id.clone(),
                        output: root,
                        reason: "it reads object space, which only the vertex stage has"
                            .to_string(),
                    });
                }
            }
        }

        // And the mirror mistake, which only exists now that a graph can
        // compute an interpolant: reading one from the vertex stage,
        // which is the stage computing it.
        let vertex_roots = self
            .vertex_outputs(registry)
            .into_iter()
            .chain(self.varying_outputs(registry));
        for root in vertex_roots {
            for id in self.dependencies_of(root) {
                let Some(name) = self.attribute_read_name(registry, id) else {
                    continue;
                };
                let Some(decl) = self.attribute(name.as_str()) else {
                    continue;
                };
                if decl.frequency != AttributeFrequency::Computed {
                    continue;
                }
                let def = self.nodes.get(&id).map(|node| node.def.clone());
                errors.push(GraphError::WrongStage {
                    node: id,
                    def: def.unwrap_or_default(),
                    output: root,
                    reason: format!(
                        "`{name}` is an interpolant the vertex stage computes, so \
                         only the fragment stage can read it"
                    ),
                });
            }
        }
    }

    /// The name a [`NodeBody::AttributeRead`] node reads, if that is what
    /// `node` is.
    fn attribute_read_name(&self, registry: &NodeRegistry, node: NodeId) -> Option<WxslIdent> {
        let def = registry.get(&self.nodes.get(&node)?.def)?;
        matches!(def.body, NodeBody::AttributeRead)
            .then(|| self.declared_name(registry, node, node::SETTING_NAME))
            .flatten()
    }

    /// Every declared interpolant is written exactly once, and every
    /// writer names one.
    ///
    /// Both halves matter and for different reasons: a declaration
    /// nothing writes spends an inter-stage location on an undefined
    /// value, and a writer naming nothing has no location to write to.
    fn check_varying_outputs(&self, registry: &NodeRegistry, errors: &mut Vec<GraphError>) {
        let mut written: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
        for node in self.varying_outputs(registry) {
            let Some(name) = self.declared_name(registry, node, node::SETTING_NAME) else {
                errors.push(GraphError::InvalidAttribute {
                    reason: format!(
                        "the interpolant output on node {} names no interpolant",
                        node.0
                    ),
                });
                continue;
            };
            match self.attribute(name.as_str()) {
                Some(decl) if decl.frequency == AttributeFrequency::Computed => {}
                Some(decl) => errors.push(GraphError::InvalidAttribute {
                    reason: format!(
                        "`{name}` is declared as a {} attribute, and the geometry \
                         supplies it — only a computed one is written by the graph",
                        decl.frequency
                    ),
                }),
                None => errors.push(GraphError::UnknownAttribute {
                    node,
                    attribute: name.as_str().to_string(),
                    declared: self
                        .attributes
                        .iter()
                        .map(|decl| decl.name.clone())
                        .collect(),
                }),
            }
            written
                .entry(name.as_str().to_string())
                .or_default()
                .push(node);
        }
        for (name, nodes) in &written {
            if nodes.len() > 1 {
                errors.push(GraphError::DuplicateOutput {
                    kind: format!("{} `{name}`", abi::VARYING_OUTPUT_ID),
                    nodes: nodes.clone(),
                });
            }
        }
        for decl in &self.attributes {
            if decl.frequency != AttributeFrequency::Computed {
                continue;
            }
            if !written.contains_key(decl.name.trim()) {
                errors.push(GraphError::InvalidAttribute {
                    reason: format!(
                        "`{}` is a computed interpolant and nothing writes it; add an \
                         `{}` node or drop the declaration",
                        decl.name.trim(),
                        abi::VARYING_OUTPUT_ID,
                    ),
                });
            }
        }
    }

    /// The declared attribute set, independent of whether anything reads
    /// it — and the one accountant for the inter-stage location budget.
    ///
    /// Everything here is checked against a *limit* rather than against
    /// another declaration, which is what makes it worth doing in one
    /// place: a graph that overruns the varying budget or the vertex
    /// buffer budget should be told which limit and by how much, not
    /// discover it as a shader-compiler error about location 16.
    fn check_attributes(&self, errors: &mut Vec<GraphError>) {
        let mut invalid = |reason: String| errors.push(GraphError::InvalidAttribute { reason });
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut per_vertex = 0usize;
        for decl in &self.attributes {
            let name = decl.name.trim();
            let Some(ident) = WxslIdent::new(name) else {
                invalid(format!("`{name}` is not a valid WXSL attribute name"));
                continue;
            };
            if !seen.insert(name) {
                invalid(format!("`{name}` is declared twice"));
            }
            if let Some(reason) = reserved_reason(ident.as_str()) {
                invalid(format!("`{name}` {reason}"));
            }
            if abi::INSTANCE_BASE_FIELDS
                .iter()
                .any(|base| base.name == name)
            {
                invalid(format!(
                    "`{name}` is a field every instance row already has, \
                     and the transform lives there",
                ));
            }
            match decl.frequency {
                AttributeFrequency::Vertex => {
                    per_vertex += 1;
                    if let Some(reason) = vertex_attribute_reason(decl.ty) {
                        invalid(format!("`{name}`: {reason}"));
                    }
                }
                AttributeFrequency::Instance => {
                    if decl.ty.is_resource() {
                        invalid(format!(
                            "`{name}` is {}, and an instance row holds values \
                             rather than resources",
                            decl.ty
                        ));
                    }
                }
                AttributeFrequency::Computed => {
                    // The same four types a per-vertex stream may be, and
                    // for the neighbouring reason: this one *is* the
                    // `@location`, and a matrix has no interpolation.
                    if !abi::INTERPOLANT_TYPES.contains(&decl.ty) {
                        invalid(format!(
                            "`{name}` is {}, and an interpolated value is a \
                             float or a float vector",
                            decl.ty
                        ));
                    }
                }
            }
        }
        if per_vertex > abi::MAX_VERTEX_ATTRIBUTES {
            invalid(format!(
                "{per_vertex} per-vertex attributes declared, and the budget is \
                 {} — each is a vertex buffer slot of its own",
                abi::MAX_VERTEX_ATTRIBUTES,
            ));
        }
        // The accountant: the base varyings, the instance index if
        // anything needs it, and one per declared per-vertex attribute.
        let geometry = GeometryInterface::new(
            self.declared_attributes(AttributeFrequency::Vertex),
            self.declared_attributes(AttributeFrequency::Instance),
            self.declared_attributes(AttributeFrequency::Computed),
        );
        let used = geometry.varyings_used();
        if used > abi::MAX_VARYING_LOCATIONS {
            invalid(format!(
                "{used} inter-stage locations used, and there are {}: \
                 {} for the shading basis{}, {} declared per-vertex \
                 attribute(s) and {} computed interpolant(s)",
                abi::MAX_VARYING_LOCATIONS,
                abi::VERTEX_OUT_FIELDS.len(),
                if geometry.instance_index_location().is_some() {
                    " plus one for the instance index"
                } else {
                    ""
                },
                geometry.vertex().len(),
                geometry.computed().len(),
            ));
        }
    }

    /// The application block's own declaration, independent of whether any
    /// node reads it: a graph that carries an unusable one should say so
    /// when it is written, not when something first reads it.
    fn check_user_block(&self, errors: &mut Vec<GraphError>) {
        let Some(block) = &self.user_block else {
            return;
        };
        let mut invalid = |reason: String| errors.push(GraphError::InvalidUserBlock { reason });
        if WxslIdent::new(block.name.trim()).is_none() {
            invalid(format!(
                "`{}` is not a valid WXSL identifier for the block itself",
                block.name
            ));
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for field in &block.fields {
            let name = field.name.trim();
            if WxslIdent::new(name).is_none() {
                invalid(format!("`{name}` is not a valid WXSL field name"));
                continue;
            }
            if !seen.insert(name) {
                invalid(format!("`{name}` is declared twice"));
            }
            if field.ty.is_resource() {
                // The block is one uniform buffer. A texture in the
                // application group is the application's business, and
                // nothing here would know how to bind it.
                invalid(format!(
                    "`{name}` is {}, and a uniform block holds values rather than resources",
                    field.ty
                ));
            }
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
        // `Graph::connect` refuses these, so reaching one here means a
        // hand-edited or older document.
        let constant = self
            .definition(registry, edge.to.node)
            .ok()
            .and_then(|def| def.input(&edge.to.socket))
            .is_some_and(|socket| socket.constant);
        if constant {
            errors.push(GraphError::ConstantInput {
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

    use super::{AttributeDecl, Edge, Graph, Node, NodeId, UserBlockDecl};
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub user_block: Option<UserBlockDecl>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub attributes: Vec<AttributeDecl>,
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
                user_block: graph.user_block,
                attributes: graph.attributes,
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
                user_block: wire.user_block,
                attributes: wire.attributes,
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

    /// A registry with one node reading a declared attribute.
    fn attribute_registry() -> NodeRegistry {
        let mut registry = NodeRegistry::new();
        registry.register(
            NodeDefinition::builder("input.attribute", "Attribute")
                .setting(crate::node::SettingDef::new(
                    node::SETTING_NAME,
                    "name",
                    "Which attribute.",
                ))
                .generic_param(crate::node::GenericParam::new("T", ValueType::ALL.to_vec()))
                .output(Socket::new("out", ValueType::F32).generic("T"))
                .declaration(NodeBody::AttributeRead),
        );
        registry
    }

    #[test]
    fn reading_an_attribute_the_graph_does_not_declare_is_reported() {
        let registry = attribute_registry();
        let mut graph = Graph::new("undeclared");
        let node = graph.add(Node::new("input.attribute").with_setting("name", "color"));
        graph
            .set_generic(&registry, node, "T", ValueType::Vec3)
            .expect("allowed");
        let errors = graph.validate(&registry).expect_err("undeclared");
        assert!(
            errors
                .0
                .iter()
                .any(|error| matches!(error, GraphError::UnknownAttribute { .. })),
            "{errors:?}"
        );

        // And declaring it — at the same type — is all it takes.
        graph.declare_attribute(AttributeDecl::vertex("color", ValueType::Vec3));
        graph.validate(&registry).expect("declared now");

        // At a different type it is a mismatch, not a silent
        // reinterpretation: the type comes from the document, so this is
        // one of the few places a resolved generic can be *wrong*.
        graph.declare_attribute(AttributeDecl::vertex("color", ValueType::Vec2));
        let errors = graph.validate(&registry).expect_err("mistyped");
        assert!(
            errors
                .0
                .iter()
                .any(|error| matches!(error, GraphError::AttributeTypeMismatch { .. })),
            "{errors:?}"
        );
    }

    #[test]
    fn a_per_vertex_attribute_of_a_type_no_vertex_buffer_carries_is_reported() {
        let registry = attribute_registry();
        let mut graph = Graph::new("integers");
        // Fine per instance — a storage buffer holds anything — and not
        // fine per vertex, where `wgpu::VertexFormat` decides.
        graph.declare_attribute(AttributeDecl::instance("count", ValueType::U32));
        graph
            .validate(&registry)
            .expect("a storage row holds a u32");
        graph.declare_attribute(AttributeDecl::vertex("count", ValueType::U32));
        let errors = graph.validate(&registry).expect_err("not a vertex format");
        assert!(
            errors
                .0
                .iter()
                .any(|error| matches!(error, GraphError::InvalidAttribute { .. })),
            "{errors:?}"
        );
    }

    #[test]
    fn overrunning_a_budget_says_which_budget_and_by_how_much() {
        // The accountant. Both limits are counted in one place, so a
        // graph that overruns either is told rather than discovering it
        // as a shader-compiler error about location 16.
        let registry = attribute_registry();
        let mut graph = Graph::new("greedy");
        for index in 0..abi::MAX_VERTEX_ATTRIBUTES + 1 {
            graph.declare_attribute(AttributeDecl::vertex(
                format!("stream_{index}"),
                ValueType::Vec2,
            ));
        }
        let errors = graph.validate(&registry).expect_err("over the slot budget");
        let reported: Vec<String> = errors.0.iter().map(|error| error.to_string()).collect();
        assert!(
            reported
                .iter()
                .any(|text| text.contains("per-vertex attributes declared")),
            "{reported:?}"
        );
    }

    #[test]
    fn an_attribute_cannot_take_a_name_the_instance_row_already_uses() {
        let registry = attribute_registry();
        let mut graph = Graph::new("shadowing");
        graph.declare_attribute(AttributeDecl::instance(
            abi::INSTANCE_BASE_FIELDS[0].name,
            ValueType::Mat4,
        ));
        let errors = graph.validate(&registry).expect_err("that name is taken");
        assert!(
            errors
                .0
                .iter()
                .any(|error| matches!(error, GraphError::InvalidAttribute { .. })),
            "{errors:?}"
        );
    }

    use crate::node::TypeRule;
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
            NodeDefinition::builder("test.product", "Product")
                .generic_param(crate::node::GenericParam::new(
                    "A",
                    vec![ValueType::F32, ValueType::Vec2, ValueType::Vec3],
                ))
                .generic_param(crate::node::GenericParam::new(
                    "B",
                    vec![ValueType::F32, ValueType::Vec2, ValueType::Vec3],
                ))
                .input(Socket::new("a", ValueType::F32).generic("A"))
                .input(Socket::new("b", ValueType::F32).generic("B"))
                .output(Socket::new("out", ValueType::F32).combine(TypeRule::Product, "A", "B"))
                .expr("{a} * {b}"),
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

        // `b` is still unconnected, but resolving `T` gave it a value of
        // the type it turned out to be (`Graph::retype_params`), so there
        // is nothing left unfed — a socket with a known type always has
        // something to edit.
        assert_eq!(
            graph
                .node(add)
                .and_then(|node| node.params.get("b"))
                .copied(),
            Some(Value::Vec3([0.0; 3]))
        );
        graph.validate(&registry).expect("resolved and fed");
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
    fn a_placed_node_starts_at_its_default_type_and_validates() {
        // What an editor gets from dropping a node on the canvas: complete,
        // not half-typed. Nothing is connected, so nothing has said what the
        // type should be — but "no type at all" is an error, and a node
        // nobody has touched should not report one.
        let registry = registry();
        let mut graph = Graph::new("g");
        let add = graph.add_resolved(&registry, Node::new("test.generic_add"));
        assert_eq!(graph.generic_type(add, "T"), Some(ValueType::F32));
        assert_eq!(
            graph
                .node(add)
                .and_then(|node| node.params.get("a"))
                .copied(),
            Some(Value::F32(0.0)),
            "a generic input has no fixed default, so it needs a pinned value"
        );
        graph.validate(&registry).expect("placed and complete");
    }

    #[test]
    fn picking_a_type_carries_the_pinned_values_across() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let add = graph.add_resolved(&registry, Node::new("test.generic_add"));
        graph.set_param(add, "a", Value::F32(0.5));
        graph
            .set_generic(&registry, add, "T", ValueType::Vec3)
            .expect("vec3f is allowed");
        assert_eq!(
            graph
                .node(add)
                .and_then(|node| node.params.get("a"))
                .copied(),
            Some(Value::Vec3([0.5; 3])),
            "the value the author typed should widen, not become a mismatch"
        );
        graph.validate(&registry).expect("retyped and still fed");
    }

    #[test]
    fn plugging_a_vector_into_a_scalar_socket_retypes_the_node() {
        // The default `f32` is a default, not a refusal waiting to happen.
        let registry = registry();
        let mut graph = Graph::new("g");
        let source = graph.add_node("test.vec");
        let add = graph.add_resolved(&registry, Node::new("test.generic_add"));
        assert_eq!(graph.generic_type(add, "T"), Some(ValueType::F32));
        graph
            .wire(&registry, (source, "out"), (add, "a"))
            .expect("a wire retypes what only a default backs");
        assert_eq!(graph.generic_type(add, "T"), Some(ValueType::Vec3));
        graph.validate(&registry).expect("retyped and still fed");
    }

    #[test]
    fn retyping_a_combined_output_solves_back_through_its_operands() {
        // `out`'s type is derived, so nothing can adopt into it directly —
        // but changing what it derives *from* does derive it, and that is a
        // plan too. The smallest one wins: `vec3f * f32` is already a
        // `vec3f`, so only `A` moves and the other operand stays a scalar
        // the author can still type a single number into.
        let registry = registry();
        let mut graph = Graph::new("g");
        let mul = graph.add_resolved(&registry, Node::new("test.product"));
        let sink = graph.add_node("test.vec"); // takes a vec3f
        assert_eq!(graph.generic_type(mul, "A"), Some(ValueType::F32));
        graph
            .wire(&registry, (mul, "out"), (sink, "v"))
            .expect("A can become vec3f, which makes the product one");
        assert_eq!(graph.generic_type(mul, "A"), Some(ValueType::Vec3));
        assert_eq!(graph.generic_type(mul, "B"), Some(ValueType::F32));

        let def = registry.get("test.product").unwrap();
        let out = def.output("out").unwrap();
        assert_eq!(graph.effective_type(mul, out), Some(ValueType::Vec3));
        graph.validate(&registry).expect("solved and fed");
    }

    #[test]
    fn retyping_stops_where_it_would_break_an_edge_that_already_exists() {
        // The line between adapting and vandalising: `out` is wired to
        // something that really is an `f32`, so `a` cannot quietly become a
        // `vec3f` and take that wire down with it.
        let registry = registry();
        let mut graph = Graph::new("g");
        let mul = graph.add_resolved(&registry, Node::new("test.product"));
        let sink = graph.add_node("test.add"); // concrete f32 inputs
        let vector = graph.add_node("test.vec");
        graph
            .wire(&registry, (mul, "out"), (sink, "a"))
            .expect("f32 * f32 is an f32");

        let error = graph
            .wire(&registry, (vector, "out"), (mul, "a"))
            .expect_err("retyping A would break the output's edge");
        assert!(
            matches!(error, GraphError::TypeMismatch { .. }),
            "{error:?}"
        );
        assert_eq!(graph.generic_type(mul, "A"), Some(ValueType::F32));
        assert_eq!(graph.edges().len(), 1, "and nothing was disconnected");
    }

    #[test]
    fn resolving_one_operand_seeds_the_other() {
        // Two independent parameters exist so `f32 * vec3f` is expressible,
        // not because two operands of different types are the common case —
        // so wiring one operand leaves the node complete, not half-typed.
        let registry = registry();
        let mut graph = Graph::new("g");
        let source = graph.add_node("test.vec");
        let mul = graph.add_node("test.product");
        graph
            .wire(&registry, (source, "out"), (mul, "a"))
            .expect("A resolves to vec3f");
        assert_eq!(graph.generic_type(mul, "A"), Some(ValueType::Vec3));
        assert_eq!(
            graph.generic_type(mul, "B"),
            Some(ValueType::Vec3),
            "the other operand should follow, so the node is usable at once"
        );
        graph.set_param(mul, "b", Value::Vec3([2.0; 3]));
        graph.validate(&registry).expect("complete from one wire");
    }

    #[test]
    fn a_wire_beats_a_resolution_no_other_wire_backs() {
        // The seeded `B` above is a default, so connecting a `f32` to `b`
        // must retype it rather than be refused — which is exactly the
        // reported bug: "connect a float and the other socket only accepts
        // floats".
        let registry = registry();
        let mut graph = Graph::new("g");
        let vector = graph.add_node("test.vec");
        let scalar = graph.add_node("test.const");
        let mul = graph.add_node("test.product");
        graph
            .wire(&registry, (vector, "out"), (mul, "a"))
            .expect("A resolves to vec3f, B is seeded to match");
        assert_eq!(graph.generic_type(mul, "B"), Some(ValueType::Vec3));
        graph
            .wire(&registry, (scalar, "out"), (mul, "b"))
            .expect("a wire retypes a seeded parameter");
        assert_eq!(graph.generic_type(mul, "B"), Some(ValueType::F32));

        let def = registry.get("test.product").unwrap();
        let out = def.output("out").unwrap();
        assert_eq!(graph.effective_type(mul, out), Some(ValueType::Vec3));
        graph.validate(&registry).expect("vec3f * f32 is valid");
    }

    #[test]
    fn an_explicit_pick_is_also_only_a_default_until_a_wire_backs_it() {
        // Same rule, reached the other way: nothing is connected to the
        // node whose type was picked, so the pick loses to the wire. Once a
        // wire *does* back it, `a_mismatched_connection_to_an_already_
        // resolved_generic_is_rejected` is the case that applies instead.
        let registry = registry();
        let mut graph = Graph::new("g");
        let vector = graph.add_node("test.vec");
        let negate = graph.add_node("test.generic_negate");
        graph
            .set_generic(&registry, negate, "T", ValueType::F32)
            .expect("f32 is allowed");
        graph
            .wire(&registry, (vector, "out"), (negate, "a"))
            .expect("the wire retypes an unbacked pick");
        assert_eq!(graph.generic_type(negate, "T"), Some(ValueType::Vec3));
    }

    #[test]
    fn disconnecting_everything_forgets_both_of_two_parameters() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let source = graph.add_node("test.const"); // f32
        let mul = graph.add_node("test.product");
        graph
            .wire(&registry, (source, "out"), (mul, "a"))
            .expect("A resolves to f32, B is seeded to match");
        assert_eq!(graph.generic_type(mul, "A"), Some(ValueType::F32));
        assert_eq!(graph.generic_type(mul, "B"), Some(ValueType::F32));

        graph
            .disconnect(&registry, &SocketRef::new(mul, "a"))
            .expect("there was an edge");
        // Both are back to the default — the type the node would have been
        // placed with — rather than to nothing, since an unresolved
        // parameter is an error and nothing about the node is broken.
        let default = registry.get("test.product").unwrap().default_generics();
        assert_eq!(graph.generic_type(mul, "A"), Some(default["A"]));
        assert_eq!(
            graph.generic_type(mul, "B"),
            Some(default["B"]),
            "a seeded parameter is released with the one that seeded it"
        );
        graph.validate(&registry).expect("resolved and fed");
    }

    #[test]
    fn unplugging_a_combined_output_keeps_what_the_operands_still_say() {
        // `out` is the only edge that goes, and it never resolved anything
        // by itself — but `a` is still wired, so `A` stays, and `B` (seeded
        // from `A`) has to be seeded again rather than left blank.
        let registry = registry();
        let mut graph = Graph::new("g");
        let source = graph.add_node("test.vec");
        let mul = graph.add_node("test.product");
        let sink = graph.add_node("test.vec");
        graph
            .wire(&registry, (source, "out"), (mul, "a"))
            .expect("A resolves to vec3f, B is seeded to match");
        graph
            .wire(&registry, (mul, "out"), (sink, "v"))
            .expect("vec3f * vec3f is a vec3f");

        graph
            .disconnect(&registry, &SocketRef::new(sink, "v"))
            .expect("there was an edge");
        assert_eq!(graph.generic_type(mul, "A"), Some(ValueType::Vec3));
        assert_eq!(graph.generic_type(mul, "B"), Some(ValueType::Vec3));
    }

    #[test]
    fn disconnecting_every_edge_releases_a_generic_resolution() {
        // The bug this pins down: a node that once resolved `T` to `f32`
        // stayed stuck there forever, even fully disconnected — so it could
        // never again accept a `vec3f`.
        let registry = registry();
        let mut graph = Graph::new("g");
        let source = graph.add_node("test.vec"); // vec3f
        let add = graph.add_node("test.generic_add");
        graph
            .wire(&registry, (source, "out"), (add, "a"))
            .expect("resolves T to vec3f");
        assert_eq!(graph.generic_type(add, "T"), Some(ValueType::Vec3));

        graph
            .disconnect(&registry, &SocketRef::new(add, "a"))
            .expect("there was an edge");
        assert_eq!(
            graph.generic_type(add, "T"),
            Some(ValueType::F32),
            "nothing constrains T anymore, so it falls back to the default"
        );

        let vec_source = graph.add_node("test.vec");
        graph
            .wire(&registry, (vec_source, "out"), (add, "a"))
            .expect("T is free to become vec3f now");
        assert_eq!(graph.generic_type(add, "T"), Some(ValueType::Vec3));
    }

    #[test]
    fn removing_a_node_releases_a_surviving_neighbors_generic_resolution() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let source = graph.add_node("test.vec"); // vec3f
        let add = graph.add_node("test.generic_add");
        graph
            .wire(&registry, (source, "out"), (add, "a"))
            .expect("resolves T to vec3f");
        assert_eq!(graph.generic_type(add, "T"), Some(ValueType::Vec3));

        graph.remove_node(&registry, source).expect("it existed");
        assert_eq!(
            graph.generic_type(add, "T"),
            Some(ValueType::F32),
            "its only connection went with the node that made it, so `T` \
             is back to the default rather than stuck or unresolved"
        );
        graph.validate(&registry).expect("resolved and fed");
    }

    #[test]
    fn a_node_still_wired_to_another_generic_node_keeps_its_resolution() {
        // The deliberately-scoped half of the same rule: removing the edge
        // that anchored a *chain* does not chase back through it — see
        // `Graph::refresh_generics`'s doc comment for why.
        let registry = registry();
        let mut graph = Graph::new("g");
        let first = graph.add_node("test.generic_negate");
        let second = graph.add_node("test.generic_negate");
        graph
            .wire(&registry, (first, "out"), (second, "a"))
            .expect("both stay polymorphic");
        let sink = graph.add_node("test.vec");
        graph
            .wire(&registry, (second, "out"), (sink, "v"))
            .expect("resolves the whole chain to vec3f");
        assert_eq!(graph.generic_type(first, "T"), Some(ValueType::Vec3));
        assert_eq!(graph.generic_type(second, "T"), Some(ValueType::Vec3));

        graph
            .disconnect(&registry, &SocketRef::new(sink, "v"))
            .unwrap();
        assert_eq!(
            graph.generic_type(second, "T"),
            Some(ValueType::Vec3),
            "second is still wired to first, so it keeps its resolution"
        );
    }

    #[test]
    fn a_combined_output_adapts_to_a_scalar_and_a_vector_operand() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let scalar = graph.add_node("test.const"); // f32
        let vector = graph.add_node("test.vec"); // vec3f
        let mul = graph.add_node("test.product");
        graph
            .wire(&registry, (scalar, "out"), (mul, "a"))
            .expect("A resolves to f32");
        graph
            .wire(&registry, (vector, "out"), (mul, "b"))
            .expect("B resolves to vec3f");

        let def = registry.get("test.product").unwrap();
        let out = def.output("out").unwrap();
        assert_eq!(graph.effective_type(mul, out), Some(ValueType::Vec3));
        graph.validate(&registry).expect("f32 * vec3f is valid");
    }

    #[test]
    fn mismatched_combined_operands_are_reported_but_do_not_block_connecting() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let two = graph.add_node("test.product");
        graph
            .set_generic(&registry, two, "A", ValueType::Vec2)
            .unwrap();
        graph
            .set_generic(&registry, two, "B", ValueType::Vec3)
            .unwrap();

        let def = registry.get("test.product").unwrap();
        let out = def.output("out").unwrap();
        assert_eq!(
            graph.effective_type(two, out),
            None,
            "vec2f and vec3f do not combine"
        );
        graph.set_param(two, "a", crate::node::Value::Vec2([0.0; 2]));
        graph.set_param(two, "b", crate::node::Value::Vec3([0.0; 3]));
        let errors = graph
            .validate(&registry)
            .expect_err("incompatible operands");
        assert!(
            errors
                .0
                .iter()
                .any(|e| matches!(e, GraphError::IncompatibleGenerics { .. })),
            "{errors:?}"
        );
    }

    #[test]
    fn repinning_one_combined_operand_disconnects_a_now_mismatched_output() {
        let registry = registry();
        let mut graph = Graph::new("g");
        let mul = graph.add_node("test.product");
        let sink = graph.add_node("test.vec"); // takes a vec3f
        graph
            .set_generic(&registry, mul, "A", ValueType::Vec3)
            .unwrap();
        graph
            .set_generic(&registry, mul, "B", ValueType::F32)
            .unwrap();
        graph.wire(&registry, (mul, "out"), (sink, "v")).unwrap();

        // Repinning B to vec2f makes the product of (vec3f, vec2f)
        // incompatible, so the output no longer agrees with `sink`.
        let disconnected = graph
            .set_generic(&registry, mul, "B", ValueType::Vec2)
            .expect("vec2f is allowed for B");
        assert_eq!(disconnected.len(), 1, "the output's stale edge came back");
        assert_eq!(disconnected[0].to.node, sink);
        assert!(graph.edges().is_empty());
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
        graph
            .disconnect(&registry, &SocketRef::new(add, "a"))
            .unwrap();
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
        graph.remove_node(&registry, a).unwrap();
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
