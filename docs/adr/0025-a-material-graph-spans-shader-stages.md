# 25. A material graph spans shader stages, and compiles per pass stage

Date: 2026-09-10

Status: Accepted, amended by [0026](0026-shadows-a-view-per-light-and-two-flags-on-the-material.md), [0027](0027-a-graph-computes-its-own-interpolants.md)

Amends [0008](0008-surface-graphs-and-a-named-shader-abi.md),
[0022](0022-material-stages-replace-the-render-path-enum.md)

## Context

Until now a material graph was one thing with one answer: a `Surface`,
evaluated in the fragment stage. Every stage compiled the same function
and differed only in what it did with the result — a colour, a G-buffer,
or nothing at all. ADR 0022 recorded the cost of "or nothing at all"
plainly: the depth-only module still carried the whole material function,
so a macro change that only affected colour recompiled the prepass too.

Two things a material has always wanted are not surface properties at all.

**Moving the vertex.** Wind, waves, growth, an inflate along the normal —
all of it happens before the geometry is rasterized, so no amount of
fragment-stage machinery can express it.

**Throwing the fragment away.** Foliage, chain-link, decals with cut-outs.
`SURFACE_FIELDS` has an `alpha`, but alpha is a blend weight the deferred
path cannot honour and blending does not stop depth being written. A
perforated material needs the holes to be holes.

Shadows are what force the issue, and they force *both* halves at once. A
shadow pass renders the same objects from a light's point of view and
writes only depth. If the graph's displacement is not compiled into it,
the object casts the shadow of its undisplaced shape. If the alpha test is
not compiled into it, a leaf casts the shadow of a rectangle. Those are
the two failures that are only fixed by compiling *part* of a material
into a stage that wants no surface at all.

So the question this answers is: what is a material graph, once it is no
longer one function?

## Decision

**A graph has three terminals, and each is a root.** `output.surface`
stays required and unique. `output.vertex` and `output.discard` are
optional and at most one each. `Graph::outputs` answers all three, and
codegen partitions the graph by reachability *from each terminal
separately* — three subgraphs, three functions, `wxsl_vertex`,
`wxsl_discard` and `wxsl_material`.

**A node reachable from two terminals is compiled into both.** Not
hoisted, not shared: emitted twice, in two functions, with its own `let`
bindings in each. Sharing would mean the vertex stage handing something to
the fragment stage, which is a varying — a scarce resource with an
accountant (ADR 0024) — and a decision no compiler should make on the
author's behalf. Two pure evaluations of the same expression are what a
shader compiler's common-subexpression pass exists for.

**A stage compiles only the partitions it needs.**
`MaterialStage::needs_surface` is the whole of it: a stage that writes no
colour gets the vertex offset and the discard test, and the material
function is not in the module at all. For a graph that neither displaces
nor discards, a depth-only module is now the vertex entry and nothing
else — which is exactly what ADR 0022 said it should be and could not yet
be.

**Whether a fragment entry exists is a property of the material.** A depth
or shadow stage needs a fragment program only when the material discards;
one that does not gets a pipeline with no fragment state. So
`MaterialStageDesc::fragment_entry` is now always a *name*, and
`GeneratedShader::fragment_entry` is the `Option` — answered per material,
and what the pipeline is built from.

**`VertexContext` is a superset of `SurfaceContext`.** Every field a
fragment-side node reads is in the vertex context too, under the same name
and type, computed before any displacement. So `input.uv`, `input.time`,
`input.world_position` and the rest are one node definition each, usable
in either stage, and which stage a node lands in is a reachability
question with no wrong answers. The only exceptions are
`abi::VERTEX_ONLY_FIELDS` — `object_position` and `object_normal` — which
the fragment stage has not got; a node reading one under a fragment
terminal is `GraphError::WrongStage`, and that is the single typing rule
partitioning needs.

**The vertex output is an object-space offset.** Object space, so a
displaced object still follows its own transform and a displacement along
the mesh's own normal means what it says. `transform_vertex_offset` is the
ABI function that applies it; `transform_vertex` is now that function with
a zero offset. The shading basis is deliberately *not* recomputed from the
displaced surface: doing it properly needs the derivative of the
displacement, which a graph does not hand over, and a confidently wrong
normal is worse than the undisplaced one.

**Discard is its own terminal, not a `Surface` field.** Putting it in
`Surface` would mean the deferred lighting pass unpacking a discard flag
from the G-buffer, which is nonsense — the fragment is long gone by then.
As its own terminal it is also its own *partition*, which is the point: a
shadow stage compiles the alpha subgraph and nothing else.

**The interface stays per material.** What a graph needs bound is computed
over the union of all three terminals' reachable sets, not per stage. A
bind group layout that narrowed per stage would mean a depth pass and a
shading pass wanting different bind groups for the same object; a pipeline
layout may be a superset of what its shader uses, so the union costs
nothing and the alternative costs a bind group rebuild per pass.

## Alternatives considered

**`@if`-gating one module per stage instead of partitioning.** The
faithful extension of ADR 0005, and ADR 0022 already chose against it for
this reason: a stage needs a different *body*, not a different entry
point, and conditional translation cannot express "the subgraph reachable
from that terminal".

**One fragment function returning both the surface and the discard flag.**
Avoids evaluating a shared node twice in a stage that needs both. It also
makes the depth stage compile the surface to get at the flag, which is the
entire thing this milestone exists to stop.

**`discard` as a field of `Surface`.** Smaller — no new terminal — and it
puts a control-flow decision in a struct the deferred path packs into a
G-buffer and unpacks a frame later.

**A separate vertex-stage input vocabulary** (`input.vertex_uv`,
`input.vertex_time`, …). Honest about the two structs being different
types, and it doubles the palette for no gain: the fields have the same
names and the same meanings, and an author moving a subgraph from one
stage to the other would have to rewire every input.

**A world-space vertex offset.** Reads more naturally for wind, and it
breaks instancing: two copies of a mesh at different transforms would
displace identically in world space rather than each in its own frame. A
graph that wants world space can transform an object-space offset itself.

**Narrowing the interface per stage.** Tempting, since a depth module
references none of the material's textures. It buys nothing — an unused
binding in a pipeline layout is free — and costs a distinct bind group per
stage for every object.

## Consequences

A depth prepass of an ordinary material now has no fragment shader at all,
and its module does not contain the graph.
`every_stage_compiles_from_the_same_graph` asserts both directions: the
shading stages contain the graph's nodes, and the prepass does not.

A graph gained a way to be wrong that it did not have: reading object
space from the fragment side. It is reported as `GraphError::WrongStage`,
naming the node, its definition, the terminal it feeds and why.

`transform_vertex` is now a one-line wrapper around
`transform_vertex_offset`, so the linked ABI's WGSL has one more function
in it than it did. The *generated* module for a material that does not
displace is unchanged.

Two stages can now produce byte-identical source: `depth_only` and
`shadow` differ in nothing for a material that neither displaces nor
discards. The variant key holds the stage as well as the source hash
(ADR 0022), so they stay separate cache entries and separate pipelines —
which they must, since they render to different targets. They will
diverge on their own once the shadow stage renders from a light's point of
view.

**Shadows themselves are not in this ADR yet.** `MaterialStage::SHADOW` is
a row in the stage table and compiles correctly; the shadow *pass* — a
depth texture array, per-light matrices in the frame group, and a filtered
lookup in `shading.wxsl` — is the consumer this partitioning was built
for, and is still owed. Until it lands, the two acceptance tests the
milestone is measured by cannot be written, and `plan.md`'s M5 section
records what remains.

If this changes, also update `wxsl_core::abi`'s stage table and context
field tables, `docs/architecture.md`'s "What a material declares" section,
and `crates/wxsl-stdlib/shaders/wxsl/vertex.wxsl`, which is the other half
of `VertexContext`.

## Amendment (ADR 0026)

The consumer this ADR was built for exists. `MaterialStage::SHADOW` is
asked for by a real pass, and the two acceptance tests it predicted —
a perforated material casting a perforated shadow, a displacing one
casting a displaced shadow — are written and pass on hardware.

One thing it recorded has stopped being true: `depth_only` and `shadow` no
longer produce the same picture, because a shadow pass binds a different
view. The generated *source* is still identical for a material that
neither displaces nor discards, and the variant key still holds the stage,
so nothing about the cache changed. See [0026](0026-shadows-a-view-per-light-and-two-flags-on-the-material.md).

## Amendment (ADR 0027)

A graph has more than three terminals. `output.varying` is one per
declared interpolant, each a vertex-stage root of its own compiled into
`wxsl_varying_<name>`, and `GraphOutputs::varyings` carries them.

The typing rule this ADR called "the single typing rule partitioning
needs" gained its mirror image: a vertex-only node may not feed a fragment
terminal, and a computed interpolant may not be read from a vertex
terminal. Both are `GraphError::WrongStage`. See [0027](0027-a-graph-computes-its-own-interpolants.md).
