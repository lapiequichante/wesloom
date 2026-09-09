# AGENTS.md

Instructions for AI coding agents (and a useful refresher for humans)
working in this repository. If you're an agent: read this file, then
`docs/architecture.md`, then the ADR whose number is referenced by the code
you're about to touch, before writing anything nontrivial.

## What this repo is

`wxsl` is a Rust workspace for building shader graphs in
[WXSL](https://wesl-lang.dev) (WGSL Extended) visually or programmatically,
and running them through a `wgpu` renderer that can switch between forward
and deferred pipelines without the graph author doing anything special. It
also ships an original base node library (`wxsl-stdlib`) covering the
usual granular shader-function ground (math, color, lighting, SDFs, …) —
see [ADR 0007](docs/adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md)
for why this is written from scratch rather than ported from an existing
library.

**Status: implemented, editor included.** The graph model, WXSL codegen, node
library, the forward/deferred `wgpu` renderer, its 2D UI layer and the visual
node editor all work and are tested end to end.
`crates/wxsl/examples/pbr_cube.rs` is the demo to run first and
`crates/wxsl/examples/editor.rs` the second. Each module's doc comment says
what it holds and which ADR governs it; that's the source of truth for "what
goes here," not this file.

## Map of the workspace

Full detail, diagrams, and the "why" live in `docs/architecture.md`. Short
version:

| Crate | Depends on | Needs GUI toolkit? | Needs wgpu? |
|---|---|---|---|
| `wxsl-core` | nothing in-workspace | no | no |
| `wxsl-render` | `wxsl-core` | no | yes |
| `wxsl-editor` | `wxsl-core`, `wxsl-render`, `wxsl-lang` | no toolkit — it draws itself | yes (ADR 0013) |
| `wxsl-stdlib` | `wxsl-core` (plus `wxsl-lang` at build time only) | no | no |
| `wxsl` (facade) | all of the above, behind features | never | via `render`/`editor` features |

The dependency arrows point *into* `wxsl-core`, plus two more:
`wxsl-editor → wxsl-render`, because the editor draws itself with the
renderer rather than with a GUI toolkit (ADR 0013), and
`wxsl-editor → wxsl-lang`, because its code-panel syntax highlighting
reuses the compiler's own lexer rather than a second one (ADR 0016). Never
make `wxsl-core` or `wxsl-render` depend on `wxsl-editor` or `wxsl-stdlib`
— that's the whole point of the split (ADR 0002).

`wxsl-stdlib → wxsl-lang` is a **build**-dependency and must stay one: the
node library derives its function nodes from the `.wxsl` files at build time
(ADR 0020), so `cargo tree -p wxsl-stdlib -e normal` shows only
`wxsl-core`. Promoting it to a normal dependency would put a shader compiler
into every build of the node library.

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

## Licensing — read this before adding to `wxsl-stdlib`

Every crate in this workspace, including `wxsl-stdlib`, is plain
MIT/Apache-2.0 — there is no special-cased crate anymore
(`wxsl-lygia` was tried and dropped, see
[ADR 0006](docs/adr/0006-lygia-port-licensing-and-isolation.md), superseded
by [ADR 0007](docs/adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md)).
That simplicity depends on one rule holding: **every function in
`wxsl-stdlib` is original code.** It's fine to look at how LYGIA,
Babylon.js, papers, or any other reference solves a problem to learn the
*technique*; it is not fine to transcribe or lightly rename someone else's
implementation, regardless of that source's license — that's a derivative
work, and reintroduces exactly the problem ADR 0007 removed. See
`crates/wxsl-stdlib/shaders/README.md` for the full authoring rule and
where new functions go.

## Build, test, and lint commands

Run from the workspace root:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo check -p wxsl --no-default-features        # graph model only: no wgpu, no GUI
cargo tree -p wxsl --no-default-features -e normal | grep -E 'wgpu|winit|egui'  # must print nothing
```

Because the whole point of the feature split is that certain combinations
must compile *without* certain dependencies, don't just check
`--all-features` and call it done — at minimum also check
`-p wxsl --no-default-features` and `-p wxsl --features editor` (which
should pull in `wxsl-render` transitively, per ADR 0002) before
considering a change to the crate/feature boundaries finished. Note that
`--workspace --no-default-features` still builds `wxsl-render`, whose
`wgpu` dependency is not optional — the "no wgpu" guarantee is about the
facade crate's feature set, not about the workspace.

### What the tests cover

- `cargo test -p wxsl-core` — the graph model: typing, cycle rejection,
  validation, codegen, macro precedence. Fast, no GPU, no shader compiler.
- `cargo test -p wxsl --test graph_to_wgsl` — the real WXSL compiler
  over the real shader sources: **every node in the library** compiled on
  both render paths, the demo graph, macro switching, node-format round trip.
  A node whose WXSL does not compile fails here. (A node *descriptor*
  disagreeing with its WXSL is no longer a thing that can happen — the
  descriptor is derived from the WXSL, ADR 0020.)
- `cargo test -p wxsl --test render_cube` — renders on a real device and
  compares the two paths' images. Skips (prints a note, passes) when no
  adapter is available, so don't read a pass as proof it ran.
- `cargo test -p wxsl --features editor --test editor_frame` — drives the
  editor for several frames on a real device: that it draws, that editing
  recompiles, that a path switch changes the WGSL, that a frame of every
  input event leaves it drawing, and that the two MSDF backends agree. Skips
  with no adapter, like `render_cube`.
- `cargo run -p wxsl --example pbr_cube -- --headless` — the fastest way
  to see whether a change to the ABI or the pipelines still produces a
  picture. Writes a PNG per path and reports how far apart they are.
- `cargo run -p wxsl --features editor --example editor -- --screenshot out.png`
  — the same for the editor: one frame, no window, reviewable as a PNG. Then
  `cargo run -p wxsl-render --example glyph_field -- <FONT> c` when the
  problem is a glyph rather than a panel; it prints the distance field as
  text, and it is how the tie-break bug in `ui::msdf` was found.

## Conventions

- `unsafe` is forbidden in `wxsl-core` (see its `Cargo.toml` lints) and
  should be avoidable everywhere else too; if you think you need it,
  justify it in the PR description and keep the unsafe block minimal.
- Keep `wxsl-core` free of `wgpu` and GUI-toolkit dependencies, full
  stop — that boundary is the reason the crate exists.
- New stdlib functions live under `crates/wxsl-stdlib/shaders/<category>/`
  (see that directory's `README.md` for the category layout, the originality
  rule, and the authoring rules that keep a function reachable from a
  graph). **The file is the node** (ADR 0020): there is no descriptor to
  write in Rust. One `fn` per file; a label line then a documentation
  paragraph in the leading comment block; one parameter per line, each with
  its own trailing `// … @default <value>`.
- The shader ABI has two halves that must be edited together:
  `wxsl_core::abi`'s tables and `crates/wxsl-stdlib/shaders/wxsl/`.
  Same for the uniform layouts: `wxsl_render::scene`'s `#[repr(C)]`
  structs mirror `shaders/wxsl/bindings.wxsl`. See ADR 0008.
- The UI pass has *three* halves: `abi::UI_ATTRIBUTES`/`UI_KINDS`,
  `wxsl_render::ui::draw::UiInstance`, and `shaders/wxsl/ui.wxsl`. The MSDF
  generator has two implementations that must agree — `ui::msdf` on the CPU
  and `shaders/wxsl/msdf.wxsl` as a compute pass — and a test in
  `crates/wxsl/tests/editor_frame.rs` compares them. See ADR 0013 and 0014.
- Anything in the editor that can be tested without a device is factored so
  that it is: distance fields, atlas packing, line breaking, caret and
  hit-testing, draw-list batching, canvas geometry and input accumulation are
  all pure functions over data. Keep it that way — CI has no GPU.
- Anything a node needs that cannot be a socket value — a loop bound, a code
  switch — is a macro variable (`wxsl_core::macros`), declared on the node
  definition. Don't reach for string substitution or a second graph.
- Prefer editing an existing ADR's "Consequences" section to record drift
  over silently diverging from what it says.

## Where things are

- `crates/wxsl/examples/pbr_cube.rs` — the demo, and the shortest
  complete example of the whole pipeline. `--dump-wesl` and `--dump-wgsl`
  show what a graph compiles to, `--list-nodes` and `--list-macros` what is
  available.
- `crates/wxsl/assets/pbr_cube.wxsl.json` — the node format, with
  comments in the file explaining it.
- `docs/architecture.md` — crate graph, data flow, the forward/deferred
  shader-switching design, macro variables, feature-flag matrix.
- `docs/adr/` — the decision log. Start at `docs/adr/README.md`.
- `docs/glossary.md` — terms (WXSL vs WGSL, node graph vs render graph,
  forward vs deferred, etc.) used without re-explanation elsewhere.
