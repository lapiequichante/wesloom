//! JSON-RPC and the MCP methods, the same for both transports.
//!
//! The protocol methods are answered here, on the transport's thread. A
//! tool call or a resource read is planned here too, then crosses to the
//! render thread as a [`Job`] and is waited for.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value as Json};

use crate::sandbox::Root;
use crate::tools::{self, Action, PlanError, Reply};

/// The protocol revisions this server speaks, newest first. A client
/// asking for another gets the newest.
pub(crate) const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// What the server tells a client about using it.
const INSTRUCTIONS: &str = "\
This server drives a running wxsl node editor. A material is a graph of nodes compiled to \
WXSL and WGSL; the pipeline is a second graph of passes; a scene holds instances drawn with \
material graphs. Read the wxsl://guide resource first: it shows a whole edit, the spelling of \
values and types, how to write a function, and how to read the errors. wxsl://nodes lists every \
node. Start with graph_get and nodes_search, edit with graph_edit (atomic, undoable, node refs \
with \"ref\"/\"$name\"), check with graph_compile or problems_list, and look with \
view_screenshot. To debug a picture, view_set shows one channel or a view of the frame \
(debug_view), pixel_probe reads every texture and buffer at a point, and frame_stats says \
what each pass cost. Every edit you make is marked as yours in the history and can be undone with \
the history tool. Paths are relative to the project root and cannot leave it.";

/// A call waiting for the render thread.
pub(crate) struct Job {
    /// What to do.
    pub(crate) action: Action,
    /// Where the answer goes.
    pub(crate) reply: mpsc::Sender<Reply>,
}

/// What the transports and the render thread share.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    /// The initialized client's name.
    pub(crate) agent: Mutex<Option<String>>,
    /// The transport has ended: stdin closed.
    pub(crate) closed: std::sync::atomic::AtomicBool,
}

/// Answers messages.
#[derive(Clone)]
pub(crate) struct Dispatcher {
    jobs: mpsc::Sender<Job>,
    shared: Arc<Shared>,
    root: Root,
    timeout: Duration,
}

/// The JSON-RPC error codes this server uses.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

impl Dispatcher {
    pub(crate) fn new(
        jobs: mpsc::Sender<Job>,
        shared: Arc<Shared>,
        root: Root,
        timeout: Duration,
    ) -> Self {
        Dispatcher {
            jobs,
            shared,
            root,
            timeout,
        }
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }

    /// Answer one message, given as text. `None` for a notification or a
    /// response, which get no answer.
    pub(crate) fn handle_text(&self, text: &str) -> Option<Json> {
        match serde_json::from_str::<Json>(text) {
            Ok(message) => self.handle(message),
            Err(error) => Some(error_response(Json::Null, PARSE_ERROR, error.to_string())),
        }
    }

    /// Answer one message.
    pub(crate) fn handle(&self, message: Json) -> Option<Json> {
        let Some(object) = message.as_object() else {
            return Some(error_response(
                Json::Null,
                INVALID_REQUEST,
                "a message is one JSON object".into(),
            ));
        };
        let id = object.get("id").cloned();
        let Some(method) = object.get("method").and_then(Json::as_str) else {
            // A response to a request this server never sends.
            return None;
        };
        let params = object.get("params").cloned().unwrap_or(Json::Null);
        let result = self.call(method, params);
        let id = id?;
        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => error_response(id, code, message),
        })
    }

    fn call(&self, method: &str, params: Json) -> Reply {
        match method {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or_default();
                let version = PROTOCOL_VERSIONS
                    .iter()
                    .find(|version| **version == asked)
                    .unwrap_or(&PROTOCOL_VERSIONS[0]);
                let client = params["clientInfo"]["name"]
                    .as_str()
                    .unwrap_or("agent")
                    .to_string();
                *self.shared.agent.lock().expect("never poisoned") = Some(client);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}, "resources": {}},
                    "serverInfo": {"name": "wxsl", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools::definitions()})),
            "resources/list" => Ok(json!({"resources": tools::resources(&self.root)})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            "tools/call" => {
                let Some(name) = params["name"].as_str() else {
                    return Err((INVALID_PARAMS, "tools/call needs a name".into()));
                };
                match tools::plan(name, params["arguments"].clone()) {
                    Ok(action) => self.dispatch(action),
                    Err(PlanError::Unknown(name)) => {
                        Err((INVALID_PARAMS, format!("no tool `{name}`")))
                    }
                    Err(PlanError::Invalid(message)) => Ok(json!({
                        "content": [{"type": "text", "text": format!("bad arguments: {message}")}],
                        "isError": true,
                    })),
                }
            }
            "resources/read" => match params["uri"].as_str() {
                Some(uri) => self.dispatch(Action::Resource(uri.to_string())),
                None => Err((INVALID_PARAMS, "resources/read needs a uri".into())),
            },
            method if method.starts_with("notifications/") => Ok(Json::Null),
            _ => Err((METHOD_NOT_FOUND, format!("no method `{method}`"))),
        }
    }

    /// Hand an action to the render thread and wait for its answer.
    fn dispatch(&self, action: Action) -> Reply {
        let (reply, answer) = mpsc::channel();
        self.jobs
            .send(Job { action, reply })
            .map_err(|_| (INTERNAL_ERROR, "the editor has shut down".to_string()))?;
        answer.recv_timeout(self.timeout).map_err(|_| {
            (
                INTERNAL_ERROR,
                "the editor did not answer in time; is its window drawing frames?".to_string(),
            )
        })?
    }

    /// The client said goodbye.
    pub(crate) fn disconnect(&self) {
        *self.shared.agent.lock().expect("never poisoned") = None;
    }
}

fn error_response(id: Json, code: i64, message: String) -> Json {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatcher() -> (Dispatcher, mpsc::Receiver<Job>) {
        let (jobs, receiver) = mpsc::channel();
        let root = Root::new(std::env::temp_dir()).unwrap();
        let dispatcher = Dispatcher::new(
            jobs,
            Arc::new(Shared::default()),
            root,
            Duration::from_secs(5),
        );
        (dispatcher, receiver)
    }

    #[test]
    fn initialize_names_the_agent_and_negotiates_the_version() {
        let (dispatcher, _) = dispatcher();
        let answer = dispatcher
            .handle_text(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                    "protocolVersion":"2025-03-26","capabilities":{},
                    "clientInfo":{"name":"claude-code","version":"1"}}}"#,
            )
            .unwrap();
        assert_eq!(answer["id"], 1);
        assert_eq!(answer["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(answer["result"]["serverInfo"]["name"], "wxsl");
        assert_eq!(
            dispatcher.shared().agent.lock().unwrap().as_deref(),
            Some("claude-code")
        );
        let newer = dispatcher
            .handle(json!({"jsonrpc":"2.0","id":2,"method":"initialize",
                "params":{"protocolVersion":"2099-01-01"}}))
            .unwrap();
        assert_eq!(newer["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
        dispatcher.disconnect();
        assert_eq!(*dispatcher.shared().agent.lock().unwrap(), None);
    }

    #[test]
    fn notifications_and_responses_get_no_answer() {
        let (dispatcher, _) = dispatcher();
        assert_eq!(
            dispatcher.handle(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
            None
        );
        assert_eq!(
            dispatcher.handle(json!({"jsonrpc":"2.0","id":4,"result":{}})),
            None
        );
    }

    #[test]
    fn errors_carry_their_json_rpc_codes() {
        let (dispatcher, _) = dispatcher();
        let code = |answer: Option<Json>| answer.unwrap()["error"]["code"].as_i64();
        assert_eq!(code(dispatcher.handle_text("{nope")), Some(PARSE_ERROR));
        assert_eq!(code(dispatcher.handle(json!([1]))), Some(INVALID_REQUEST));
        assert_eq!(
            code(dispatcher.handle(json!({"jsonrpc":"2.0","id":1,"method":"nope"}))),
            Some(METHOD_NOT_FOUND)
        );
        assert_eq!(
            code(
                dispatcher.handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"nope"}}))
            ),
            Some(INVALID_PARAMS)
        );
        let bad = dispatcher
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"graph_get","arguments":{"x":1}}}))
            .unwrap();
        assert_eq!(
            bad["result"]["isError"], true,
            "bad arguments are a tool error"
        );
    }

    #[test]
    fn tools_list_without_an_editor_and_calls_wait_for_the_render_thread() {
        let (dispatcher, jobs) = dispatcher();
        let list = dispatcher
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .unwrap();
        assert!(list["result"]["tools"].as_array().unwrap().len() > 20);

        let render = std::thread::spawn(move || {
            let job = jobs.recv().unwrap();
            assert!(matches!(job.action, Action::Run { .. }));
            job.reply
                .send(Ok(json!({"content": [], "isError": false})))
                .unwrap();
        });
        let answer = dispatcher
            .handle(json!({"jsonrpc":"2.0","id":"a","method":"tools/call",
                "params":{"name":"graph_get"}}))
            .unwrap();
        render.join().unwrap();
        assert_eq!(answer["id"], "a");
        assert_eq!(answer["result"]["isError"], false);
    }
}
