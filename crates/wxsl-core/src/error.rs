//! Error types for graph validation ([`GraphError`]) and code generation
//! ([`CodegenError`]).
//!
//! Both are hand-written `std::error::Error` implementations: this crate
//! deliberately has no error-handling dependency, per the "keep
//! `wxsl-core` dependency-light" rule in `AGENTS.md`.

use core::fmt;

use crate::graph::{NodeId, SocketRef};
use crate::macros::MacroValue;
use crate::node::{TypeRule, ValueType};

/// A single problem found while validating a [`crate::graph::Graph`].
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum GraphError {
    /// A node references a definition that is not in the registry.
    UnknownDefinition {
        /// The offending node.
        node: NodeId,
        /// The definition id the node asked for.
        def: String,
    },
    /// An edge or parameter references a node that is not in the graph.
    UnknownNode(NodeId),
    /// A node has no socket with the requested name.
    UnknownSocket {
        /// The socket that could not be resolved.
        socket: SocketRef,
        /// Whether an input or an output socket was being looked up.
        direction: Direction,
    },
    /// The two ends of an edge disagree about the value type.
    TypeMismatch {
        /// Producing socket.
        from: SocketRef,
        /// Consuming socket.
        to: SocketRef,
        /// Type produced by `from`.
        produced: ValueType,
        /// Type expected by `to`.
        expected: ValueType,
    },
    /// An input socket already has an incoming edge; inputs take exactly one.
    InputAlreadyConnected {
        /// The socket that is already driven.
        socket: SocketRef,
    },
    /// Connecting these two sockets would introduce a cycle, so the
    /// connection was rejected: the graph is acyclic by construction.
    WouldCycle {
        /// Producing socket.
        from: SocketRef,
        /// Consuming socket.
        to: SocketRef,
    },
    /// The graph contains a cycle. Only reachable for a graph that was not
    /// built through [`crate::graph::Graph::connect`] (e.g. deserialized).
    Cycle {
        /// Nodes that take part in the cycle, in traversal order.
        nodes: Vec<NodeId>,
    },
    /// A parameter value has a different type than the socket it overrides.
    ParamTypeMismatch {
        /// The socket the parameter is for.
        socket: SocketRef,
        /// Type of the supplied value.
        supplied: ValueType,
        /// Type the socket declares.
        expected: ValueType,
    },
    /// A node parameter names a socket the node does not have.
    UnknownParam {
        /// The node carrying the parameter.
        node: NodeId,
        /// The parameter name.
        param: String,
    },
    /// An input socket is neither connected nor given a value, and its
    /// definition provides no default.
    MissingInput {
        /// The unfed socket.
        socket: SocketRef,
    },
    /// Two nodes declare the same macro with different defaults and the graph
    /// does not pin the value. Set it explicitly with
    /// [`crate::graph::Graph::set_macro`] to resolve the ambiguity.
    ConflictingMacroDefault {
        /// The macro name.
        name: String,
        /// One declared default.
        first: MacroValue,
        /// The other declared default.
        second: MacroValue,
    },
    /// A macro name pinned by the graph is not a valid WXSL identifier, so it
    /// could be neither declared as a const nor named in an `@if`. Only
    /// reachable from a hand-edited or generated graph file.
    InvalidMacroName {
        /// The offending name.
        name: String,
    },
    /// The graph-level value for a macro has a different kind than the
    /// declaration it overrides (e.g. a flag pinned to a float).
    MacroKindMismatch {
        /// The macro name.
        name: String,
        /// The value the graph pins.
        supplied: MacroValue,
        /// The value the declaration defaults to.
        declared: MacroValue,
    },
    /// A socket is generic over a type parameter that this node instance has
    /// not resolved: no edge reaching it (directly or transitively, through
    /// other unresolved generic sockets) settles on a concrete type, and
    /// nothing pinned it with [`crate::graph::Graph::set_generic`]. Codegen
    /// has no concrete WGSL type to emit for it — the generic equivalent of
    /// [`GraphError::MissingInput`].
    UnresolvedGeneric {
        /// The node whose parameter is unresolved.
        node: NodeId,
        /// The parameter's name.
        param: String,
    },
    /// [`crate::graph::Graph::set_generic`] was asked to resolve a parameter
    /// to a type its declaration does not allow.
    InvalidGenericType {
        /// The node.
        node: NodeId,
        /// The parameter's name.
        param: String,
        /// The type that was requested.
        ty: ValueType,
        /// The types the parameter actually allows.
        allowed: Vec<ValueType>,
    },
    /// A socket's type is derived from two generic parameters by a
    /// [`crate::node::TypeRule`] (see [`crate::node::Socket::combine`]), and both
    /// are resolved but do not combine — e.g. `vec2f` and `vec3f`, neither a
    /// scalar nor matching the other. Codegen has no type to emit for the
    /// socket; the graph-level analogue of [`GraphError::TypeMismatch`], for
    /// a type that is derived from two operands rather than read off one
    /// edge.
    IncompatibleGenerics {
        /// The node.
        node: NodeId,
        /// The socket whose type could not be derived.
        socket: String,
        /// The rule that failed to combine the two, whose
        /// [`crate::node::TypeRule::requirement`] explains what it wanted.
        rule: TypeRule,
        /// The first parameter's name and resolved type.
        a: (String, ValueType),
        /// The second parameter's name and resolved type.
        b: (String, ValueType),
    },
}

/// Whether a socket lookup was for an input or an output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// A node input (consumes a value).
    Input,
    /// A node output (produces a value).
    Output,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Direction::Input => f.write_str("input"),
            Direction::Output => f.write_str("output"),
        }
    }
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphError::UnknownDefinition { node, def } => {
                write!(f, "node {node} references unknown node definition `{def}`")
            }
            GraphError::UnknownNode(id) => write!(f, "no such node: {id}"),
            GraphError::UnknownSocket { socket, direction } => write!(
                f,
                "node {} has no {direction} socket `{}`",
                socket.node, socket.socket
            ),
            GraphError::TypeMismatch {
                from,
                to,
                produced,
                expected,
            } => write!(
                f,
                "type mismatch connecting {from} ({produced}) to {to} ({expected})"
            ),
            GraphError::InputAlreadyConnected { socket } => {
                write!(f, "input {socket} is already connected")
            }
            GraphError::WouldCycle { from, to } => {
                write!(f, "connecting {from} to {to} would create a cycle")
            }
            GraphError::Cycle { nodes } => {
                f.write_str("graph contains a cycle: ")?;
                for (i, n) in nodes.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" -> ")?;
                    }
                    write!(f, "{n}")?;
                }
                Ok(())
            }
            GraphError::ParamTypeMismatch {
                socket,
                supplied,
                expected,
            } => write!(
                f,
                "parameter for {socket} has type {supplied}, but the socket is {expected}"
            ),
            GraphError::UnknownParam { node, param } => write!(
                f,
                "node {node} has a parameter `{param}` that matches no input socket"
            ),
            GraphError::MissingInput { socket } => write!(
                f,
                "input {socket} is not connected and has no value or default"
            ),
            GraphError::ConflictingMacroDefault {
                name,
                first,
                second,
            } => write!(
                f,
                "macro `{name}` is declared with conflicting defaults ({first} and {second}); pin it on the graph to resolve"
            ),
            GraphError::InvalidMacroName { name } => {
                write!(f, "macro name `{name}` is not a valid WXSL identifier")
            }
            GraphError::MacroKindMismatch {
                name,
                supplied,
                declared,
            } => write!(
                f,
                "macro `{name}` is declared as {} but the graph pins it to {} ({supplied} vs {declared})",
                declared.kind(),
                supplied.kind(),
            ),
            GraphError::UnresolvedGeneric { node, param } => write!(
                f,
                "node {node} has not resolved its generic parameter `{param}`; connect \
                 something to a socket that uses it, or pick a type explicitly"
            ),
            GraphError::InvalidGenericType {
                node,
                param,
                ty,
                allowed,
            } => {
                write!(
                    f,
                    "node {node}'s generic parameter `{param}` cannot be {ty}; it allows "
                )?;
                for (index, candidate) in allowed.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{candidate}")?;
                }
                Ok(())
            }
            GraphError::IncompatibleGenerics {
                node,
                socket,
                rule,
                a,
                b,
            } => write!(
                f,
                "node {node}'s socket `{socket}` cannot combine `{}` ({}) with `{}` ({}) \
                 as a {rule}: {}",
                a.0,
                a.1,
                b.0,
                b.1,
                rule.requirement()
            ),
        }
    }
}

impl std::error::Error for GraphError {}

/// Every problem found by one [`crate::graph::Graph::validate`] call.
///
/// Validation reports as much as it can in a single pass rather than stopping
/// at the first error, because an editor wants to underline every bad socket
/// at once, not one per fix.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphErrors(pub Vec<GraphError>);

impl GraphErrors {
    /// The first reported error, if any.
    pub fn first(&self) -> Option<&GraphError> {
        self.0.first()
    }

    /// Number of reported errors.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the list is empty (i.e. validation succeeded).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<GraphError> for GraphErrors {
    fn from(value: GraphError) -> Self {
        GraphErrors(vec![value])
    }
}

impl fmt::Display for GraphErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.split_first() {
            None => f.write_str("no errors"),
            Some((first, [])) => write!(f, "{first}"),
            Some((first, rest)) => {
                write!(f, "{} graph errors:\n  - {first}", self.0.len())?;
                for e in rest {
                    write!(f, "\n  - {e}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for GraphErrors {}

/// A problem that stopped [`crate::codegen::generate`] from emitting WXSL.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CodegenError {
    /// The graph did not validate. Codegen always validates first: emitting
    /// WXSL from an invalid graph would just move the error into the shader
    /// compiler, where it is much harder to explain.
    Invalid(GraphErrors),
    /// The graph has no surface-output node, so there is nothing to compile.
    NoOutputNode,
    /// The graph has more than one surface-output node.
    MultipleOutputNodes(Vec<NodeId>),
    /// A node definition's expression template referenced a socket the node
    /// does not declare. A bug in the node definition, not in the graph.
    BadTemplate {
        /// Definition id whose template is wrong.
        def: String,
        /// The placeholder that could not be resolved.
        placeholder: String,
    },
    /// A node definition declares a different number of expressions than
    /// outputs. Also a node-definition bug.
    OutputArityMismatch {
        /// Definition id at fault.
        def: String,
        /// Number of declared outputs.
        outputs: usize,
        /// Number of expressions supplied.
        exprs: usize,
    },
    /// A value could not be written as a WXSL literal (non-finite float).
    UnrepresentableValue {
        /// Where the value came from.
        socket: SocketRef,
    },
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodegenError::Invalid(errors) => write!(f, "graph is invalid: {errors}"),
            CodegenError::NoOutputNode => f.write_str("graph has no surface output node"),
            CodegenError::MultipleOutputNodes(nodes) => {
                write!(
                    f,
                    "graph has {} surface output nodes (expected 1): ",
                    nodes.len()
                )?;
                for (i, n) in nodes.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{n}")?;
                }
                Ok(())
            }
            CodegenError::BadTemplate { def, placeholder } => write!(
                f,
                "node definition `{def}` has an expression template referencing unknown socket `{placeholder}`"
            ),
            CodegenError::OutputArityMismatch {
                def,
                outputs,
                exprs,
            } => write!(
                f,
                "node definition `{def}` declares {outputs} outputs but supplies {exprs} expressions"
            ),
            CodegenError::UnrepresentableValue { socket } => {
                write!(f, "value for {socket} cannot be written as a WXSL literal")
            }
        }
    }
}

impl std::error::Error for CodegenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CodegenError::Invalid(e) => Some(e),
            _ => None,
        }
    }
}

impl From<GraphErrors> for CodegenError {
    fn from(value: GraphErrors) -> Self {
        CodegenError::Invalid(value)
    }
}
