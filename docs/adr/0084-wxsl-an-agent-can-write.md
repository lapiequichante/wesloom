# 84. A generated reference, a checked guide, and shaders refused instead of panicking

Date: 2026-10-15

Status: Accepted

## Context

Plan6 M2: make WXSL writable by an LLM. M1 (ADR 0083) gave an agent the
tools. Recording the acceptance session ("make a material for rusty metal,
look at it, fix it") through them found what an agent actually trips on:

- The node reference was built at run time from `Query::Nodes`, so it
  could not be read outside a session, and nothing checked it.
- The editor wrote types two ways. The reference, the WXSL, and the
  diagnostics wrote `vec3f`, while the graph JSON accepted only `vec3`.
- `function_save` answered success for a function that cannot compile,
  for example one returning the wrong type or calling an undefined
  function. The WXSL compiler does not type-check bodies, and nothing
  validated the WGSL until a graph used the function.
- Then wgpu's uncaptured validation error **panicked the editor**. An
  agent could crash the user's window, and so could a person saving a
  function or hot-reloading a broken library file.

## Decision

- **One reference generator**, `wxsl_mcp::node_reference(shaders,
  pipeline)`. It writes, as Markdown, every node's id, label, doc, typed
  sockets with their defaults, generics, settings, macros and domain
  restrictions:
  - `wxsl://nodes` is generated from the running editor's registries, so
    it includes the session's `user.*` functions;
  - `docs/reference/nodes.md` is generated from the shipped registries by
    `cargo run -p wxsl-mcp --example node_reference`;
  - a device-free test fails when the file is out of date.
- **A short authoring guide**, `crates/wxsl-mcp/authoring.md`, served as
  `wxsl://guide`. It sits in the crate so that the crate packages it. Its
  snippets are checked in two places:
  - device-free, in `wxsl-mcp`: every call plans, every source derives,
    and every refused source fails with the quoted text, unless only the
    device refuses it;
  - on a device, in `crates/wxsl/tests/mcp.rs`: every snippet runs, in
    order, against the `editor` example's starting graph.
- **WGSL spellings are accepted** wherever the JSON names a type or tags a
  value: `vec3f` as well as `vec3`, `mat4x4f` as well as `mat4`. These are
  serde aliases, so documents are still written as before.
- **A shader module is validated where it is created.**
  `wxsl_render::variants::shader_module` creates it inside a validation
  error scope. A refusal becomes `RenderError::ShaderCompile` and goes to
  the problems panel instead of panicking. `ShaderVariants::insert` now
  returns a `Result`.
- **A function's save validates its WGSL.** `function::validate`
  compiles the module on its own and has the device check it, so a broken
  function is refused at the save that introduced it. A templated function
  has no WGSL of its own, so it is checked when a node first instantiates
  it.
- **The acceptance session is a recorded transcript**,
  `crates/wxsl/tests/transcripts/rusty_metal.jsonl`. It has one JSON-RPC
  message per line, with `#` notes, and is replayed by the `mcp` test.
  The test checks every answer, that each fix changed the picture, that
  the final picture is rust over steel, and that the history shows four
  steps, all marked as the agent's.

## Alternatives considered

- **naga in the editor, to validate without a device.** Rejected: naga is
  a test-only dependency of the workspace, and wgpu only re-exports it on
  some backends. The device is already there when a save runs.
- **Generating the reference in `wxsl-stdlib` or `wxsl-core`.** Rejected:
  only the editor's session knows the `user.*` nodes. The pipeline
  registry needs `wxsl-frame`'s effects, which `wxsl-stdlib` cannot see.
- **Rewriting the graph JSON to the WGSL spellings.** Rejected: it would
  change every saved document. Aliases change nothing that is written.

## Consequences

- When a node changes, run the example to regenerate the reference, or
  `node_reference` fails. It prints the command to run.
- On WebGPU the error scope answers asynchronously. There a module is
  returned unchecked, as before, and the browser reports the error.
- The guide quotes diagnostics, so changing a diagnostic's wording means
  changing the guide too. Its test names the snippet that drifted.
- A new recorded session is one more file in `tests/transcripts/` and one
  more test calling `replay`.
