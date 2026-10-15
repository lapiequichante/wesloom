//! Commands and queries: the editor's single mutation path, and its
//! read-only counterpart (ADR 0081).
//!
//! Every change the UI makes to a document is a [`Command`], applied by
//! [`Editor::apply`](crate::Editor::apply) after the frame's widgets have
//! run. Tests and agents use the same vocabulary. `history` holds the undo
//! side.

use std::fmt;

use serde::{Deserialize, Serialize};
use wxsl_core::{
    Edge, Graph, GraphError, MacroValue, Node, NodeId, NodeRegistry, SocketRef, Value, ValueType,
};

use crate::canvas;
use crate::pipeline::CanvasMode;

/// Which of the editor's documents a graph command edits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Document {
    /// The material graph.
    #[default]
    Material,
    /// The pipeline document (ADR 0043). A command naming it opens the
    /// pipeline canvas's document if it was never opened.
    Pipeline,
}

/// One change to the editor's documents.
///
/// Serialized with a `"command"` tag, so a transcript line reads
/// `{"command":"set_param","node":3,"socket":"a","value":{"f32":0.5}}`. A
/// graph command's `document` defaults to the material.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Command {
    /// Add a node, its generics at their defaults and its inputs fed
    /// (`Graph::add_resolved`), so a node dropped on a canvas arrives
    /// complete rather than reporting an unresolved parameter. The outcome
    /// names the new id.
    AddNode {
        /// The document to edit.
        #[serde(default)]
        document: Document,
        /// The node: its definition, and whatever it pins.
        node: Node,
    },
    /// Remove nodes and every edge touching them.
    RemoveNodes {
        /// The document to edit.
        #[serde(default)]
        document: Document,
        /// The nodes.
        nodes: Vec<NodeId>,
    },
    /// Connect an output to an input, replacing whatever fed the input.
    Connect {
        /// The document to edit.
        #[serde(default)]
        document: Document,
        /// The output.
        from: SocketRef,
        /// The input.
        to: SocketRef,
    },
    /// Remove the edge feeding an input.
    Disconnect {
        /// The document to edit.
        #[serde(default)]
        document: Document,
        /// The input.
        to: SocketRef,
    },
    /// Pin a value on an input.
    SetParam {
        /// The document to edit.
        #[serde(default)]
        document: Document,
        /// The node.
        node: NodeId,
        /// The input's name.
        socket: String,
        /// The value.
        value: Value,
    },
    /// Set one of a node's settings.
    SetSetting {
        /// The document to edit.
        #[serde(default)]
        document: Document,
        /// The node.
        node: NodeId,
        /// The setting's name.
        name: String,
        /// Its value.
        value: String,
    },
    /// Pick a generic parameter's type, dropping the edges that no longer
    /// fit (`Graph::set_generic`). The outcome lists them.
    SetGeneric {
        /// The document to edit.
        #[serde(default)]
        document: Document,
        /// The node.
        node: NodeId,
        /// The parameter's name.
        param: String,
        /// The type.
        ty: ValueType,
    },
    /// Pin a macro variable, or with `None` go back to its default.
    SetMacro {
        /// The document to edit.
        #[serde(default)]
        document: Document,
        /// The variable's name.
        name: String,
        /// The value.
        value: Option<MacroValue>,
    },
    /// Put nodes at graph-space positions.
    MoveNodes {
        /// The document to edit.
        #[serde(default)]
        document: Document,
        /// Each node and its new position.
        positions: Vec<(NodeId, [f32; 2])>,
    },
    /// Name a node, or with `None` show its definition's label.
    SetLabel {
        /// The document to edit.
        #[serde(default)]
        document: Document,
        /// The node.
        node: NodeId,
        /// The name.
        label: Option<String>,
    },
    /// Colour a node's title strip, or with `None` use the default.
    SetColor {
        /// The document to edit.
        #[serde(default)]
        document: Document,
        /// The node.
        node: NodeId,
        /// Linear RGB.
        color: Option<[f32; 3]>,
    },
    /// Rename the graph.
    SetName {
        /// The document to edit.
        #[serde(default)]
        document: Document,
        /// The name.
        name: String,
    },
    /// Lay every node out again.
    TidyLayout {
        /// The document to edit.
        #[serde(default)]
        document: Document,
    },
    /// Set a material parameter: a buffer write, no recompile. Every
    /// `param` node declaring `name` takes the value as its default.
    SetMaterialParam {
        /// The parameter's name.
        name: String,
        /// The value.
        value: Value,
    },
    /// Tune a running pass's effect parameter (ADR 0042): a buffer write.
    /// The pass is addressed by its label.
    SetPassParam {
        /// The pass's label.
        pass: String,
        /// The effect parameter's name.
        name: String,
        /// The value.
        value: Value,
    },
    /// Open a `.scene.json`, or import a `.gltf`/`.glb` as the scene,
    /// replacing the open one (ADR 0082).
    OpenScene {
        /// The file.
        path: String,
    },
    /// Import a `.gltf`/`.glb` into the open scene, or as the scene when
    /// none is open. Needs the `gltf` feature.
    ImportGltf {
        /// The file.
        path: String,
    },
    /// Close the scene. The material canvas goes back to the graph it had
    /// before a scene material was opened.
    CloseScene,
    /// Write the scene as a `.scene.json`. Not an undo step.
    SaveScene {
        /// The file.
        path: String,
    },
    /// Select an instance, or with `None` nothing, and open its material
    /// in the material canvas. Not an undo step.
    SelectInstance {
        /// The instance, by index.
        instance: Option<usize>,
    },
    /// Set an instance's transform, relative to its node.
    SetTransform {
        /// The instance.
        instance: usize,
        /// Column-major.
        transform: [f32; 16],
    },
    /// Draw an instance with another of the scene's materials.
    SetInstanceMaterial {
        /// The instance.
        instance: usize,
        /// The material, by index.
        material: usize,
    },
    /// Rename an instance.
    SetInstanceName {
        /// The instance.
        instance: usize,
        /// The name.
        name: String,
    },
    /// Replace a scene material's tags.
    SetMaterialTags {
        /// The material.
        material: usize,
        /// The tags.
        tags: Vec<String>,
    },
    /// Set a scene material's `render_order`.
    SetRenderOrder {
        /// The material.
        material: usize,
        /// The order.
        order: i32,
    },
    /// Set a scene material's `max_layers`.
    SetMaxLayers {
        /// The material.
        material: usize,
        /// The layers.
        layers: i32,
    },
    /// Set the colour and intensity of one of the scene's lights.
    SetLight {
        /// The light, by index in the environment block.
        light: usize,
        /// Linear RGB.
        color: [f32; 3],
        /// Its intensity.
        intensity: f32,
    },
    /// Remove instances.
    RemoveInstances {
        /// The instances.
        instances: Vec<usize>,
    },
    /// Point the panels at another canvas. Session state, like the
    /// commands below it: not an undo step.
    SetCanvasMode {
        /// The canvas.
        mode: CanvasMode,
    },
    /// Preview through a stock pipeline by name (`forward`, `deferred`),
    /// or through the pipeline canvas's document with `document`. Lands at
    /// the next [`Editor::sync`](crate::Editor::sync), which has the
    /// device.
    SetPipeline {
        /// The pipeline.
        pipeline: String,
    },
    /// Show one of the lighting pass's debug views by label, or `shaded`
    /// to shade again ([`Query::View`] lists them).
    SetDebugView {
        /// The view's label.
        view: String,
    },
    /// Step through the running pass list (plan3 N9): run its first
    /// `passes` and show the resource labelled `show`, or what the last of
    /// them wrote. With no `passes`, run live again.
    StepFrame {
        /// How many passes run.
        #[serde(default)]
        passes: Option<usize>,
        /// The resource to show, by label.
        #[serde(default)]
        show: Option<String>,
    },
    /// Apply commands in order, as one undo step. If one fails, nothing
    /// changes.
    Batch {
        /// The commands.
        commands: Vec<Command>,
    },
    /// Undo the last change.
    Undo,
    /// Redo the last undone change.
    Redo,
}

impl Command {
    /// Read a transcript: one JSON command per line. Blank lines and lines
    /// starting with `//` are skipped.
    pub fn parse_transcript(text: &str) -> Result<Vec<Command>, TranscriptError> {
        text.lines()
            .enumerate()
            .filter(|(_, line)| {
                let line = line.trim();
                !line.is_empty() && !line.starts_with("//")
            })
            .map(|(index, line)| {
                serde_json::from_str(line).map_err(|error| TranscriptError {
                    line: index + 1,
                    message: error.to_string(),
                })
            })
            .collect()
    }

    /// Write commands as a transcript, one per line.
    pub fn to_transcript(commands: &[Command]) -> String {
        commands
            .iter()
            .map(|command| {
                let mut line = serde_json::to_string(command).expect("a command always serializes");
                line.push('\n');
                line
            })
            .collect()
    }

    /// What a gesture repeating this command edits. Two consecutive
    /// commands with the same key, during one gesture, are one undo step.
    pub(crate) fn merge_key(&self) -> Option<String> {
        Some(match self {
            Command::MoveNodes {
                document,
                positions,
            } => {
                let ids: Vec<u32> = positions.iter().map(|(id, _)| id.0).collect();
                format!("move {document:?} {ids:?}")
            }
            Command::SetParam {
                document,
                node,
                socket,
                ..
            } => format!("param {document:?} {node} {socket}"),
            Command::SetLabel { document, node, .. } => format!("label {document:?} {node}"),
            Command::SetColor { document, node, .. } => format!("color {document:?} {node}"),
            Command::SetMaterialParam { name, .. } => format!("material {name}"),
            Command::SetPassParam { pass, name, .. } => format!("pass {pass} {name}"),
            Command::SetTransform { instance, .. } => format!("transform {instance}"),
            Command::SetInstanceName { instance, .. } => format!("instance name {instance}"),
            Command::SetMaterialTags { material, .. } => format!("tags {material}"),
            Command::SetRenderOrder { material, .. } => format!("order {material}"),
            Command::SetMaxLayers { material, .. } => format!("layers {material}"),
            Command::SetLight { light, .. } => format!("light {light}"),
            _ => return None,
        })
    }

    /// Whether the command names `document`, so its state is kept before
    /// the command runs.
    pub(crate) fn mentions(&self, which: Document) -> bool {
        match self {
            Command::AddNode { document, .. }
            | Command::RemoveNodes { document, .. }
            | Command::Connect { document, .. }
            | Command::Disconnect { document, .. }
            | Command::SetParam { document, .. }
            | Command::SetSetting { document, .. }
            | Command::SetGeneric { document, .. }
            | Command::SetMacro { document, .. }
            | Command::MoveNodes { document, .. }
            | Command::SetLabel { document, .. }
            | Command::SetColor { document, .. }
            | Command::SetName { document, .. }
            | Command::TidyLayout { document } => *document == which,
            Command::SetMaterialParam { .. } => which == Document::Material,
            Command::Batch { commands } => commands.iter().any(|command| command.mentions(which)),
            _ => false,
        }
    }

    /// Whether the command changes what a document compiles to, rather
    /// than only its layout or names.
    pub(crate) fn rebuilds(&self) -> bool {
        !matches!(
            self,
            Command::MoveNodes { .. }
                | Command::SetLabel { .. }
                | Command::SetColor { .. }
                | Command::TidyLayout { .. }
                | Command::SetMaterialParam { .. }
                | Command::SetPassParam { .. }
        )
    }

    /// Whether the command changes session state the history does not
    /// keep: the canvas, the preview's pipeline, its debug view, stepping.
    pub(crate) fn is_view(&self) -> bool {
        matches!(
            self,
            Command::SetCanvasMode { .. }
                | Command::SetPipeline { .. }
                | Command::SetDebugView { .. }
                | Command::StepFrame { .. }
        )
    }

    /// The command's tag, as a transcript spells it.
    pub(crate) fn name(&self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|value| value.get("command")?.as_str().map(str::to_string))
            .unwrap_or_default()
    }
}

/// What a command did beyond the change it names.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Outcome {
    /// The nodes `add_node` created, in order.
    pub added: Vec<NodeId>,
    /// Edges removed as a side effect: the edge a `connect` replaced, or
    /// those a `set_generic` dropped.
    pub disconnected: Vec<Edge>,
}

/// Why a command was refused. A refused command changes nothing.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CommandError {
    /// The graph refused the edit.
    Graph(GraphError),
    /// No node has this id.
    NoSuchNode(NodeId),
    /// No definition has this id.
    UnknownDefinition(String),
    /// The node's definition has no input of this name.
    UnknownSocket(SocketRef),
    /// The node's definition has no setting of this name.
    UnknownSetting {
        /// The node.
        node: NodeId,
        /// The setting.
        name: String,
    },
    /// Nothing feeds this input.
    NotConnected(SocketRef),
    /// No `param` node declares this material parameter.
    UnknownMaterialParam(String),
    /// The renderer refused a parameter write.
    Render(String),
    /// The history is empty.
    NothingToUndo,
    /// Nothing has been undone since the last change.
    NothingToRedo,
    /// `undo` and `redo` are steps of their own, never part of a batch.
    HistoryInBatch,
    /// The command needs a scene and none is open.
    NoScene,
    /// The scene has no instance at this index.
    NoSuchInstance(usize),
    /// The scene has no material at this index.
    NoSuchMaterial(usize),
    /// The scene's environment has no light at this index.
    NoSuchLight(usize),
    /// A scene file could not be read, imported or written.
    Scene(String),
    /// No stock pipeline has this name.
    NoSuchPipeline(String),
    /// No debug view has this label.
    NoSuchDebugView(String),
    /// The pass list being stepped writes no resource of this label.
    NoSuchResource(String),
    /// A command inside a batch failed.
    InBatch {
        /// Its index in the batch.
        index: usize,
        /// What it failed with.
        error: Box<CommandError>,
    },
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandError::Graph(error) => write!(f, "{error}"),
            CommandError::NoSuchNode(id) => write!(f, "no node {id}"),
            CommandError::UnknownDefinition(id) => write!(f, "no node definition `{id}`"),
            CommandError::UnknownSocket(socket) => write!(f, "no input {socket}"),
            CommandError::UnknownSetting { node, name } => {
                write!(f, "node {node} has no setting `{name}`")
            }
            CommandError::NotConnected(socket) => write!(f, "nothing feeds {socket}"),
            CommandError::UnknownMaterialParam(name) => {
                write!(f, "no param node declares `{name}`")
            }
            CommandError::Render(message) => f.write_str(message),
            CommandError::NothingToUndo => f.write_str("nothing to undo"),
            CommandError::NothingToRedo => f.write_str("nothing to redo"),
            CommandError::HistoryInBatch => f.write_str("undo and redo cannot be batched"),
            CommandError::NoScene => f.write_str("no scene is open"),
            CommandError::NoSuchInstance(index) => write!(f, "no instance {index}"),
            CommandError::NoSuchMaterial(index) => write!(f, "no scene material {index}"),
            CommandError::NoSuchLight(index) => write!(f, "no light {index}"),
            CommandError::Scene(message) => f.write_str(message),
            CommandError::NoSuchPipeline(name) => write!(
                f,
                "no pipeline `{name}`; one of forward, deferred or document"
            ),
            CommandError::NoSuchDebugView(name) => {
                write!(f, "no debug view `{name}`; the view query lists them")
            }
            CommandError::NoSuchResource(name) => {
                write!(f, "the stepped passes write no resource `{name}`")
            }
            CommandError::InBatch { index, error } => {
                write!(f, "batch command {index}: {error}")
            }
        }
    }
}

impl std::error::Error for CommandError {}

impl From<GraphError> for CommandError {
    fn from(error: GraphError) -> Self {
        CommandError::Graph(error)
    }
}

/// A transcript line that is not a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptError {
    /// The line, counting from 1.
    pub line: usize,
    /// What the parser said.
    pub message: String,
}

impl fmt::Display for TranscriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "transcript line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for TranscriptError {}

/// A read-only question about the editor's state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Query {
    /// A document's graph. The pipeline's is absent until it is opened.
    Graph {
        /// Which document.
        #[serde(default)]
        document: Document,
    },
    /// What stops the material, the pipeline document or a hot reload.
    Diagnostics,
    /// The material's generated WXSL and its WGSL.
    Source,
    /// The node definitions a document can use, optionally filtered by a
    /// case-insensitive substring of their id, label or category.
    Nodes {
        /// Which document's library.
        #[serde(default)]
        document: Document,
        /// The filter.
        #[serde(default)]
        search: Option<String>,
    },
    /// The running pass list.
    Passes,
    /// Sizes and counters.
    Stats,
    /// How many steps can be undone and redone.
    History,
    /// The open scene document, and the selected instance.
    Scene,
    /// The session state the view commands set: the canvas, the
    /// preview's pipeline, its debug view and the pass stepping.
    View,
}

/// The answer to a [`Query`].
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Answer {
    /// The graph, if the document exists.
    Graph {
        /// The graph.
        graph: Option<Graph>,
    },
    /// Every current problem, as text.
    Diagnostics {
        /// The material's compile errors.
        material: Vec<String>,
        /// The pipeline document's compile errors.
        pipeline: Vec<String>,
        /// The last hot reload's errors.
        reload: Vec<String>,
    },
    /// The generated code.
    Source {
        /// WXSL, as generated from the graph.
        wxsl: String,
        /// The WGSL it compiled to.
        wgsl: String,
    },
    /// Node definitions.
    Nodes {
        /// One per definition, in registry order.
        nodes: Vec<NodeSummary>,
    },
    /// The running passes.
    Passes {
        /// One per pass, in order.
        passes: Vec<PassSummary>,
    },
    /// Sizes and counters.
    Stats {
        /// Nodes in the material graph.
        nodes: usize,
        /// Edges in the material graph.
        edges: usize,
        /// Shader variants compiled so far.
        variants: usize,
        /// The last measured frame's GPU time, if timestamps exist.
        frame_ms: Option<f32>,
    },
    /// Undo depth.
    History {
        /// Steps that can be undone.
        undo: usize,
        /// Steps that can be redone.
        redo: usize,
        /// The steps that can be undone, oldest first.
        steps: Vec<HistoryStep>,
    },
    /// The open scene.
    Scene {
        /// The document, if a scene is open.
        scene: Option<wxsl_core::scene::Scene>,
        /// The selected instance.
        selected: Option<usize>,
        /// The material open in the material canvas, by index.
        material: Option<usize>,
    },
    /// The session state.
    View {
        /// The canvas the panels point at.
        mode: CanvasMode,
        /// The preview's pipeline: a stock name, or `document`.
        pipeline: String,
        /// The debug view showing, `shaded` when none.
        debug_view: String,
        /// The debug views the running plan can show, by label.
        debug_views: Vec<String>,
        /// The pass list being stepped through, if it is.
        stepping: Option<StepSummary>,
    },
}

/// One undo step, as a [`Query::History`] answer lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct HistoryStep {
    /// The command that made it, by tag; a merged gesture names its
    /// first command.
    pub command: String,
    /// Whether an agent applied it ([`Editor::apply_as_agent`](crate::Editor::apply_as_agent)).
    pub agent: bool,
}

/// The pass stepping, as a [`Query::View`] answer shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct StepSummary {
    /// Every pass label, in schedule order.
    pub passes: Vec<String>,
    /// How many of them run.
    pub count: usize,
    /// The resources the running passes write that can be shown, by
    /// label.
    pub resources: Vec<String>,
    /// The resource showing, by label: `None` for the frame's target.
    pub shown: Option<String>,
}

/// A node definition, as a [`Query::Nodes`] answer lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct NodeSummary {
    /// The registry id (`package.name`).
    pub id: String,
    /// Its label.
    pub label: String,
    /// Its category.
    pub category: String,
    /// Its documentation.
    pub doc: String,
    /// Its inputs.
    pub inputs: Vec<SocketSummary>,
    /// Its outputs.
    pub outputs: Vec<SocketSummary>,
    /// Its settings' names.
    pub settings: Vec<String>,
}

/// One socket of a [`NodeSummary`].
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct SocketSummary {
    /// Its name.
    pub name: String,
    /// Its type; a placeholder when `generic` is set.
    pub ty: ValueType,
    /// The generic parameter it follows, if any.
    pub generic: Option<String>,
    /// Its default value, if it has one.
    pub default: Option<Value>,
}

/// One pass, as a [`Query::Passes`] answer lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct PassSummary {
    /// The pass's label.
    pub label: String,
    /// When it runs.
    pub policy: String,
    /// Its effect parameters and their current values.
    pub parameters: Vec<(String, Value)>,
}

impl NodeSummary {
    pub(crate) fn of(definition: &wxsl_core::node::NodeDefinition) -> Self {
        let sockets = |sockets: &[wxsl_core::node::Socket]| {
            sockets
                .iter()
                .map(|socket| SocketSummary {
                    name: socket.name.to_string(),
                    ty: socket.ty,
                    generic: socket.generic.as_ref().map(ToString::to_string),
                    default: socket.default,
                })
                .collect()
        };
        NodeSummary {
            id: definition.id.clone(),
            label: definition.label.clone(),
            category: definition.category.clone(),
            doc: definition.doc.clone(),
            inputs: sockets(&definition.inputs),
            outputs: sockets(&definition.outputs),
            settings: definition
                .settings
                .iter()
                .map(|setting| setting.name.to_string())
                .collect(),
        }
    }
}

/// Apply a graph command to `graph`. Anything else is the editor's to
/// apply. On error the graph may be half changed; the caller restores it.
pub(crate) fn edit_graph(
    graph: &mut Graph,
    registry: &NodeRegistry,
    command: &Command,
    outcome: &mut Outcome,
) -> Result<(), CommandError> {
    let exists = |graph: &Graph, node: NodeId| {
        graph
            .node(node)
            .map(|_| ())
            .ok_or(CommandError::NoSuchNode(node))
    };
    match command {
        Command::AddNode { node, .. } => {
            if registry.get(&node.def).is_none() {
                return Err(CommandError::UnknownDefinition(node.def.clone()));
            }
            outcome
                .added
                .push(graph.add_resolved(registry, node.clone()));
        }
        Command::RemoveNodes { nodes, .. } => {
            for &node in nodes {
                graph
                    .remove_node(registry, node)
                    .ok_or(CommandError::NoSuchNode(node))?;
            }
        }
        Command::Connect { from, to, .. } => {
            let previous = graph.disconnect(registry, to);
            graph.connect(registry, from.clone(), to.clone())?;
            outcome
                .disconnected
                .extend(previous.filter(|edge| edge.from != *from));
        }
        Command::Disconnect { to, .. } => {
            graph
                .disconnect(registry, to)
                .ok_or_else(|| CommandError::NotConnected(to.clone()))?;
        }
        Command::SetParam {
            node,
            socket,
            value,
            ..
        } => {
            let definition = graph.definition(registry, *node)?;
            if definition.input(socket).is_none() {
                return Err(CommandError::UnknownSocket(SocketRef::new(
                    *node,
                    socket.as_str(),
                )));
            }
            graph.set_param(*node, socket.as_str(), *value);
        }
        Command::SetSetting {
            node, name, value, ..
        } => {
            let definition = graph.definition(registry, *node)?;
            if !definition
                .settings
                .iter()
                .any(|setting| setting.name.as_str() == name)
            {
                return Err(CommandError::UnknownSetting {
                    node: *node,
                    name: name.clone(),
                });
            }
            graph.set_setting(*node, name.as_str(), value.as_str());
        }
        Command::SetGeneric {
            node, param, ty, ..
        } => {
            let dropped = graph.set_generic(registry, *node, param, *ty)?;
            outcome.disconnected.extend(dropped);
        }
        Command::SetMacro { name, value, .. } => match value {
            Some(value) => {
                graph.set_macro(name.as_str(), *value);
            }
            None => {
                graph.unset_macro(name);
            }
        },
        Command::MoveNodes { positions, .. } => {
            for &(node, position) in positions {
                exists(graph, node)?;
                if let Some(node) = graph.node_mut(node) {
                    node.position = Some(position);
                }
            }
        }
        Command::SetLabel { node, label, .. } => {
            exists(graph, *node)?;
            if let Some(node) = graph.node_mut(*node) {
                node.label = label.clone();
            }
        }
        Command::SetColor { node, color, .. } => {
            exists(graph, *node)?;
            if let Some(node) = graph.node_mut(*node) {
                node.color = *color;
            }
        }
        Command::SetName { name, .. } => graph.set_name(name.as_str()),
        Command::TidyLayout { .. } => canvas::tidy_layout(graph, registry),
        Command::SetMaterialParam { name, value } => {
            let declaring: Vec<NodeId> = graph
                .nodes()
                .filter(|(id, node)| {
                    registry.get(&node.def).is_some_and(|definition| {
                        matches!(definition.body, wxsl_core::node::NodeBody::Param)
                    }) && graph.setting(registry, *id, wxsl_core::node::SETTING_NAME)
                        == Some(name.as_str())
                })
                .map(|(id, _)| id)
                .collect();
            if declaring.is_empty() {
                return Err(CommandError::UnknownMaterialParam(name.clone()));
            }
            for id in declaring {
                graph.set_param(id, wxsl_core::node::SOCKET_VALUE, *value);
            }
        }
        _ => unreachable!("not a graph command: the editor applies it"),
    }
    Ok(())
}
