# 0032. Stage analysis computes the cut between stages

Date: 2026-09-12

Status: Accepted

Amends [ADR 0025](0025-a-material-graph-spans-shader-stages.md) (a shared
node is no longer always compiled into both stages) and
[ADR 0027](0027-a-graph-computes-its-own-interpolants.md) (the author no
longer has to hand-declare every cross-stage value).

## Context

ADR 0025 partitioned a material graph by terminal: whatever fed the vertex
output compiled into the vertex stage, whatever fed the surface compiled
into the fragment stage, and a node reachable from both was *emitted
twice* — deliberately, because sharing meant the vertex stage handing a
value to the fragment stage, "which is a varying — a scarce resource with
an accountant — and a decision no compiler should make on the author's
behalf". ADR 0027 then built the hand mechanism: declare an interpolant,
wire `output.varying` on the vertex side, read `input.attribute` on the
fragment side.

Three things followed from using the mechanism by hand that should not
have been the author's job. Every cross-stage value was a three-step
wiring ritual. A vertex-only read (object space) under a fragment terminal
was a flat error — `GraphError::WrongStage` — even though interpolating
object space down is exactly what the stage boundary is *for*. And moving
a computation between stages meant rewiring, when it is semantically one
edit ([request.md](../../../request.md)).

## Decision

`wxsl_core::stages::analyze` decides where every node runs, before
codegen partitions anything:

* **A per-node constraint, `Auto` by default.** `Node.stage` carries
  `Auto`/`Vertex`/`Fragment`; `Auto` means *earliest stage that can
  produce me and satisfies every consumer* — so a node both stages read
  is computed once, per vertex. An explicit constraint is the author's
  override, and names the old behaviour where wanted: `Fragment` on a
  shared node pins the duplicate-in-both-stages behaviour ADR 0025 chose.
* **The cut is synthesized.** A vertex-stage value a fragment consumer
  reads becomes a synthesized interpolant — the same mechanism ADR 0027
  built, declared by the analysis instead of the author, named `autoN`,
  taking its location from the same accountant under the same
  16-location budget. Codegen emits it exactly as a hand-wired one: a
  vertex-stage function of its own, written by the vertex entry, read
  through the attributes struct. There is no second mechanism; there is
  one mechanism, with a compiler-driven author.
* **What cannot ride an interpolant is duplicated, visibly.** A bool, a
  matrix, or a cut that does not fit the budget falls back to computing
  the node in both stages — what the graph always did, never an error,
  and recorded in the plan (`StagePlan::duplicated`) so the editor can
  say so. An *explicit* vertex constraint that cannot reach the fragment
  stage is an error naming the node, not a silent duplicate: the author
  asked for the vertex stage.
* **The stage rules move into the analysis.** The two `WrongStage` walks
  ADR 0025/0027 added to validation are superseded: `Auto` never
  mis-places a node, so those walks' only remaining target is an explicit
  constraint naming an impossible stage, and `Graph::validate` now runs
  the analysis — which is why the editor's problem panel and the compiler
  cannot disagree about where a node runs.
* **The plan is data.** `StagePlan` answers stage-per-node, cuts, and
  duplications as pure data from a device-free function, which is what
  the editor's stage display will read.

The acceptance test is an image equality: a graph the compiler placed
(no stage machinery in it at all) renders — on hardware — the same picture
as the same value hand-wired through a declared interpolant.

## Alternatives considered

* **Keep terminal-driven placement, polish the errors.** The status quo
  the request argued against: the mechanism existed but only at the
  author's expense.
* **Auto-duplicate always; cuts only on explicit `Vertex`.** Smaller
  step, and it leaves the common graph paying double evaluation for
  something the compiler could share. The scarcity argument is met by
  the budget: a cut costs one location or it does not happen.
* **Silently duplicate when an explicit `Vertex` constraint cannot reach
  the fragment stage.** Rejected: the author made a decision; quietly
  not doing it is worse than an error that names the reason.

## Consequences

* A graph with no node shared across the stage boundary generates exactly
  what it did before this ADR (`StagePlan::is_empty`); the existing
  suites pass unchanged, and the interpolants tests only needed their
  error-message wording kept, not their expectations changed.
* Reading object space into a fragment-bound value is no longer an error
  by itself — it is a vertex computation and a synthesized interpolant,
  which is the honest semantics. Pinning `Fragment` on such a node still
  is.
* `StagePlan::cuts` names the interpolants `auto0, auto1, …` — internal
  names, skipped past any declared attribute so they cannot collide in
  the attributes struct. They are stable within one compilation, not
  across edits; nothing should pin them.
* The editor work that shows the computed stage per node is still owed;
  `StagePlan` is the data it will read (plan2 P5/P9).
* If this changes, also update `docs/architecture.md`'s stage
  description, `crates/wxsl/tests/stage_analysis.rs` (the acceptance
  test), and ADR 0025/0027's amended clauses here.
