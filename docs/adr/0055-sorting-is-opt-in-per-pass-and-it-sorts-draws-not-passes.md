# 0055. Sorting is opt-in per pass, and it sorts draws, not passes

Date: 2026-10-10

Status: Accepted

## Context

ADR 0005 left draw ordering to the application, on the reasoning that a
renderer which owns no scene graph has nothing to order with. That held
while every frame was one cube; it stopped being enough the moment plan5
proposed tiered transparency (D5 needs a back-to-front tier to exist) and
the owner asked for the Babylon.js rendering-group control — one object
pinned in front of the others, per draw, without authoring a pass per
level.

Two orderings were being confused. *Pass-level* ordering already exists
and is already data: a pipeline document lists passes, the scheduler
orders them by resources, and declaration order breaks ties — the
overlay-pass idiom. *Draw-level* ordering inside one pass did not exist.
Adding it had to answer a recorded decision (the application owns order),
a correctness constraint (a draw's index in the draw list is its row in
the instance buffer, ADR 0024 — and ADR 0046's velocity stage reads
previous-frame rows by the same index), and the tier boundary (the
painter's algorithm is sound only for non-interpenetrating draws).

## Decision

* `PassDesc` grows **`sort: SortOrder`** — `None` (submission order, the
  default and every pass list's behaviour before this field), `FrontToBack`
  (early-z), `BackToFront` (the painter's tier). A pipeline document pins
  it with the `sort` setting on a `pass.geometry` node; a name that is not
  one is a named compile error.
* A **`render_order: i32`** on `MaterialConfig` (through resolution to
  the draw, ADR 0038's road; default 0) is the per-object group control.
  The sort key is `(render_order, camera depth)`; `BackToFront` flips the
  depth term, never the order term — groups still draw ascending, far
  before near *within* each group.
* The renderer sorts a pass's draws in the frame compile, from the draw
  list the application handed it. **The instance rows are untouched**:
  each recorded draw carries its own row index, so a sorted pass draws
  row *k* whenever it likes and the previous-frame rows line up
  regardless — ADR 0046's velocity chain is sound under sorting by
  construction, and the `motion` suite guards it.
* `render_order` is undefined across passes. Passes order passes
  (documents, scheduler); draws order draws inside one pass; fragments
  order by depth or peeling. One ordering system per level.

This amends ADR 0005's application-owns-order note in one direction
only: the application's ownership survives as the opt-out. The renderer
sorts *the draw list the application handed it* — it derives nothing,
culls nothing, owns nothing between frames. There is no scene graph on
the other side of this ADR, and the `None` default is the proof.

## Alternatives considered

* **Tags as groups** (`opaque && group1`): works, but every level is a
  string the material must carry and every pass must name; an integer on
  the draw says the same thing with no vocabulary to grow.
* **Sort keys supplied per draw by the application** (a `with_sort_key`
  on `DrawItem`): moves the work back to the application, which is the
  decision being amended — and the camera, which the application should
  not have to consult to say "nearest first".
* **Sorting transparents only**: tempting, but the opaque tier is where
  early-z pays, and the groups semantics (an order-1 opaque over an
  order-0 transparent) want one key everywhere.

## Consequences

* plan5's D3 and D4 land on this ADR; D5's `max_layers` tiers read the
  back-to-front tier from it.
* The mixed-tier rule is stated, not solved: sorted transparents
  composite after the peeled composite, sound when no sorted-only draw
  interpenetrates a peeled one. Detecting interpenetration is the
  problem peeling exists to solve; the renderer does not attempt it.
* The gallery and preset documents are unchanged — nothing sorts until a
  document asks.
* If this ever grows (culling, visibility, between-frame state), that is
  a new ADR against this one's line, not an extension of it.
