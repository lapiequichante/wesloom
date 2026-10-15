# 83. An MCP server over the editor's commands, hand-written on `std`

Date: 2026-10-15

Status: Accepted

## Context

Plan6 M1. An agent should be able to write shaders, inspect a scene and
debug a frame in the editor the user already has open, and in CI or on a
machine with no screen. ADR 0081 made `Command` and `Query` the editor's
whole vocabulary. What is left to decide is the transport, which thread
runs what, how tools map onto commands, and what an agent may touch.

The editor is single-threaded and owns the device: every compile, every
readback and every frame happens on the render thread. MCP clients speak
JSON-RPC either over stdio or over HTTP (the "streamable HTTP"
transport). Pulling an async runtime into the editor, or a web framework
into the workspace, would be the heaviest dependency the project has.

## Decision

- **A new crate, `wxsl-mcp`**, depending on `wxsl-editor` and
  `wxsl-render`. The facade re-exports it as `wxsl::mcp` behind an `mcp`
  feature (which implies `editor`), off by default. The editor does not
  know it exists.
- **Two transports, one dispatcher.**
  - HTTP: `POST /mcp` on a `127.0.0.1` listener, hand-written on
    `std::net`, one thread per connection. It answers with
    `application/json` and never opens an event stream (`GET` is
    `405`).
  - stdio: one JSON message per line.

  Both feed the same `Dispatcher`, which answers the protocol methods
  (`initialize`, `ping`, `tools/list`, `resources/*`) on its own thread.
  No tokio and no HTTP crate: `serde_json` was already a dependency.
- **Threading.** A `tools/call` is planned on the transport thread: its
  arguments are parsed, its paths confined, and it becomes an `Action`.
  The action then crosses an `mpsc` channel to the render thread, and the
  transport waits for the reply. The host calls `Server::serve(&mut
  editor, …)` once per frame, which drains the channel. A headless host
  blocks in `Server::wait` instead of drawing frames.
- **Every tool is commands and queries.**
  - An edit is applied with `Editor::apply_as_agent`, so its undo step is
    marked as the agent's (`Query::History` lists the steps).
  - The editor then runs `Editor::sync`, the compile half of `frame`, so
    the diagnostics returned describe the edit.
  - `graph_edit` takes a list of raw `Command`s as one atomic batch;
    the other tools are shorthands for common commands.
  - The session state an agent needs gains commands, which are not undo
    steps: `set_canvas_mode`, `set_pipeline`, `set_debug_view` and
    `step_frame`, plus `Query::View`.
  - Two tools are not commands:
    - `function_save`, `Editor::define_function`, needs the device;
    - `view_screenshot` draws two editor frames into an offscreen target
      and returns a PNG as MCP image content.
- **Tool names use underscores** (`graph_edit`, not plan6's
  `graph.edit`), because several clients refuse dots in tool names.
- **Security.**
  - The listener binds `127.0.0.1` only.
  - Every HTTP request needs `Authorization: Bearer <token>`. The token is
    random per run unless the host sets one. A request whose `Origin` is
    not local is refused, which defeats DNS rebinding.
  - Every path a tool takes, including the paths inside raw commands, is
    resolved under the project root, and is refused if it escapes it.
  - Saving is an explicit tool. Everything else can be undone.
  - While a client is initialized, the editor's status bar shows the
    agent's name.

## Alternatives considered

- **The official Rust SDK (`rmcp`) on tokio.** It is complete and
  maintained, but it brings an async runtime and a large dependency tree
  into an editor that has no other use for them. The subset used here is
  small: two transports, three methods with content, no sampling and no
  server-sent notifications.
- **A channel inside `Editor`, drained by `Editor::frame`.** It would save
  the host one call per frame. But the editor would then know about
  replies, timeouts and agents' threads, and its test surface would grow
  for something only one host uses.
- **SSE streaming responses.** Nothing the server does is long enough to
  need progress events. A JSON reply is the simplest form the transport
  allows.

## Consequences

- The protocol, the tool plans, path confinement and HTTP parsing are
  tested with no device. A headless editor is driven over HTTP and
  in-process in `cargo test -p wxsl --features mcp --test mcp`, which
  skips without an adapter.
- The `editor` example gains `--mcp [PORT]` and `--mcp-stdio --headless`.
- A new tool is a plan in `wxsl_mcp::tools` that maps onto existing
  commands and queries. If it needs a new capability, that is a new
  `Command` or `Query` first (ADR 0081).
- The debug views and the pixel probe of E2 arrive as queries, and their
  tools follow without changing the transport.
- An HTTP client's session is not tracked across restarts. The agent
  indicator clears when the client sends `DELETE /mcp` or when stdin
  closes.
