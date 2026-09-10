# 27. A graph computes its own interpolants

Date: 2026-09-10

Status: Accepted

Amends [0024](0024-a-material-declares-the-geometry-it-requires.md),
[0025](0025-a-material-graph-spans-shader-stages.md)

## Context

ADR 0024 gave a material a way to say what it needs from its geometry:
declare an attribute, name it from a node, and the mesh or the draw
supplies it. ADR 0025 gave a graph a vertex stage of its own.

Between them there is a gap. A graph can now compute something per vertex
— that is what the vertex partition is — but the only thing it may do
with the result is offset the position. Anything else it works out up
there is thrown away, and a fragment-side node wanting a per-vertex
quantity has to recompute it per fragment, which is the cost the vertex
stage exists to avoid.

The mechanism is already sitting there. ADR 0024's accountant counts
inter-stage locations, the generated IO structs already carry extra
`@location`s beside the ABI's, and `MaterialAttributes` is already the
struct a material function receives them in. What is missing is a way to
say *this value comes from the graph's own vertex stage* rather than from
the mesh.

## Decision

**A computed interpolant is a third attribute frequency.**
`AttributeFrequency::Computed`, beside `Vertex` and `Instance`. The graph
declares a name and a type exactly as it does for the other two.

**The reading node does not change.** `input.attribute` reads all three,
because from the fragment side there is nothing to tell them apart: a
stream the mesh carried and a value the vertex partition computed both
arrive as a `@location` that was interpolated. This is ADR 0024's own
claim — which backing a name has is the declaration's business and not the
node's — extended by one, and it means moving a value from `computed` to
`instance` because it turned out to be uniform per object is a one-line
edit to the declaration with no rewiring at all.

**The writing side is a terminal.** `output.varying`, carrying the name as
a setting, is the vertex-stage root for one interpolant. Several per
graph, unlike the other three terminals, because there is one per
declaration: each is its own partition, compiled into
`wxsl_varying_<name>`, and two interpolants from unrelated subgraphs cost
only what each of them reads.

**One typing rule, mirroring ADR 0025's.** A vertex-only node may not feed
a fragment terminal; a computed interpolant may not be read from a vertex
terminal, because that is the stage computing it. Both are
`GraphError::WrongStage`, both name the node and say which stage may do
what. Everything else stays reachability.

**Interpolants are numbered after everything the geometry brings**, in
name order among themselves. So declaring one never moves a per-vertex
attribute's location — and an attribute's location is in the *vertex
buffer layout*, which means moving it is a rebuilt pipeline and a
re-uploaded mesh for geometry that did not change.

**A stage with no fragment program computes none of them.** An
interpolant exists to be read per fragment; a plain depth prepass has
nothing to hand one to, so it does not run the subgraph. The location
stays declared in the vertex output struct either way, because the
interface is per material (ADR 0025) — which makes this a saving in the
vertex stage rather than a second pipeline layout.

**A declaration nothing writes is an error, and so is a writer naming a
name the geometry supplies.** The first spends an inter-stage location on
an undefined value; the second is a contradiction rather than an override.

## Alternatives considered

**A pair of matched nodes with no declaration** — `output.varying` and
`input.varying`, each carrying the name, and the type inferred from
whatever is wired into the writer. Fewer moving parts, and it puts the
type in a place two nodes can disagree about. The declaration is also
where the *budget* is enforced, and a budget over things that only exist
implicitly is a budget nobody can read.

**A fourth field on `GraphOutputs` holding one interpolant.** Enough for
the first use, and wrong the moment a graph wants two. The other three
terminals are singular because there is one surface, one offset and one
discard decision; there is no such number for interpolants.

**One function returning every interpolant at once.** One call in the
vertex entry instead of *n*. It also means a graph computing two of them
from unrelated subgraphs evaluates both to get either, which is the same
mistake ADR 0025 declined for the surface and the discard test.

**Letting a vertex-stage node read a computed interpolant**, resolving the
order between them. It is a dependency graph and it would work, and it
would mean two very different things — "compute this here" and "read what
was interpolated" — sharing a node whose meaning depended on where it
happened to sit.

**Numbering in declaration order.** Simpler, and it renumbers everything
downstream when somebody reorders a list. Name order was already the rule
for the other two (ADR 0024) for the same reason.

## Consequences

`GeometryInterface` now describes something the geometry does not supply.
The name is a little wrong and the placement is not: it is the one
accountant for inter-stage locations, and an interpolant is one. The
budget error now names all three kinds of spender.

A material that declares no interpolants generates exactly what it did
before — no `wxsl_varying_` function, no varyings struct — and a test
asserts it for every stage.

The vertex entry now binds `vertex_context(input)` once, into `vtx`,
instead of inlining the call into the transform. Everything in the vertex
stage reads the same *undisplaced* context: the displacement is computed
from it rather than from its own result, and so is every interpolant.

`MaterialAttributes` carries the interpolants in the vertex stage too,
where they are the value being computed rather than a value to read. They
are left zeroed there, and the typing rule above is what stops anything
reading them.

Nothing in `wxsl-stdlib` uses this yet. It is the mechanism a later
milestone's nodes need — anything expensive and smooth: a per-vertex wind
phase, a triplanar blend weight, a cheap curvature term.

If this changes, also update `wxsl_core::abi`'s `INTERPOLANT_TYPES` and
`varying_output_def`, and `docs/architecture.md`'s "What a material
declares".
