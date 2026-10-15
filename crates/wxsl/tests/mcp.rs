//! A headless editor driven through the MCP server (ADR 0083): an agent's
//! session over HTTP, and the same calls over the line transport stdio
//! uses.
//!
//! The protocol, the tool plans, path confinement and HTTP parsing are
//! tested without a device in `wxsl-mcp` itself. What needs one is the
//! editor the tools run against, so this **skips** (prints a note and
//! passes) when there is no adapter or font. A skip is not a pass.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use glam::Vec2;
use serde_json::{json, Value as Json};
use wxsl::core::graph::Graph;
use wxsl::editor::{Editor, EditorConfig};
use wxsl::mcp::{Frame, Root, Server};
use wxsl::render::gpu::GpuContext;
use wxsl::render::ui::input::UiEvent;

const DEMO_GRAPH: &str = include_str!("../assets/pbr_cube.wxsl.json");
const SIZE: (u32, u32) = (1280, 800);
/// The repository, which is the agent's root here.
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn gpu() -> Option<GpuContext> {
    match pollster::block_on(GpuContext::headless()) {
        Ok(gpu) => Some(gpu),
        Err(error) => {
            println!("skipping: no wgpu adapter ({error})");
            None
        }
    }
}

fn editor(gpu: &GpuContext) -> Option<Editor> {
    let read = |name: &str| std::fs::read(format!("{ROOT}/resources/fonts/{name}")).ok();
    let (Some(ui), Some(mono)) = (read("Inter-Regular.ttf"), read("JetBrainsMono-Regular.ttf"))
    else {
        println!("skipping: the repository's fonts are missing");
        return None;
    };
    let graph: Graph = serde_json::from_str(DEMO_GRAPH).expect("the demo graph parses");
    let config = EditorConfig::new(
        wxsl::stdlib_library(),
        wxsl::stdlib::registry(),
        graph,
        ui,
        mono,
    );
    let mut editor = Editor::new(&gpu.device, &gpu.queue, config).expect("the editor starts");
    editor.handle_event(UiEvent::Resized {
        size: Vec2::new(SIZE.0 as f32, SIZE.1 as f32),
        scale: 1.0,
    });
    Some(editor)
}

/// Serve on this thread until `client`, running on another, is done.
fn drive<T: Send + 'static>(
    gpu: &GpuContext,
    editor: &mut Editor,
    server: &mut Server,
    client: impl FnOnce() -> T + Send + 'static,
) -> T {
    let client = std::thread::spawn(client);
    let mut time = 0.0;
    while !client.is_finished() {
        if server.wait(Duration::from_millis(20)) {
            time += 0.016;
            server.serve(
                editor,
                &Frame {
                    device: &gpu.device,
                    queue: &gpu.queue,
                    width: SIZE.0,
                    height: SIZE.1,
                    time,
                },
            );
        }
    }
    client.join().expect("the client finished")
}

/// One HTTP request; returns the status and the body.
fn http(address: SocketAddr, method: &str, token: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(address).unwrap();
    write!(
        stream,
        "{method} /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         Connection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, body.to_string())
}

/// A JSON-RPC request over HTTP, returning its `result`.
fn rpc(address: SocketAddr, token: &str, id: u32, method: &str, params: Json) -> Json {
    let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let (status, body) = http(address, "POST", token, &body.to_string());
    assert_eq!(status, 200, "{method}: {body}");
    let answer: Json = serde_json::from_str(&body).unwrap();
    assert!(answer.get("error").is_none(), "{method}: {answer}");
    answer["result"].clone()
}

/// A tool result's text, parsed back to JSON, or its error text.
fn tool_json(result: &Json) -> Result<Json, String> {
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    if result["isError"] == true {
        return Err(text.to_string());
    }
    Ok(serde_json::from_str(text).unwrap_or(Json::String(text.to_string())))
}

#[test]
fn an_agent_edits_compiles_looks_and_undoes_over_http() {
    let Some(gpu) = gpu() else { return };
    let Some(mut editor) = editor(&gpu) else {
        return;
    };
    let mut server = Server::http(Root::new(ROOT).unwrap(), 0, Some("secret".into())).unwrap();
    let address = server.address().unwrap();
    let nodes_before = editor.graph().node_count();

    drive(&gpu, &mut editor, &mut server, move || {
        let call = |id: u32, name: &str, arguments: Json| {
            rpc(
                address,
                "secret",
                id,
                "tools/call",
                json!({"name": name, "arguments": arguments}),
            )
        };

        assert_eq!(http(address, "POST", "wrong", "{}").0, 401);
        let init = rpc(
            address,
            "secret",
            1,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "test-agent", "version": "0"}}),
        );
        assert_eq!(init["serverInfo"]["name"], "wxsl");
        let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert_eq!(
            http(address, "POST", "secret", &initialized.to_string()).0,
            202
        );

        let tools = rpc(address, "secret", 2, "tools/list", json!({}));
        assert!(tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "graph_edit"));

        // Add a node and name it in the same batch, through a ref.
        let edit = tool_json(&call(
            3,
            "graph_edit",
            json!({"commands": [
                {"command": "add_node", "node": {"def": "math.multiply"}, "ref": "m"},
                {"command": "set_label", "node": "$m", "label": "agent multiply"},
            ]}),
        ))
        .expect("the edit applies");
        let id = edit["refs"]["m"].as_u64().unwrap();
        assert_eq!(edit["outcome"]["added"], json!([id]));
        let graph = tool_json(&call(4, "graph_get", json!({}))).unwrap();
        assert_eq!(
            graph["graph"]["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|node| node["id"] == id)
                .map(|node| node["label"].clone()),
            Some(json!("agent multiply"))
        );

        let compiled = tool_json(&call(5, "graph_compile", json!({}))).unwrap();
        assert!(compiled["source"]["wxsl"].as_str().unwrap().contains("fn "));
        assert_eq!(compiled["diagnostics"]["material"], json!([]));

        // A failing batch changes nothing.
        let refused = tool_json(&call(
            6,
            "graph_edit",
            json!({"commands": [
                {"command": "set_label", "node": id, "label": "renamed"},
                {"command": "remove_nodes", "nodes": [99999]},
            ]}),
        ));
        assert!(refused.unwrap_err().contains("99999"));

        let history = tool_json(&call(7, "history", json!({}))).unwrap();
        let steps = history["history"]["steps"].as_array().unwrap().clone();
        assert_eq!(steps.len(), 1, "one batch, one step: {steps:?}");
        assert_eq!(steps[0], json!({"command": "batch", "agent": true}));
        tool_json(&call(8, "history", json!({"step": "undo"}))).unwrap();

        // A function becomes a node, and the library offers it.
        let source = "// Lift\n//\n// Add a constant to every channel.\n\n\
                      fn lift(\n    color: vec3f, // The colour. @default 0.5, 0.5, 0.5\n    \
                      amount: f32,  // What to add. @default 0.1\n) -> vec3f {\n    \
                      return color + vec3f(amount);\n}\n";
        let saved = tool_json(&call(9, "function_save", json!({"source": source}))).unwrap();
        let node = saved["node"].as_str().unwrap().to_string();
        assert!(node.starts_with("user."), "{node}");
        let found = tool_json(&call(10, "nodes_search", json!({"search": "lift"}))).unwrap();
        assert!(found["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == node.as_str()));
        let broken = call(11, "function_save", json!({"source": "fn broken( {"}));
        assert_eq!(broken["isError"], true);

        // The view: a debug view on the deferred path, then a picture.
        let view = tool_json(&call(
            12,
            "view_set",
            json!({"pipeline": "deferred", "debug_view": "normal"}),
        ))
        .unwrap();
        assert_eq!(view["view"]["pipeline"], "deferred");
        assert_eq!(view["view"]["debug_view"], "normal");
        tool_json(&call(13, "view_set", json!({"debug_view": "shaded"}))).unwrap();
        let picture = call(14, "view_screenshot", json!({"max_width": 400}));
        assert_eq!(picture["content"][0]["type"], "image");
        assert_eq!(picture["content"][0]["mimeType"], "image/png");
        assert!(picture["content"][0]["data"]
            .as_str()
            .unwrap()
            .starts_with("iVBORw0KGgo"));
        assert_eq!(picture["content"][1]["text"], "320x200");

        let stepped = tool_json(&call(15, "view_step", json!({"passes": 1}))).unwrap();
        assert_eq!(stepped["view"]["stepping"]["count"], 1);
        tool_json(&call(16, "view_step", json!({}))).unwrap();

        // Paths stay under the root.
        let escape = call(
            17,
            "scene_open",
            json!({"path": "../../outside.scene.json"}),
        );
        assert_eq!(escape["isError"], true);
        let raw_escape = call(
            18,
            "graph_edit",
            json!({"commands": [{"command": "save_scene", "path": "/tmp/x.scene.json"}]}),
        );
        assert_eq!(raw_escape["isError"], true);

        let resources = rpc(address, "secret", 19, "resources/list", json!({}));
        assert!(resources["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|resource| resource["uri"] == "wxsl://adr"));
        let reference = rpc(
            address,
            "secret",
            20,
            "resources/read",
            json!({"uri": "wxsl://nodes"}),
        );
        assert!(reference["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("`math.multiply`"));

        assert_eq!(http(address, "DELETE", "secret", "").0, 200);
    });

    assert_eq!(
        editor.graph().node_count(),
        nodes_before,
        "the agent's edit was undone"
    );
    // The next serve sees the session gone and clears the indicator.
    server.serve(
        &mut editor,
        &Frame {
            device: &gpu.device,
            queue: &gpu.queue,
            width: SIZE.0,
            height: SIZE.1,
            time: 1.0,
        },
    );
    assert_eq!(editor.agent(), None);
}

/// A writer the test reads back.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_line_transport_serves_until_its_input_ends() {
    let Some(gpu) = gpu() else { return };
    let Some(mut editor) = editor(&gpu) else {
        return;
    };
    let script = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "clientInfo": {"name": "ci"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "problems_list"}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "view_screenshot", "arguments": {"viewport": true, "max_width": 64}}}),
    ]
    .iter()
    .map(|message| format!("{message}\n"))
    .collect::<String>();
    let output = Captured::default();
    let mut server = Server::lines(
        Root::new(ROOT).unwrap(),
        BufReader::new(std::io::Cursor::new(script.into_bytes())),
        output.clone(),
    )
    .unwrap();
    let mut time = 0.0;
    while !server.is_closed() {
        if server.wait(Duration::from_millis(20)) {
            time += 0.016;
            server.serve(
                &mut editor,
                &Frame {
                    device: &gpu.device,
                    queue: &gpu.queue,
                    width: SIZE.0,
                    height: SIZE.1,
                    time,
                },
            );
        }
    }
    let text = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    let answers: Vec<Json> = BufReader::new(text.as_bytes())
        .lines()
        .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
        .collect();
    assert_eq!(
        answers.len(),
        3,
        "the notification gets no answer: {answers:?}"
    );
    assert_eq!(answers[0]["result"]["serverInfo"]["name"], "wxsl");
    let problems = tool_json(&answers[1]["result"]).unwrap();
    assert_eq!(problems["diagnostics"]["material"], json!([]));
    assert_eq!(answers[2]["result"]["content"][0]["type"], "image");
}
