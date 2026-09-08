# 0002. Cargo workspace layout and crate boundaries

Date: 2026-09-08

Status: Accepted

## Context

The project has three requirements that all shape compile-time dependency
graphs, not just runtime behavior:

1. Consumers who only need the shader graph model and compiler (say, a game
   engine loading pre-authored graphs at runtime) must not be forced to
   compile or link a GUI toolkit just because the *project* also has a
   visual editor.
2. Consumers who want the visual editor need a GUI toolkit and, to actually
   preview a graph while editing it, a `wgpu` renderer — but that's a
   choice they opt into, not a default.
3. The base node library (originally planned as a LYGIA port, see ADR 0006
   and its successor ADR 0007) is a large, independently-growing body of
   shader source; a consumer with a small custom node set shouldn't have to
   compile all of it just because it depends on the graph model.

A single crate with Cargo features can express "optional dependency," but
it can't express "this dependency direction must never exist" as a
structural guarantee — that requires separate crates, since nothing stops
a future feature from accidentally introducing a `wesloom-core -> wesloom-editor`
edge inside one crate the way it would immediately be visible as a new
`[dependencies]` line across crates.

## Decision

A Cargo workspace with one crate per concern:

- **`wesloom-core`** — the node/socket/graph data model and the graph → WESL
  codegen. No `wgpu`, no GUI toolkit, ever. This is the crate a headless
  runtime consumer (e.g. loading a baked graph in a shipped game) depends
  on.
- **`wesloom-render`** — the `wgpu` renderer: pipeline abstraction and the
  forward/deferred shader-variant switching (ADR 0005). Depends on
  `wesloom-core` only.
- **`wesloom-editor`** — the visual node editor UI (a GUI toolkit
  dependency, see ADR 0004). Depends on `wesloom-core` only. Nothing
  depends on this crate.
- **`wesloom-stdlib`** — the base node library: original shader functions
  (math, color, lighting, SDFs, …) exposed as `wesloom-core` node
  definitions (see ADR 0007). Depends on `wesloom-core` only, ordinary
  workspace MIT/Apache-2.0 license.
- **`wesloom`** — the facade most consumers depend on directly. Re-exports
  the above behind Cargo features (`render` and `stdlib` default-on,
  `editor` default-off) so a given build only pays for what it enables.

The rule that matters more than the exact crate names: **dependency arrows
only ever point into `wesloom-core`.** `wesloom-core` and `wesloom-render`
must never depend on `wesloom-editor` or `wesloom-stdlib`, in either the
crate graph or the facade's feature graph.

## Alternatives considered

- **One crate, features for `editor`/`render`/`stdlib`.** Rejected: an
  `optional` dependency still gets compiled and linked whenever some other
  enabled feature happens to need it transitively, and there is no
  Cargo-enforced way to say "this feature must never depend on that one" —
  it relies entirely on the crate's authors not adding the wrong `dep:`
  edge in `[features]`, which is exactly the kind of rule that erodes over
  many independent agent sessions without a structural backstop.
- **Separate repos per crate.** Rejected as premature: nothing here needs
  independent versioning or release cadence yet, and a workspace keeps
  cross-crate refactors (which will be common while `wesloom-core`'s data
  model is still settling) to a single PR.

## Consequences

- Adding a dependency to `wesloom-core` is a bigger deal than adding one to
  `wesloom-editor` — it's shared by every consumer regardless of features
  enabled. Justify it in the PR/commit description.
- CI (and any agent finishing a change to the crate boundaries) must check
  `cargo check --workspace --no-default-features` in addition to
  `--all-features`; a change that only compiles with everything enabled can
  still silently violate the "core has no GUI/wgpu deps" rule if checked
  the wrong way. See `AGENTS.md`'s command list.
- `wesloom-render` not depending on `wesloom-editor` means the editor is
  responsible for wiring itself to a renderer at the application level
  (e.g. an example binary), not the other way around.
