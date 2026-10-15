//! The tools and resources, each a plan over the editor's commands and
//! queries (ADR 0083).
//!
//! [`plan`] runs on the transport's thread with no editor: it parses a
//! call's arguments into an [`Action`], refusing bad ones before they
//! cross to the render thread. [`run`] runs the action against the editor
//! there. A new tool is a row in [`definitions`] and an arm in [`plan`];
//! if it needs something the editor cannot yet do, that is a new
//! `Command` or `Query` first (ADR 0081).

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{json, Map, Value as Json};
use wxsl_core::graph::NodeId;
use wxsl_core::{MacroValue, Value};
use wxsl_editor::{CanvasMode, Command, Document, Editor, Query};
use wxsl_render::gpu::OffscreenTarget;
use wxsl_render::ui::UiTarget;

use crate::image;
use crate::sandbox::Root;
use crate::Frame;

/// How wide a screenshot is at most, unless the call asks otherwise.
const SCREENSHOT_WIDTH: u32 = 1024;

/// The graph commands, which take a `document`.
const GRAPH_COMMANDS: &[&str] = &[
    "add_node",
    "remove_nodes",
    "connect",
    "disconnect",
    "set_param",
    "set_setting",
    "set_generic",
    "set_macro",
    "move_nodes",
    "set_label",
    "set_color",
    "set_name",
    "tidy_layout",
];

/// What a tool call does on the render thread.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    /// Apply an edit, if any, bring the preview up to date, then answer
    /// the queries.
    Run {
        /// The edit.
        edit: Option<Edit>,
        /// The questions, answered after it.
        queries: Vec<Query>,
    },
    /// Save a function's source as a `user.*` node.
    DefineFunction(String),
    /// Draw the editor and return a PNG of it, or of the viewport.
    Screenshot {
        /// The viewport (the preview or the scene) rather than the window.
        viewport: bool,
        /// The widest the image may be.
        max_width: u32,
    },
    /// Read a resource by URI.
    Resource(String),
}

/// An edit, as a tool asks for it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Edit {
    /// One command.
    Command(Command),
    /// Raw commands, applied as one batch once their `$name` node
    /// references are resolved against the document they edit.
    Batch {
        /// The document a command that names none edits.
        document: Document,
        /// The commands, as JSON.
        commands: Vec<Json>,
    },
}

/// Why a call was not planned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlanError {
    /// No tool has the name.
    Unknown(String),
    /// The arguments do not fit the tool.
    Invalid(String),
}

/// What [`run`] answers: a tool result or resource contents, or a
/// JSON-RPC error.
pub(crate) type Reply = Result<Json, (i64, String)>;

/// The JSON-RPC code for a resource that does not exist.
pub(crate) const RESOURCE_NOT_FOUND: i64 = -32002;

/// The format notes shared by the editing tools' descriptions.
const FORMATS: &str = "Values are tagged: {\"f32\":0.5}, {\"vec3\":[1,0,0]}, \
{\"bool\":true}, {\"i32\":2}. Macro values: {\"flag\":true}, {\"int\":2}, {\"float\":0.5}. \
A socket is {\"node\":3,\"socket\":\"out\"}.";

/// Every tool, as `tools/list` returns it.
pub(crate) fn definitions() -> Vec<Json> {
    let document = json!({
        "type": "string",
        "enum": ["material", "pipeline"],
        "description": "Which document: the material graph (default) or the pipeline document."
    });
    let empty = json!({"type": "object", "properties": {}, "additionalProperties": false});
    let commands = json!({
        "type": "array",
        "items": {"type": "object"},
        "description": format!(
            "Editor commands, applied in order as one undo step; if one fails nothing changes. \
    Each is an object tagged by \"command\": add_node {{node:{{def}}}}, remove_nodes {{nodes:[id]}}, \
    connect {{from:socket,to:socket}}, disconnect {{to:socket}}, set_param {{node,socket,value}}, \
    set_setting {{node,name,value:string}}, set_generic {{node,param,ty}}, set_macro {{name,value|null}}, \
    set_label {{node,label}}, set_name {{name}}, tidy_layout, set_material_param {{name,value}}, \
    set_pass_param {{pass,name,value}}. An add_node may carry \"ref\":\"name\"; later commands then \
    write \"$name\" wherever a node id goes. {FORMATS}"
        )
    });
    let tool = |name: &str, description: &str, schema: Json| json!({"name": name, "description": description, "inputSchema": schema});
    vec![
        tool(
            "nodes_search",
            "Search the node library a document can use, by a case-insensitive substring of the \
node's id, label or category. Returns each node's id, doc, inputs (type, default) and outputs.",
            json!({"type": "object", "properties": {
                "search": {"type": "string"},
                "document": document,
            }, "additionalProperties": false}),
        ),
        tool(
            "graph_get",
            "The document's graph: its nodes by id (definition, pinned params, generics, \
settings) and its edges.",
            json!({"type": "object", "properties": {"document": document},
                "additionalProperties": false}),
        ),
        tool(
            "graph_edit",
            "Edit a graph with editor commands, atomically, as one undo step marked as yours. \
Returns what was added (and the ids of named refs) and the diagnostics after recompiling.",
            json!({"type": "object", "properties": {
                "document": document,
                "commands": commands,
            }, "required": ["commands"], "additionalProperties": false}),
        ),
        tool(
            "graph_compile",
            "Recompile the material and return its generated WXSL, the WGSL it compiled to, and \
every diagnostic (material, pipeline document, shader reload).",
            empty.clone(),
        ),
        tool(
            "function_save",
            "Save WXSL source as a node in the `user` package: one `fn`, a label line and a doc \
paragraph above it, one parameter per line each with `// ... @default <value>`. Returns the node \
id, or the compiler's diagnostics by line. The node is then in the library for this session.",
            json!({"type": "object", "properties": {"source": {"type": "string"}},
                "required": ["source"], "additionalProperties": false}),
        ),
        tool(
            "macros_set",
            "Pin a macro variable of a document, or with no value go back to its default.",
            json!({"type": "object", "properties": {
                "name": {"type": "string"},
                "value": {"type": "object", "description": "{\"flag\":true}, {\"int\":2} or {\"float\":0.5}"},
                "document": document,
            }, "required": ["name"], "additionalProperties": false}),
        ),
        tool(
            "pipeline_get",
            "The pipeline document's graph (absent until it is first edited), the running pass \
list with each pass's effect parameters, and which pipeline the preview runs.",
            empty.clone(),
        ),
        tool(
            "pipeline_edit",
            "graph_edit on the pipeline document: pass nodes, resources and effects.",
            json!({"type": "object", "properties": {"commands": commands},
                "required": ["commands"], "additionalProperties": false}),
        ),
        tool(
            "pass_param",
            &format!(
                "Tune a running pass's effect parameter, addressed by the pass's label \
(pipeline_get lists them): a buffer write, no recompile. {FORMATS}"
            ),
            json!({"type": "object", "properties": {
                "pass": {"type": "string"},
                "name": {"type": "string"},
                "value": {"type": "object"},
            }, "required": ["pass", "name", "value"], "additionalProperties": false}),
        ),
        tool(
            "scene_list",
            "The open scene document (meshes, materials with their graphs, instances, node \
hierarchy, environment), the selected instance and the material open in the material canvas.",
            empty.clone(),
        ),
        tool(
            "scene_open",
            "Open a .scene.json, or import a .gltf/.glb as the scene, replacing the open one, and \
show the scene canvas. The path is relative to the project root.",
            json!({"type": "object", "properties": {"path": {"type": "string"}},
                "required": ["path"], "additionalProperties": false}),
        ),
        tool(
            "scene_import_gltf",
            "Import a .gltf/.glb into the open scene (or as the scene when none is open) and show \
the scene canvas. The path is relative to the project root.",
            json!({"type": "object", "properties": {"path": {"type": "string"}},
                "required": ["path"], "additionalProperties": false}),
        ),
        tool(
            "scene_save",
            "Write the open scene as a .scene.json under the project root.",
            json!({"type": "object", "properties": {"path": {"type": "string"}},
                "required": ["path"], "additionalProperties": false}),
        ),
        tool(
            "scene_select",
            "Select an instance by index, or nothing with null, and open its material in the \
material canvas, so graph_* tools edit it and recompile it across the scene.",
            json!({"type": "object", "properties": {"instance": {"type": ["integer", "null"]}},
                "required": ["instance"], "additionalProperties": false}),
        ),
        tool(
            "scene_set_transform",
            "Set an instance's transform relative to its node: 16 floats, column-major.",
            json!({"type": "object", "properties": {
                "instance": {"type": "integer"},
                "transform": {"type": "array", "items": {"type": "number"}, "minItems": 16, "maxItems": 16},
            }, "required": ["instance", "transform"], "additionalProperties": false}),
        ),
        tool(
            "scene_assign_material",
            "Draw an instance with another of the scene's materials, by index.",
            json!({"type": "object", "properties": {
                "instance": {"type": "integer"},
                "material": {"type": "integer"},
            }, "required": ["instance", "material"], "additionalProperties": false}),
        ),
        tool(
            "environment_edit",
            "Set the linear RGB colour and the intensity of one of the scene's lights, by index.",
            json!({"type": "object", "properties": {
                "light": {"type": "integer"},
                "color": {"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3},
                "intensity": {"type": "number"},
            }, "required": ["light", "color", "intensity"], "additionalProperties": false}),
        ),
        tool(
            "view_get",
            "The session state: the canvas showing, the preview's pipeline, the debug view and \
the ones available, and the pass stepping.",
            empty.clone(),
        ),
        tool(
            "view_set",
            "Change what the editor shows; none of it is an undo step. mode: material, pipeline \
or scene. pipeline: forward, deferred, or document (the pipeline document). debug_view: a label \
view_get lists (albedo, normal, depth, ...) or shaded.",
            json!({"type": "object", "properties": {
                "mode": {"type": "string", "enum": ["material", "pipeline", "scene"]},
                "pipeline": {"type": "string"},
                "debug_view": {"type": "string"},
            }, "additionalProperties": false}),
        ),
        tool(
            "view_step",
            "Freeze the frame and run only the first `passes` passes of the running pass list, \
showing the resource labelled `show` or what the last of them wrote; with no `passes`, run live \
again. Take a viewport screenshot to see the result.",
            json!({"type": "object", "properties": {
                "passes": {"type": ["integer", "null"]},
                "show": {"type": ["string", "null"]},
            }, "additionalProperties": false}),
        ),
        tool(
            "view_screenshot",
            "A PNG of the editor window, or with viewport of what the preview or the scene \
viewport renders alone.",
            json!({"type": "object", "properties": {
                "viewport": {"type": "boolean"},
                "max_width": {"type": "integer", "minimum": 16},
            }, "additionalProperties": false}),
        ),
        tool(
            "frame_stats",
            "Counters: nodes, edges, compiled shader variants, the last GPU frame time, and the \
running passes.",
            empty.clone(),
        ),
        tool(
            "problems_list",
            "Every current problem, after recompiling: the material's errors, the pipeline \
document's and the last shader reload's.",
            empty.clone(),
        ),
        tool(
            "history",
            "Undo or redo one step, or with no step just list the steps that can be undone, \
oldest first, each marked with whether an agent made it.",
            json!({"type": "object", "properties": {
                "step": {"type": "string", "enum": ["undo", "redo"]},
            }, "additionalProperties": false}),
        ),
    ]
}

/// Every resource, as `resources/list` returns it.
pub(crate) fn resources(root: &Root) -> Vec<Json> {
    let mut list = vec![
        json!({"uri": "wxsl://nodes", "name": "node reference",
            "description": "Every node a graph can place: id, sockets, types, defaults, doc.",
            "mimeType": "text/markdown"}),
        json!({"uri": "wxsl://guide", "name": "authoring guide",
            "description": "How to build a material with graph_edit and write a function: \
the source's shape, value and type spellings, and how to read the errors.",
            "mimeType": "text/markdown"}),
        json!({"uri": "wxsl://abi", "name": "shader ABI",
            "description": "The WXSL modules a material and a user function are written against.",
            "mimeType": "text/plain"}),
    ];
    if root.path().join(ADR_INDEX).is_file() {
        list.push(json!({"uri": "wxsl://adr", "name": "ADR index",
            "description": "The project's architecture decisions, one line each.",
            "mimeType": "text/markdown"}));
    }
    list
}

/// The authoring guide (plan6 M2). Its snippets are checked here and in
/// the facade's `mcp` test.
pub(crate) const GUIDE: &str = include_str!("../authoring.md");

/// Where the ADR index is, under the root.
const ADR_INDEX: &str = "docs/adr/README.md";

/// The ABI modules the `wxsl://abi` resource shows.
const ABI_MODULES: &[&str] = &[
    wxsl_core::abi::SURFACE_MODULE,
    wxsl_core::abi::VERTEX_MODULE,
    "package::wxsl::bindings",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    document: Document,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Which {
    #[serde(default)]
    document: Document,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edits {
    #[serde(default)]
    document: Document,
    commands: Vec<Json>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineEdits {
    commands: Vec<Json>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Macro {
    name: String,
    #[serde(default)]
    value: Option<MacroValue>,
    #[serde(default)]
    document: Document,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PassParam {
    pass: String,
    name: String,
    value: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePath {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Select {
    instance: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Transform {
    instance: usize,
    transform: [f32; 16],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Assign {
    instance: usize,
    material: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Light {
    light: usize,
    color: [f32; 3],
    intensity: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewSet {
    #[serde(default)]
    mode: Option<CanvasMode>,
    #[serde(default)]
    pipeline: Option<String>,
    #[serde(default)]
    debug_view: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    #[serde(default)]
    passes: Option<usize>,
    #[serde(default)]
    show: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Screenshot {
    #[serde(default)]
    viewport: bool,
    #[serde(default)]
    max_width: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    #[serde(default)]
    step: Option<HistoryStep>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum HistoryStep {
    Undo,
    Redo,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Nothing {}

/// Plan a `tools/call`.
pub(crate) fn plan(name: &str, arguments: Json) -> Result<Action, PlanError> {
    fn parse<T: serde::de::DeserializeOwned>(arguments: Json) -> Result<T, PlanError> {
        let arguments = match arguments {
            Json::Null => Json::Object(Map::new()),
            other => other,
        };
        serde_json::from_value(arguments).map_err(|error| PlanError::Invalid(error.to_string()))
    }
    let run = |edit: Option<Edit>, queries: Vec<Query>| Ok(Action::Run { edit, queries });
    let command = |command: Command| Some(Edit::Command(command));
    let diagnostics = || vec![Query::Diagnostics];
    match name {
        "nodes_search" => {
            let Search { search, document } = parse(arguments)?;
            run(None, vec![Query::Nodes { document, search }])
        }
        "graph_get" => {
            let Which { document } = parse(arguments)?;
            run(None, vec![Query::Graph { document }])
        }
        "graph_edit" => {
            let Edits { document, commands } = parse(arguments)?;
            run(Some(Edit::Batch { document, commands }), diagnostics())
        }
        "pipeline_edit" => {
            let PipelineEdits { commands } = parse(arguments)?;
            run(
                Some(Edit::Batch {
                    document: Document::Pipeline,
                    commands,
                }),
                diagnostics(),
            )
        }
        "graph_compile" => {
            let Nothing {} = parse(arguments)?;
            run(None, vec![Query::Source, Query::Diagnostics])
        }
        "function_save" => {
            let Source { source } = parse(arguments)?;
            Ok(Action::DefineFunction(source))
        }
        "macros_set" => {
            let Macro {
                name,
                value,
                document,
            } = parse(arguments)?;
            run(
                command(Command::SetMacro {
                    document,
                    name,
                    value,
                }),
                diagnostics(),
            )
        }
        "pipeline_get" => {
            let Nothing {} = parse(arguments)?;
            run(
                None,
                vec![
                    Query::Graph {
                        document: Document::Pipeline,
                    },
                    Query::Passes,
                    Query::View,
                ],
            )
        }
        "pass_param" => {
            let PassParam { pass, name, value } = parse(arguments)?;
            run(
                command(Command::SetPassParam { pass, name, value }),
                vec![Query::Passes],
            )
        }
        "scene_list" => {
            let Nothing {} = parse(arguments)?;
            run(None, vec![Query::Scene])
        }
        "scene_open" | "scene_import_gltf" => {
            let FilePath { path } = parse(arguments)?;
            let open = if name == "scene_open" {
                Command::OpenScene { path }
            } else {
                Command::ImportGltf { path }
            };
            run(
                command(Command::Batch {
                    commands: vec![
                        open,
                        Command::SetCanvasMode {
                            mode: CanvasMode::Scene,
                        },
                    ],
                }),
                diagnostics(),
            )
        }
        "scene_save" => {
            let FilePath { path } = parse(arguments)?;
            run(command(Command::SaveScene { path }), Vec::new())
        }
        "scene_select" => {
            let Select { instance } = parse(arguments)?;
            run(command(Command::SelectInstance { instance }), diagnostics())
        }
        "scene_set_transform" => {
            let Transform {
                instance,
                transform,
            } = parse(arguments)?;
            run(
                command(Command::SetTransform {
                    instance,
                    transform,
                }),
                Vec::new(),
            )
        }
        "scene_assign_material" => {
            let Assign { instance, material } = parse(arguments)?;
            run(
                command(Command::SetInstanceMaterial { instance, material }),
                diagnostics(),
            )
        }
        "environment_edit" => {
            let Light {
                light,
                color,
                intensity,
            } = parse(arguments)?;
            run(
                command(Command::SetLight {
                    light,
                    color,
                    intensity,
                }),
                Vec::new(),
            )
        }
        "view_get" => {
            let Nothing {} = parse(arguments)?;
            run(None, vec![Query::View])
        }
        "view_set" => {
            let ViewSet {
                mode,
                pipeline,
                debug_view,
            } = parse(arguments)?;
            let mut commands = Vec::new();
            commands.extend(mode.map(|mode| Command::SetCanvasMode { mode }));
            commands.extend(pipeline.map(|pipeline| Command::SetPipeline { pipeline }));
            commands.extend(debug_view.map(|view| Command::SetDebugView { view }));
            let edit = (!commands.is_empty()).then_some(Edit::Command(Command::Batch { commands }));
            run(edit, vec![Query::View])
        }
        "view_step" => {
            let Step { passes, show } = parse(arguments)?;
            run(
                command(Command::StepFrame { passes, show }),
                vec![Query::View],
            )
        }
        "view_screenshot" => {
            let Screenshot {
                viewport,
                max_width,
            } = parse(arguments)?;
            Ok(Action::Screenshot {
                viewport,
                max_width: max_width.unwrap_or(SCREENSHOT_WIDTH).max(16),
            })
        }
        "frame_stats" => {
            let Nothing {} = parse(arguments)?;
            run(None, vec![Query::Stats, Query::Passes])
        }
        "problems_list" => {
            let Nothing {} = parse(arguments)?;
            run(None, diagnostics())
        }
        "history" => {
            let History { step } = parse(arguments)?;
            let edit = step.map(|step| {
                Edit::Command(match step {
                    HistoryStep::Undo => Command::Undo,
                    HistoryStep::Redo => Command::Redo,
                })
            });
            run(edit, vec![Query::History])
        }
        _ => Err(PlanError::Unknown(name.to_string())),
    }
}

/// A tool result holding one text block.
fn text(text: String, error: bool) -> Json {
    json!({"content": [{"type": "text", "text": text}], "isError": error})
}

/// A tool result reporting a failure.
fn failure(message: impl Into<String>) -> Reply {
    Ok(text(message.into(), true))
}

/// Run an action against the editor, on the render thread.
pub(crate) fn run(action: Action, editor: &mut Editor, frame: &Frame<'_>, root: &Root) -> Reply {
    match action {
        Action::Run { edit, queries } => {
            let mut result = Map::new();
            if let Some(edit) = edit {
                let (mut command, refs) = match edit {
                    Edit::Command(command) => (command, BTreeMap::new()),
                    Edit::Batch { document, commands } => match resolve(editor, document, commands)
                    {
                        Ok(resolved) => resolved,
                        Err(message) => return failure(message),
                    },
                };
                if let Err(message) = root.confine(&mut command) {
                    return failure(message);
                }
                match editor.apply_as_agent(command) {
                    Ok(outcome) => {
                        result.insert("outcome".into(), json!(outcome));
                        if !refs.is_empty() {
                            result.insert("refs".into(), json!(refs));
                        }
                    }
                    Err(error) => return failure(error.to_string()),
                }
            }
            editor.sync(frame.device, frame.queue);
            for query in &queries {
                let (name, answer) = answer(editor, query);
                result.insert(name, answer);
            }
            Ok(text(pretty(&Json::Object(result)), false))
        }
        Action::DefineFunction(source) => match editor.define_function(frame.device, &source) {
            Ok(node) => {
                editor.sync(frame.device, frame.queue);
                Ok(text(pretty(&json!({"node": node})), false))
            }
            Err(marks) => {
                let lines: Vec<String> = marks
                    .iter()
                    .map(|mark| format!("line {}: {}", mark.line + 1, mark.message))
                    .collect();
                failure(lines.join("\n"))
            }
        },
        Action::Screenshot {
            viewport,
            max_width,
        } => match screenshot(editor, frame, viewport) {
            Ok((rgba, width, height)) => {
                let (rgba, width, height) = image::fit(rgba, width, height, max_width);
                match image::png(&rgba, width, height) {
                    Ok(png) => Ok(json!({"content": [
                        {"type": "image", "data": image::base64(&png), "mimeType": "image/png"},
                        {"type": "text", "text": format!("{width}x{height}")},
                    ], "isError": false})),
                    Err(message) => failure(message),
                }
            }
            Err(message) => failure(message),
        },
        Action::Resource(uri) => {
            let (mime, body) = match uri.as_str() {
                "wxsl://nodes" => (
                    "text/markdown",
                    crate::node_reference(
                        editor.registry(),
                        &wxsl_render::document_registry(editor.preview().effects()),
                    ),
                ),
                "wxsl://guide" => ("text/markdown", GUIDE.to_string()),
                "wxsl://abi" => {
                    let library = editor.preview().library();
                    let body = ABI_MODULES
                        .iter()
                        .filter_map(|path| {
                            Some(format!("// ---- {path}\n{}\n", library.get(path)?))
                        })
                        .collect();
                    ("text/plain", body)
                }
                "wxsl://adr" => match std::fs::read_to_string(root.path().join(ADR_INDEX)) {
                    Ok(body) => ("text/markdown", body),
                    Err(_) => return Err((RESOURCE_NOT_FOUND, format!("no resource {uri}"))),
                },
                _ => return Err((RESOURCE_NOT_FOUND, format!("no resource {uri}"))),
            };
            Ok(json!({"contents": [{"uri": uri, "mimeType": mime, "text": body}]}))
        }
    }
}

/// Resolve a batch's `$name` references to the ids its `add_node`s will
/// get, and give every graph command that names no document `document`.
fn resolve(
    editor: &Editor,
    document: Document,
    commands: Vec<Json>,
) -> Result<(Command, BTreeMap<String, u32>), String> {
    let mut next = [
        Some(editor.graph().next_id().0),
        editor.pipeline_graph().map(|graph| graph.next_id().0),
    ];
    let mut refs = BTreeMap::new();
    let commands = commands
        .into_iter()
        .enumerate()
        .map(|(index, command)| {
            prepare(command, document, &mut next, &mut refs)
                .map_err(|message| format!("command {index}: {message}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let commands = commands
        .into_iter()
        .enumerate()
        .map(|(index, command)| {
            serde_json::from_value(command).map_err(|error| format!("command {index}: {error}"))
        })
        .collect::<Result<Vec<Command>, _>>()?;
    Ok((Command::Batch { commands }, refs))
}

/// One command of a batch: its document defaulted, its `ref` taken, its
/// references substituted. Nested batches are walked in order.
fn prepare(
    mut command: Json,
    document: Document,
    next: &mut [Option<u32>; 2],
    refs: &mut BTreeMap<String, u32>,
) -> Result<Json, String> {
    let object = command
        .as_object_mut()
        .ok_or("a command is an object tagged by \"command\"")?;
    let tag = object
        .get("command")
        .and_then(Json::as_str)
        .ok_or("a command needs a \"command\" tag")?
        .to_string();
    if tag == "batch" {
        if let Some(Json::Array(inner)) = object.remove("commands") {
            let inner = inner
                .into_iter()
                .map(|command| prepare(command, document, next, refs))
                .collect::<Result<Vec<_>, _>>()?;
            object.insert("commands".into(), Json::Array(inner));
        }
        return Ok(command);
    }
    let graph_command = GRAPH_COMMANDS.contains(&tag.as_str());
    if graph_command && !object.contains_key("document") {
        object.insert("document".into(), json!(document));
    }
    let name = object.remove("ref");
    substitute(&mut command, refs)?;
    if tag == "add_node" {
        let target: Document = serde_json::from_value(command["document"].clone())
            .map_err(|error| error.to_string())?;
        let slot = &mut next[target as usize];
        if let Some(name) = name {
            let name = name.as_str().ok_or("a ref is a string")?.to_string();
            let id = slot.ok_or(
                "the pipeline document is not open yet, so its new nodes cannot be named; \
                 edit it once without refs, or call pipeline_get after a first edit",
            )?;
            if refs.insert(name.clone(), id).is_some() {
                return Err(format!("ref `{name}` is defined twice"));
            }
        }
        if let Some(id) = slot.as_mut() {
            *id += 1;
        }
    } else if name.is_some() {
        return Err("only add_node takes a ref".into());
    }
    Ok(command)
}

/// Replace every `"$name"` string naming a known ref with its id.
fn substitute(value: &mut Json, refs: &BTreeMap<String, u32>) -> Result<(), String> {
    match value {
        Json::String(text) => {
            if let Some(name) = text.strip_prefix('$') {
                let id = refs
                    .get(name)
                    .ok_or_else(|| format!("no node was added as `{name}` before this command"))?;
                *value = json!(NodeId(*id));
            }
        }
        Json::Array(items) => {
            for item in items {
                substitute(item, refs)?;
            }
        }
        Json::Object(object) => {
            for item in object.values_mut() {
                substitute(item, refs)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// A query's answer, named by its tag. An answer whose one field has the
/// tag's name, such as `{"graph": …}`, is that field.
fn answer(editor: &Editor, query: &Query) -> (String, Json) {
    let mut answer = json!(editor.query(query));
    let name = answer
        .as_object_mut()
        .and_then(|object| object.remove("answer"))
        .and_then(|name| name.as_str().map(str::to_string))
        .unwrap_or_default();
    if let Some(object) = answer.as_object_mut() {
        if object.len() == 1 {
            if let Some(inner) = object.remove(&name) {
                return (name, inner);
            }
        }
    }
    (name, answer)
}

/// JSON for a model to read.
fn pretty(value: &Json) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// Draw the editor twice into an offscreen target, so the material and
/// the glyph atlas have settled, and read back the window or the
/// viewport.
fn screenshot(
    editor: &mut Editor,
    frame: &Frame<'_>,
    viewport: bool,
) -> Result<(Vec<u8>, u32, u32), String> {
    let (width, height) = (frame.width.max(1), frame.height.max(1));
    let target = OffscreenTarget::new(frame.device, width, height);
    for _ in 0..2 {
        let mut encoder = frame
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mcp screenshot"),
            });
        editor
            .frame(
                frame.device,
                frame.queue,
                &mut encoder,
                &UiTarget {
                    view: target.view(),
                    format: target.format(),
                    width,
                    height,
                    clear: None,
                },
                frame.time,
            )
            .map_err(|error| error.to_string())?;
        frame.queue.submit([encoder.finish()]);
    }
    Ok(if viewport {
        let (width, height) = editor.preview().size();
        (
            editor.preview().read_rgba8(frame.device, frame.queue),
            width,
            height,
        )
    } else {
        (target.read_rgba8(frame.device, frame.queue), width, height)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_tool_plans_and_no_other_does() {
        let minimal = |name: &str| match name {
            "graph_edit" | "pipeline_edit" => json!({"commands": []}),
            "function_save" => json!({"source": "fn f() {}"}),
            "macros_set" => json!({"name": "QUALITY"}),
            "pass_param" => json!({"pass": "bloom", "name": "strength", "value": {"f32": 1.0}}),
            "scene_open" | "scene_import_gltf" | "scene_save" => json!({"path": "a"}),
            "scene_select" => json!({"instance": null}),
            "scene_set_transform" => json!({"instance": 0, "transform": ([0.0f32; 16])}),
            "scene_assign_material" => json!({"instance": 0, "material": 1}),
            "environment_edit" => json!({"light": 0, "color": [1, 1, 1], "intensity": 2}),
            _ => Json::Null,
        };
        let names: Vec<String> = definitions()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect();
        for name in &names {
            assert!(
                name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "{name}: clients refuse anything but [A-Za-z0-9_-]"
            );
            plan(name, minimal(name)).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        }
        assert_eq!(
            plan("graph.edit", Json::Null),
            Err(PlanError::Unknown("graph.edit".into()))
        );
    }

    /// The guide's fenced snippets: (info string, body, the `text` fence
    /// after it).
    fn guide_snippets() -> Vec<(&'static str, String, Option<String>)> {
        let mut snippets: Vec<(&str, String, Option<String>)> = Vec::new();
        let mut lines = GUIDE.lines();
        while let Some(line) = lines.next() {
            let Some(info) = line.strip_prefix("```").filter(|info| !info.is_empty()) else {
                continue;
            };
            let body: Vec<&str> = lines.by_ref().take_while(|line| *line != "```").collect();
            let body = body.join("\n") + "\n";
            match info {
                "text" => snippets.last_mut().expect("a text after a snippet").2 = Some(body),
                _ => snippets.push((info, body, None)),
            }
        }
        snippets
    }

    #[test]
    fn the_guide_calls_plan_and_its_sources_derive() {
        let snippets = guide_snippets();
        assert!(snippets.len() >= 8, "the guide lost its snippets");
        for (info, body, expected) in snippets {
            match info {
                "json tool" => {
                    let call: Json = serde_json::from_str(&body).expect("a guide call parses");
                    let name = call["name"].as_str().unwrap();
                    plan(name, call["arguments"].clone())
                        .unwrap_or_else(|error| panic!("{name}: {error:?}"));
                }
                "wxsl" | "wxsl refused" => {
                    let name = body
                        .split("fn ")
                        .nth(1)
                        .and_then(|rest| rest.split(['(', '<']).next())
                        .unwrap_or("function");
                    let derived =
                        wxsl_lang::node_from_source(&body, &format!("package::user::{name}"));
                    match (info, derived) {
                        ("wxsl", derived) => {
                            derived.unwrap_or_else(|error| panic!("{body}\n{error:?}"));
                        }
                        // Refused before any device: the guide quotes it.
                        (_, Err(errors)) => {
                            let expected = expected.expect("a refused snippet says why");
                            assert!(
                                errors
                                    .iter()
                                    .any(|error| error.message.contains(expected.trim())),
                                "{expected:?} not in {errors:?}"
                            );
                        }
                        // Refused by the device's validator, which the
                        // facade's `mcp` test runs.
                        (_, Ok(_)) => {}
                    }
                }
                other => panic!("unknown snippet `{other}`"),
            }
        }
    }

    #[test]
    fn unknown_arguments_are_refused_by_name() {
        let Err(PlanError::Invalid(message)) = plan("graph_get", json!({"documnet": "material"}))
        else {
            panic!("a misspelt argument planned")
        };
        assert!(message.contains("documnet"), "{message}");
    }

    #[test]
    fn view_set_batches_its_changes() {
        assert_eq!(
            plan(
                "view_set",
                json!({"mode": "scene", "pipeline": "deferred", "debug_view": "normal"})
            ),
            Ok(Action::Run {
                edit: Some(Edit::Command(Command::Batch {
                    commands: vec![
                        Command::SetCanvasMode {
                            mode: CanvasMode::Scene
                        },
                        Command::SetPipeline {
                            pipeline: "deferred".into()
                        },
                        Command::SetDebugView {
                            view: "normal".into()
                        },
                    ]
                })),
                queries: vec![Query::View],
            })
        );
    }

    #[test]
    fn refs_name_the_ids_a_batch_adds_and_documents_default() {
        let mut next = [Some(7), None];
        let mut refs = BTreeMap::new();
        let commands = [
            json!({"command": "add_node", "node": {"def": "a"}, "ref": "first"}),
            json!({"command": "add_node", "node": {"def": "b"}}),
            json!({"command": "add_node", "node": {"def": "c"}, "ref": "third"}),
            json!({"command": "connect",
                "from": {"node": "$first", "socket": "out"},
                "to": {"node": "$third", "socket": "a"}}),
            json!({"command": "set_light", "light": 0, "color": [1, 1, 1], "intensity": 1}),
        ];
        let prepared: Vec<Json> = commands
            .into_iter()
            .map(|command| prepare(command, Document::Material, &mut next, &mut refs).unwrap())
            .collect();
        assert_eq!(
            refs,
            BTreeMap::from([("first".into(), 7), ("third".into(), 9)])
        );
        assert_eq!(next[0], Some(10));
        assert_eq!(prepared[3]["from"]["node"], json!(7));
        assert_eq!(prepared[3]["to"]["node"], json!(9));
        assert_eq!(prepared[3]["document"], json!("material"));
        assert!(prepared[4].get("document").is_none(), "not a graph command");
        for command in prepared {
            serde_json::from_value::<Command>(command).unwrap();
        }

        let unknown = json!({"command": "remove_nodes", "nodes": ["$nope"]});
        assert!(prepare(unknown, Document::Material, &mut next, &mut refs).is_err());
        let closed = json!({"command": "add_node", "document": "pipeline",
            "node": {"def": "a"}, "ref": "p"});
        assert!(
            prepare(closed, Document::Material, &mut next, &mut refs).is_err(),
            "a pipeline document that is not open has no known ids"
        );
    }
}
