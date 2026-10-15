//! The on-disk shape of a [`super::Graph`].
//!
//! Kept separate from the in-memory type so the file format stays a
//! deliberate, readable thing (a list of nodes with ids, not a map keyed
//! by stringified integers) and so `next_id` — an implementation detail —
//! is recomputed on load rather than trusted from the file.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    AttributeDecl, BakeDecl, Edge, Graph, GraphDomain, Node, NodeId, UserBlockDecl, SCHEMA_VERSION,
};
use crate::abi;
use crate::error::check_document_version;
use crate::macros::MacroSet;

/// What an absent `version` field means: the file was written before
/// versioning, and its shape is this build's.
fn default_version() -> u32 {
    super::SCHEMA_VERSION
}

/// What an absent `abi` field means: the file predates the pin, and
/// its vocabulary is this build's.
fn default_abi() -> u32 {
    crate::abi::REVISION
}

#[derive(Serialize, Deserialize)]
pub(super) struct WireNode {
    pub id: NodeId,
    #[serde(flatten)]
    pub node: Node,
}

#[derive(Serialize, Deserialize)]
pub(super) struct WireGraph {
    /// The document format version this file was written as; absent in
    /// files from before versioning, which read as this build's.
    #[serde(default = "default_version")]
    pub version: u32,
    /// The [`abi::REVISION`] the document pins: what its node ids,
    /// sockets and macros are written against.
    #[serde(default = "default_abi")]
    pub abi: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "GraphDomain::is_surface")]
    pub domain: GraphDomain,
    #[serde(default, skip_serializing_if = "MacroSet::is_empty")]
    pub macros: MacroSet,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_block: Option<UserBlockDecl>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<AttributeDecl>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bakes: Vec<BakeDecl>,
    #[serde(default)]
    pub nodes: Vec<WireNode>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

impl From<Graph> for WireGraph {
    fn from(graph: Graph) -> Self {
        WireGraph {
            version: SCHEMA_VERSION,
            abi: abi::REVISION,
            name: graph.name,
            domain: graph.domain,
            macros: graph.macros,
            user_block: graph.user_block,
            attributes: graph.attributes,
            bakes: graph.bakes,
            nodes: graph
                .nodes
                .into_iter()
                .map(|(id, node)| WireNode { id, node })
                .collect(),
            edges: graph.edges,
        }
    }
}

impl TryFrom<WireGraph> for Graph {
    // A `String`, because serde's `try_from` support turns whatever the
    // conversion says into the parse error the caller already handles;
    // the message is the named diagnostic.
    type Error = String;

    fn try_from(wire: WireGraph) -> Result<Self, Self::Error> {
        check_document_version(wire.version, SCHEMA_VERSION, wire.abi, "graph")?;
        let nodes: BTreeMap<NodeId, Node> = wire
            .nodes
            .into_iter()
            .map(|entry| (entry.id, entry.node))
            .collect();
        let next_id = nodes.keys().map(|id| id.0).max().unwrap_or(0) + 1;
        Ok(Graph {
            name: wire.name,
            domain: wire.domain,
            nodes,
            edges: wire.edges,
            macros: wire.macros,
            user_block: wire.user_block,
            attributes: wire.attributes,
            bakes: wire.bakes,
            next_id,
        })
    }
}
