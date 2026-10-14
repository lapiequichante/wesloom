# 81. Commands are the editor's single mutation path, undone by snapshot

Date: 2026-10-14

Status: Accepted

## Context

Plan6 M0. The editor changed its documents in place, from wherever a
widget happened to hold `&mut Graph`: the canvas, both inspectors, the
palette and the tidy button. So it had no undo. An agent (M1) or a test
could only reach it through a few `pub fn`s and `graph_mut()`. The scene
mode (E1) and the MCP server (M1) both need one vocabulary that the UI,
the tests and an agent share.

`Graph`'s own edits have side effects: connecting resolves generics along
a chain, disconnecting forgets them, and `set_generic` drops edges that
no longer fit. A hand-written inverse per command would have to reproduce
all of that, and would drift whenever `wxsl-core` changes.

## Decision

- **`Command`** (in `wxsl_editor`, serde, tagged `"command"`) names every
  mutation of the editor's documents. Graph commands take a `document`,
  either `material` (the default) or `pipeline`:
  - `add_node`, `remove_nodes`;
  - `connect`, which replaces whatever fed the input, and `disconnect`;
  - `set_param`, `set_setting`, `set_generic`, `set_macro`;
  - `move_nodes`, `set_label`, `set_color`, `tidy_layout`.

  Besides those:
  - `set_material_param` and `set_pass_param`, the live buffer writes;
  - `batch`, which is atomic: one failure restores everything;
  - `undo` and `redo`.
- **`Editor::apply(Command) -> Result<Outcome, CommandError>`** is the only
  path that changes a document. The UI's gestures emit commands into the
  frame's request list, which is applied after the widgets have run.
  `graph_mut` and `pipeline_graph_mut` are removed.
- **Undo is by snapshot.** Before a command, the editor keeps a copy of
  every document the command may touch, plus the old value of every pass
  parameter it sets. An undo puts them back and keeps the state it
  replaced as the redo. The inverse is therefore exact, whatever
  `wxsl-core` does to generics, and it is the same code for every
  command. A graph is a few kilobytes and the history holds 200 entries.
- **Gestures coalesce.** While the pointer is down or a text field has
  focus, a command that repeats the previous one's target merges into its
  entry: one drag, one slider move or one rename is one undo. An `apply`
  from outside the UI never merges.
- **`Query`** is the read-only counterpart. It covers the graph, the
  diagnostics, the generated WXSL/WGSL, the registry, the pass list, the
  variant stats and the history depth. `Editor::query` returns an
  `Answer`, which serializes.
- **Transcripts** are JSON lines of commands. `Command::parse_transcript`
  reads them, and replaying one is a regression test.
- The selection, the view, the preview's pipeline and the code editor's
  buffers are session state, not document state. They are not commands
  yet. Saving a function needs the device, so it stays
  `Editor::save_function` until M1 drains commands on the render thread.

## Alternatives considered

- **A semantic inverse per command** (`connect` ↔ `disconnect`). It is
  readable in a history list. But it is wrong as soon as generics
  re-resolve, and a node re-added by redo gets a new id, which breaks
  every later command that names it.
- **Diffing nodes and edges.** It costs less memory than a snapshot. But
  it needs `wxsl-core` to accept raw edges and chosen node ids, which is
  an API that bypasses the graph's invariants.
- **Commands in `wxsl-core`.** The graph edits would be reusable without
  the editor, but material parameters, pass parameters and the documents
  an editor holds are not core concepts.

## Consequences

- The command round trip (apply, undo, redo) and transcripts are tested
  without a device, against a small registry.
- `Ctrl`/`Cmd`+`Z` undoes and `Shift`+`Z` or `Y` redoes when no text has
  focus. The code editor keeps its own buffer undo.
- A new editing gesture is a new `Command` variant, never a direct
  mutation. E1 adds the scene commands, and M1's tools map onto
  commands and queries.
- Removing `graph_mut` and `pipeline_graph_mut` changes the stable editor
  surface (ADR 0078), and `api/wxsl-editor.txt` records it.
