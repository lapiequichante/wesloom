# AGENTS.md

Instructions for AI coding agents (and a useful refresher for humans)
working in this repository. If you're an agent: read this file, then
`docs/architecture.md`, then the ADR whose number is referenced by the code
you're about to touch, before writing anything nontrivial.

## What this repo is

`wesloom` is a Rust workspace for building shader graphs in
[WESL](https://wesl-lang.dev) (WGSL Extended) visually or programmatically,
and running them through a `wgpu` renderer that can switch between forward
and deferred pipelines without the graph author doing anything special. It
also ships an original base node library (`wesloom-stdlib`) covering the
usual granular shader-function ground (math, color, lighting, SDFs, …) —
see [ADR 0007](docs/adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md)
for why this is written from scratch rather than ported from an existing
library.

**Status: implemented, except the editor.** The graph model, WESL codegen,
node library, and the forward/deferred `wgpu` renderer all work and are
tested end to end; `crates/wesloom/examples/pbr_cube.rs` is the demo to run
first. `wesloom-editor` is still module stubs with no UI. Each module's doc
comment says what it holds and which ADR governs it; that's the source of
truth for "what goes here," not this file.

## Map of the workspace

Full detail, diagrams, and the "why" live in `docs/architecture.md`. Short
version:

| Crate | Depends on | Needs GUI toolkit? | Needs wgpu? |
|---|---|---|---|
| `wesloom-core` | nothing in-workspace | no | no |
| `wesloom-render` | `wesloom-core` | no | yes |
| `wesloom-editor` | `wesloom-core` | yes | no (delegates drawing to `wesloom-render` at the app level) |
| `wesloom-stdlib` | `wesloom-core` | no | no |
| `wesloom` (facade) | all of the above, behind features | via `editor` feature | via `render` feature |

The dependency arrows only ever point *into* `wesloom-core`. Never make
`wesloom-core` or `wesloom-render` depend on `wesloom-editor` or
`wesloom-stdlib` — that's the whole point of the split (ADR 0002).

## Before you start a nontrivial change

1. Check `docs/adr/` for an existing decision that covers the area. If one
   exists and your change conflicts with it, say so and propose superseding
   it (see "Writing an ADR" below) rather than quietly working around it.
2. If no ADR covers a nontrivial architectural choice you're about to make
   (a new public trait boundary, a new crate, a new dependency that affects
   binary size or compile time, a change to the feature-flag matrix), write
   one first. "Nontrivial" is a judgment call; when in doubt, write it —
   a short ADR costs little and saves the next agent from re-deriving your
   reasoning.
3. Prefer extending an existing module stub over adding new top-level
   modules. The stubs in each crate's `src/lib.rs` are the intended shape
   of the codebase, not just placeholders to delete.

## Writing an ADR

Copy `docs/adr/template.md` to `docs/adr/NNNN-short-title.md` (next
sequential number, check `docs/adr/` for the current max), fill it in, and
link it from `docs/adr/README.md`. Keep it short: context, decision,
consequences. If it changes a decision made in a previous ADR, mark the old
one "Superseded by NNNN" rather than deleting it.

## Licensing — read this before adding to `wesloom-stdlib`

Every crate in this workspace, including `wesloom-stdlib`, is plain
MIT/Apache-2.0 — there is no special-cased crate anymore
(`wesloom-lygia` was tried and dropped, see
[ADR 0006](docs/adr/0006-lygia-port-licensing-and-isolation.md), superseded
by [ADR 0007](docs/adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md)).
That simplicity depends on one rule holding: **every function in
`wesloom-stdlib` is original code.** It's fine to look at how LYGIA,
Babylon.js, papers, or any other reference solves a problem to learn the
*technique*; it is not fine to transcribe or lightly rename someone else's
implementation, regardless of that source's license — that's a derivative
work, and reintroduces exactly the problem ADR 0007 removed. See
`crates/wesloom-stdlib/shaders/README.md` for the full authoring rule and
where new functions go.

## Build, test, and lint commands

Run from the workspace root:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo check -p wesloom --no-default-features        # graph model only: no wgpu, no GUI
cargo tree -p wesloom --no-default-features -e normal | grep -E 'wgpu|winit|egui'  # must print nothing
```

Because the whole point of the feature split is that certain combinations
must compile *without* certain dependencies, don't just check
`--all-features` and call it done — at minimum also check
`-p wesloom --no-default-features` and `-p wesloom --features editor` (which
should pull in `wesloom-render` transitively, per ADR 0002) before
considering a change to the crate/feature boundaries finished. Note that
`--workspace --no-default-features` still builds `wesloom-render`, whose
`wgpu` dependency is not optional — the "no wgpu" guarantee is about the
facade crate's feature set, not about the workspace.

### What the tests cover

- `cargo test -p wesloom-core` — the graph model: typing, cycle rejection,
  validation, codegen, macro precedence. Fast, no GPU, no shader compiler.
- `cargo test -p wesloom --test graph_to_wgsl` — the real `wesl` compiler
  over the real shader sources: **every node in the library** compiled on
  both render paths, the demo graph, macro switching, node-format round trip.
  This is the test that catches a node descriptor disagreeing with its WESL.
- `cargo test -p wesloom --test render_cube` — renders on a real device and
  compares the two paths' images. Skips (prints a note, passes) when no
  adapter is available, so don't read a pass as proof it ran.
- `cargo run -p wesloom --example pbr_cube -- --headless` — the fastest way
  to see whether a change to the ABI or the pipelines still produces a
  picture. Writes a PNG per path and reports how far apart they are.

## Conventions

- `unsafe` is forbidden in `wesloom-core` (see its `Cargo.toml` lints) and
  should be avoidable everywhere else too; if you think you need it,
  justify it in the PR description and keep the unsafe block minimal.
- Keep `wesloom-core` free of `wgpu` and GUI-toolkit dependencies, full
  stop — that boundary is the reason the crate exists.
- New stdlib functions live under `crates/wesloom-stdlib/shaders/<category>/`
  (see that directory's `README.md` for the category layout, the originality
  rule, and the three authoring rules that keep a function reachable from a
  graph).
- The shader ABI has two halves that must be edited together:
  `wesloom_core::abi`'s tables and `crates/wesloom-stdlib/shaders/wesloom/`.
  Same for the uniform layouts: `wesloom_render::scene`'s `#[repr(C)]`
  structs mirror `shaders/wesloom/bindings.wesl`. See ADR 0008.
- Anything a node needs that cannot be a socket value — a loop bound, a code
  switch — is a macro variable (`wesloom_core::macros`), declared on the node
  definition. Don't reach for string substitution or a second graph.
- Prefer editing an existing ADR's "Consequences" section to record drift
  over silently diverging from what it says.

## Where things are

- `crates/wesloom/examples/pbr_cube.rs` — the demo, and the shortest
  complete example of the whole pipeline. `--dump-wesl` and `--dump-wgsl`
  show what a graph compiles to, `--list-nodes` and `--list-macros` what is
  available.
- `crates/wesloom/assets/pbr_cube.wesloom.json` — the node format, with
  comments in the file explaining it.
- `docs/architecture.md` — crate graph, data flow, the forward/deferred
  shader-switching design, macro variables, feature-flag matrix.
- `docs/adr/` — the decision log. Start at `docs/adr/README.md`.
- `docs/glossary.md` — terms (WESL vs WGSL, node graph vs render graph,
  forward vs deferred, etc.) used without re-explanation elsewhere.
