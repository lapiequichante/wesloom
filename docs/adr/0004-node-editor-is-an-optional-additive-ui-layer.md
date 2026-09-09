# 0004. Visual node editor is an optional, additive UI layer

Date: 2026-09-08

Status: Accepted; amended by [0013](0013-the-editor-draws-itself-with-wxsl-render.md)

## Context

The project must work as a real embeddable library for two very different
consumers: an application shipping a visual shader-graph editor to end
users (e.g. a tool for artists), and a runtime that just needs to load a
graph authored elsewhere and compile/run it (e.g. a game loading a baked
`.wxsl`/graph asset with no editor UI in the shipped binary at all). The
second consumer must not pay — in compile time, binary size, or transitive
dependencies — for a GUI toolkit it never uses.

## Decision

The visual node editor lives entirely in `wxsl-editor` (ADR 0002),
depends only on `wxsl-core`, and is reached through the `wxsl` facade
only via an opt-in `editor` Cargo feature (default off). `wxsl-core`'s
graph/node/codegen types carry no notion of "how to draw this" — no widget
trait, no color/layout metadata baked into the core node definition.
Whatever visual representation a node needs (position, color, custom
widgets for its inputs) is data the editor layer owns and associates with a
core graph, not data the core graph owns about itself.

Concretely: a `wxsl-core::graph::Graph` must be fully constructible,
serializable, and compilable with the `editor` feature (and therefore the
whole `wxsl-editor` crate, and its GUI toolkit dependency) absent from
the build entirely.

## Alternatives considered

- **Put lightweight display hints (name, category, color) directly on the
  core node trait.** Rejected: even "lightweight" display data tends to
  grow (icons, widget kind per socket, layout hints), and once it's on the
  core trait every headless consumer carries it whether or not anything
  ever renders it. Keeping the core node trait describe only *interface*
  (sockets, types, WXSL emission) and letting the editor maintain its own
  side-table of per-node display data keeps that growth contained to the
  crate that actually needs it.
- **Make the editor the primary crate and the headless path the "reduced"
  build.** Rejected: inverts the actual constraint, which is that the
  headless/runtime path is the one that must never regress by accident.
  Structuring the workspace so the GUI-free crate is the foundation (ADR
  0002) makes that the default outcome instead of something that has to be
  separately verified.

## Consequences

- Any new core node capability must be expressible without reference to how
  it looks; if a node needs, say, a color picker in the editor, the editor
  decides that from the socket's *type*, not from editor-specific metadata
  stored on the node.
- `wxsl-editor` needs its own persistence for display data (node
  positions, etc.) alongside whatever `wxsl-core` uses to serialize the
  graph itself — these are two related but separate documents, not one.
- CI must verify `cargo check -p wxsl-core` and
  `cargo check --workspace --no-default-features` both succeed with no GUI
  crate in the dependency tree (see `AGENTS.md`).
- **Amendment (ADR 0013).** The last bullet of the Decision above — that the
  editor delegates drawing and stays wgpu-free — did not survive contact with
  the work: `wxsl-editor` now depends on `wxsl-render` and draws itself
  with it, rather than describing its UI to a third-party GUI toolkit. What
  this ADR decided that *does* still hold is everything about `wxsl-core`:
  the core graph model carries no display metadata, the editor keeps its own
  side-table (and derives widgets from socket *types*), and a graph is fully
  usable with the whole editor crate absent from the build.
