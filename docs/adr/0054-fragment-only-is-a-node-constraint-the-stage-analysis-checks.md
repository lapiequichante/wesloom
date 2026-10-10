# 0054. Fragment-only is a node constraint the stage analysis checks

Date: 2026-10-09

Status: Accepted

## Context

A screen-space derivative (`dpdx`, `dpdy`, `fwidth`, …) is a function of
the pixel grid: it has no value in the vertex stage, and no interpolant
can carry one, because the value it computes *is* a measurement of how
the pixel grid samples a quantity. The stage analysis (ADR 0032) places
every node by reachability from the terminals and can refuse a placement
that cannot be honoured — but it had no way to *know* a node needed the
fragment stage, because a derivative call looks like any other function
call in the node's signature. The result was the worst kind of failure:
a graph that typed, validated, and was rejected by the WGSL compiler
three layers downstream, naming a builtin instead of the decision that
caused it.

This blocked the guide's ticket 2 and every fwidth-based node after it —
`roughness_aa` chief among them — and it is the prerequisite half of
plan5's ordering work (D1 there).

## Decision

A `NodeDefinition` may carry **`fragment_only`**, and the stage analysis
enforces it:

* The derivation sets it automatically when a node source's body names a
  derivative builtin (a word-shaped scan of the declaration, comments
  excluded); an author can also set it through
  `NodeDefinitionBuilder::fragment_only` for bodies that carry the
  constraint without revealing it.
* `Auto` never places a fragment-only node outside the fragment stage.
* An explicit vertex pin on a fragment-only node is
  `GraphError::WrongStage`, naming the node and the derivative.
* A vertex consumer of a fragment-only node's output is the same error —
  and it is an *error*, not the old answer. The analysis's fallback for
  a node both stages read is "compute twice", and computing a derivative
  twice means computing it once in a stage where it does not exist.
  There is no interpolant to spend.

Compute is covered where bakes are already checked: `generate_bake`'s
purity loop refuses a fragment-only node in a bake cone, since a bake is
a compute pass over the bake domain.

Screen-domain graphs need nothing: a fullscreen pass is a fragment
stage.

## Alternatives considered

* **Pin the node in the demo.** A per-instance `Fragment` stage pin
  protects one graph; the next graph pastes the same node into a vertex
  chain and fails downstream again. The constraint is a fact about the
  *body*, so it belongs on the definition.
* **Let the WGSL compiler be the error.** It is, today, and the error is
  a builtin name with no decision in it. The whole point of the named
  errors this codebase reports is that the failure names the choice that
  caused it.
* **Type-level marking in the language** (a `@fragment_only` attribute
  the parser carries). Rejected for the same reason ADR 0020 reads
  metadata from comments: the compiler would thread an attribute
  through every pass to answer a question one scan of the body settles.

## Consequences

* `roughness_aa` ships with this ADR — the first node the constraint
  unblocks — and the guide's ticket 2 closes. The fwidth-driven recipes
  (AA'd `sdf_coverage` widths, AA'd pattern nodes) are now authorable
  and corpus-gated.
* The negative cases are corpus: four tests in the stage analysis (auto
  placement, vertex consumer, vertex pin, the shared-node case that
  would once have duplicated) fail in about a second, with no device.
* build.rs's generated table emits the flag, so a derived node and its
  registered definition cannot disagree about it — the same property
  every other derived field has.
* What this does **not** decide: whether a *material* may demand forward
  shading under a deferred pipeline. That is plan5's D2, and its ADR is
  owed separately. Derivatives in a deferred pipeline's G-buffer pass
  are already legal — the pass is a fragment stage — so most
  derivative shading does not need D2 at all.
* If the derivative family grows (a new builtin the scan should catch),
  update `wxsl-lang`'s `names_derivative` list — the corpus gate's
  detection tests will say so by name.
