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
use crate::node::{
    self, GraphDomain, NodeBody, NodeDefinition, NodeRegistry, Socket, Value, ValueType,
};
use crate::resources::{
    BufferLayout, GeometryInterface, MaterialInterface, ResourceBinding, UserBlock,
};
use crate::wxsl::WxslIdent;

mod generics;
#[cfg(test)]
mod tests;
mod validate;
#[cfg(feature = "serde")]
mod wire;

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
    /// The stage constraint the author pinned on this node, or [`StageConstraint::Auto`] —
    /// the default, and the reason the field usually serializes to
    /// nothing. Read by stage analysis
    /// ([`crate::stages`], plan2 P9), not by the editor's layout code.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "StageConstraint::is_auto")
    )]
    pub stage: StageConstraint,
}

/// How firmly an author has said which shader stage a node runs in.
///
/// The default is [`StageConstraint::Auto`]: stage analysis decides, and
/// computes shared nodes once, in the vertex stage, handing them to the
/// fragment side as synthesized interpolants. A constraint is the author
/// overriding that — most often `Fragment`, to pin the old behaviour of
/// duplicating a shared subtree into the fragment stage instead of paying
/// an interpolant for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StageConstraint {
    /// The analysis decides, from what the node reads and what consumes it.
    #[default]
    Auto,
    /// Compute in the vertex stage. If only fragment consumers read the
    /// node, its value rides a synthesized interpolant down.
    Vertex,
    /// Compute in the fragment stage. A vertex terminal that still needs
    /// the node makes the vertex stage compute its own copy.
    Fragment,
}

impl StageConstraint {
    /// Whether this is [`StageConstraint::Auto`] — the serde skip test.
    pub fn is_auto(&self) -> bool {
        *self == StageConstraint::Auto
    }
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
            stage: StageConstraint::Auto,
        }
    }

    /// Pin `value` on the input named `socket`.
    pub fn with_param(mut self, socket: impl Into<String>, value: Value) -> Self {
        self.params.insert(socket.into(), value);
        self
    }

    /// Constrain which shader stage this node compiles into.
    pub fn with_stage(mut self, stage: StageConstraint) -> Self {
        self.stage = stage;
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

/// The node format's schema version — the same wire shape also carries the
/// pipeline documents, so one number covers both. Stamped into every
/// serialized graph's `version` field and checked on load (a *newer*
/// version than this build knows is a named parse error); the ABI a
/// document's vocabulary is written against is pinned separately, by the
/// `abi` field, against [`crate::abi::REVISION`].
pub const SCHEMA_VERSION: u32 = 1;

/// A typed, acyclic shader node graph.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(try_from = "wire::WireGraph", into = "wire::WireGraph")
)]
pub struct Graph {
    name: String,
    domain: GraphDomain,
    nodes: BTreeMap<NodeId, Node>,
    edges: Vec<Edge>,
    macros: MacroSet,
    user_block: Option<UserBlockDecl>,
    attributes: Vec<AttributeDecl>,
    bakes: Vec<BakeDecl>,
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

/// One part of a material the author wants *precomputed*: the subgraph
/// feeding one node's output, evaluated per texel of a small 2D table and
/// sampled back at the surface's uv
/// ([ADR 0045](../../../docs/adr/0045-a-bake-is-an-effect-over-a-material-subgraph.md)).
///
/// A graph-level declaration, like [`AttributeDecl`] and for the same
/// reason: it is a claim *about a node* rather than a node of its own —
/// "bake this term" — and it has to stay stated and checkable while the
/// branch it names is half-wired, exactly as an attribute declaration
/// does. The node keeps its own semantics; what the declaration changes is
/// which of two ways its value reaches the surface, and that switch is the
/// material configuration's `bakes` flag, not an edit here.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub struct BakeDecl {
    /// The node whose output subgraph is baked. Its *other* output
    /// sockets, if it has any, must be unused — the bake replaces the
    /// node wholesale when it is active.
    pub node: NodeId,
    /// Which of the node's output sockets is baked, or `None` for the
    /// first — the common case, a one-output node like `generative.fbm3`.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub socket: Option<String>,
    /// The name the bake table goes by: what the host binds the texture
    /// by, what the pipeline's `resource.color` is *labelled*, and the
    /// `@group(1)` variable the generated sample reads. A WGSL identifier;
    /// the sampler beside it is `{texture}_sampler`.
    pub texture: String,
    /// The bake table's size, in pixels. Fixed, like a LUT's: the value is
    /// a fact of the subgraph, not of the window.
    pub size: [u32; 2],
    /// The table's precision — `standard` (8-bit) or `hdr` (half float),
    /// the two precisions a write-only storage target can carry. The
    /// pipeline's `resource.color` for this texture must spell the same
    /// one.
    #[cfg_attr(
        feature = "serde",
        serde(default = "default_bake_precision", with = "bake_precision")
    )]
    pub precision: crate::abi::GBufferPrecision,
}

/// What a [`BakeDecl`] defaults to: `hdr`, the same default a `resource.color`
/// carries, because the two must agree and half float is the honest
/// starting point for a value that may exceed 0..1.
fn default_bake_precision() -> crate::abi::GBufferPrecision {
    crate::abi::GBufferPrecision::HighDynamicRange
}

/// [`BakeDecl::precision`] on the wire, as the [`GBufferPrecision::name`]
/// every document spells precisions with.
#[cfg(feature = "serde")]
mod bake_precision {
    use crate::abi::GBufferPrecision;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        precision: &GBufferPrecision,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(precision.name())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<GBufferPrecision, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        crate::abi::GBufferPrecision::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown precision `{text}`")))
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
            domain: GraphDomain::Surface,
            nodes: BTreeMap::new(),
            edges: Vec::new(),
            macros: MacroSet::new(),
            user_block: None,
            attributes: Vec::new(),
            bakes: Vec::new(),
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

    /// What kind of graph this is: a material, a screen effect, or a
    /// pipeline document (ADR 0040).
    ///
    /// [`GraphDomain::Surface`] unless said otherwise, which is why every
    /// document written before domains existed still loads as what it was.
    pub fn domain(&self) -> GraphDomain {
        self.domain
    }

    /// Set the domain. A builder form is [`Graph::in_domain`].
    ///
    /// Changing it does not rewire anything — it changes which nodes
    /// [`Graph::validate`] will accept, so a graph moved across domains
    /// reports the nodes that do not come with it rather than silently
    /// compiling as something else.
    pub fn set_domain(&mut self, domain: GraphDomain) {
        self.domain = domain;
    }

    /// An empty graph in `domain`: [`Graph::new`] plus
    /// [`Graph::set_domain`].
    pub fn in_domain(name: impl Into<String>, domain: GraphDomain) -> Self {
        let mut graph = Graph::new(name);
        graph.set_domain(domain);
        graph
    }

    /// Insert `node`, returning its fresh id.
    pub fn add(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        self.nodes.insert(id, node);
        id
    }

    /// The id the next added node gets. Ids are never reused, so a batch
    /// of edits can name the nodes it is about to add.
    pub fn next_id(&self) -> NodeId {
        NodeId(self.next_id)
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
    /// connecting a wire retypes it (`Graph::plan_retype`) and so does
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
    /// `Graph::refresh_generics`.
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

    /// Every bake declaration, in declaration order.
    pub fn bakes(&self) -> &[BakeDecl] {
        &self.bakes
    }

    /// The bake declaration naming `texture`, if there is one — how
    /// [`crate::codegen`] and the renderer address a bake without knowing
    /// node ids: by the name its table goes by.
    pub fn bake(&self, texture: &str) -> Option<&BakeDecl> {
        self.bakes
            .iter()
            .find(|decl| decl.texture.trim() == texture.trim())
    }

    /// Declare a bake. Replaces the declaration naming the same texture,
    /// if one is there — the texture name is the bake's identity, and two
    /// decls sharing one would be two tables under one binding.
    pub fn declare_bake(&mut self, decl: BakeDecl) {
        self.bakes
            .retain(|existing| existing.texture != decl.texture);
        self.bakes.push(decl);
    }

    /// Drop the bake declaration naming `texture`.
    pub fn remove_bake(&mut self, texture: &str) -> Option<BakeDecl> {
        let index = self
            .bakes
            .iter()
            .position(|decl| decl.texture.trim() == texture.trim())?;
        Some(self.bakes.remove(index))
    }

    /// Pin `node`'s setting `name`. Unlike [`Graph::setting`] this does not
    /// check the definition declares it — [`Graph::validate`] reports a
    /// setting that matches nothing.
    pub fn set_setting(&mut self, node: NodeId, name: impl Into<String>, value: impl Into<String>) {
        if let Some(instance) = self.nodes.get_mut(&node) {
            instance.settings.insert(name.into(), value.into());
        }
    }

    /// Constrain which shader stage `node` compiles into. The analysis
    /// that reads it is [`crate::stages::analyze`].
    pub fn set_stage(&mut self, node: NodeId, stage: StageConstraint) {
        if let Some(instance) = self.nodes.get_mut(&node) {
            instance.stage = stage;
        }
    }

    /// The stage constraint `node` carries.
    pub fn stage(&self, node: NodeId) -> StageConstraint {
        self.nodes
            .get(&node)
            .map(|instance| instance.stage)
            .unwrap_or_default()
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
        let mut resources: Vec<(WxslIdent, ValueType, Option<BakeDecl>)> = Vec::new();
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
                    if !resources.iter().any(|(existing, _, _)| *existing == name) {
                        resources.push((name, socket.ty, None));
                    }
                }
                NodeBody::UserRead => reads_user = true,
                _ => {}
            }
        }

        // The bake tables, each as a texture and its own sampler — the
        // declaration supplies both names, so the host can create and
        // bind them without knowing the node ids. Only bakes whose node is
        // reachable join in, exactly as for a parked `texture.texture_2d`:
        // a parked branch declares nothing, and emits nothing.
        for decl in &self.bakes {
            if !reachable.contains(&decl.node) {
                continue;
            }
            let (Some(texture), Some(sampler)) = (
                WxslIdent::new(decl.texture.trim()),
                WxslIdent::new(&format!("{}_sampler", decl.texture.trim())),
            ) else {
                // Malformed names are `validate`'s to report; the interface
                // skips them the way it skips a declaring node with an
                // unusable name.
                continue;
            };
            if !resources
                .iter()
                .any(|(existing, _, _)| *existing == texture)
            {
                // The sampler is the bake's too: a host binding a bake's
                // resources looks for the flag, and both halves of one
                // table should answer to it.
                resources.push((texture, ValueType::Texture2d, Some(decl.clone())));
                resources.push((sampler, ValueType::Sampler, Some(decl.clone())));
            }
        }

        // Name order, so adding a texture cannot renumber the ones already
        // there — a binding index that moves is a bind group that has to
        // be rebuilt for no reason.
        resources.sort_by(|(a, _, _), (b, _, _)| a.as_str().cmp(b.as_str()));
        let resources = resources
            .into_iter()
            .enumerate()
            .map(|(index, (name, ty, bake))| ResourceBinding {
                name,
                ty,
                binding: abi::MATERIAL_RESOURCE_BINDING_BASE + index as u32,
                bake,
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
    pub(crate) fn declared_attributes(
        &self,
        frequency: AttributeFrequency,
    ) -> Vec<(WxslIdent, ValueType)> {
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
    /// `Graph::refresh_generics` for exactly what "constrained"
    /// means and the one case (a chain of generic nodes) it deliberately
    /// does not chase through.
    pub fn disconnect(&mut self, registry: &NodeRegistry, input: &SocketRef) -> Option<Edge> {
        let index = self.edges.iter().position(|edge| &edge.to == input)?;
        let edge = self.edges.remove(index);
        self.refresh_generics(registry, edge.to.node);
        self.refresh_generics(registry, edge.from.node);
        Some(edge)
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

    /// [`Graph::dependencies_of`], except the walk does not descend
    /// through a node in `stops` — a value the *other* stage computes and
    /// hands over, so its own inputs are none of this partition's
    /// business. `stops` themselves are in the set; their inputs are not.
    pub(crate) fn dependencies_stopping_at(
        &self,
        root: NodeId,
        stops: &BTreeSet<NodeId>,
    ) -> BTreeSet<NodeId> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if !seen.insert(node) {
                continue;
            }
            if stops.contains(&node) && node != root {
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

    /// Every screen-output node. Exactly one is valid, in a screen graph.
    pub fn screen_outputs(&self, registry: &NodeRegistry) -> Vec<NodeId> {
        self.terminals(registry, NodeDefinition::is_screen_output)
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
