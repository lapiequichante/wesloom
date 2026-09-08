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

**Status: scaffolding.** As of this writing there is no rendering, no
compiler integration, and no editor UI — just the crate boundaries, feature
flags, and documentation this file is part of. Every module stub in the
crates below has a doc comment saying what it's for and which ADR governs
it; that's the source of truth for "what to build here," not this file.

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
cargo fmt --check
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo check --workspace --no-default-features   # must succeed with zero GUI/wgpu deps pulled in
```

Because the whole point of the feature split is that certain combinations
must compile *without* certain dependencies, don't just check
`--all-features` and call it done — at minimum also check
`--no-default-features` and `-p wesloom --features editor` (which should
pull in `wesloom-render` transitively, per ADR 0002) before considering a
change to the crate/feature boundaries finished.

## Conventions

- `unsafe` is forbidden in `wesloom-core` (see its `Cargo.toml` lints) and
  should be avoidable everywhere else too; if you think you need it,
  justify it in the PR description and keep the unsafe block minimal.
- Keep `wesloom-core` free of `wgpu` and GUI-toolkit dependencies, full
  stop — that boundary is the reason the crate exists.
- New stdlib functions live under `crates/wesloom-stdlib/shaders/<category>/`
  (see that directory's `README.md` for the category layout and the
  originality rule).
- Prefer editing an existing ADR's "Consequences" section to record drift
  over silently diverging from what it says.

## Where things are

- `docs/architecture.md` — crate graph, data flow, the forward/deferred
  shader-switching design, feature-flag matrix.
- `docs/adr/` — the decision log. Start at `docs/adr/README.md`.
- `docs/glossary.md` — terms (WESL vs WGSL, node graph vs render graph,
  forward vs deferred, etc.) used without re-explanation elsewhere.
