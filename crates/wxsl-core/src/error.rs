//! Error types for graph validation ([`GraphError`]) and code generation
//! ([`CodegenError`]).
//!
//! Both are hand-written `std::error::Error` implementations: this crate
//! deliberately has no error-handling dependency, per the "keep
//! `wxsl-core` dependency-light" rule in `AGENTS.md`.

use core::fmt;

use crate::graph::{NodeId, SocketRef};
use crate::macros::MacroValue;
use crate::node::ValueType;

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
