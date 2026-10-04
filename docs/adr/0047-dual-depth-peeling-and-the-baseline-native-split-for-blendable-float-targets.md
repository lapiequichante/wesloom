# 0047. Dual depth peeling, and the baseline/native split for blendable float targets

Date: 2026-10-04

Status: Accepted

Implements plan3's M8. The `PeelFront`/`PeelBack` rows were the empty
chairs ADR 0022 left, waiting on a peel test.

## Context

Order-independent transparency has to composite surfaces the mesh
submission order does not know. Dual depth peeling does it by taking the
nearest and farthest remaining fragment each iteration, front-to-back on
one accumulator and back-to-front on the other, then compositing the two
over the opaque colour.

The classic depth pass stores that pair as `(-z, z)` — here `(1 - z, z)`,
so the clear can be zero — in one `rg32float` target updated by `MAX`
blending. `rg32float` is not blendable on the WebGPU baseline. That needs
`wgpu::Features::FLOAT32_BLENDABLE`, which is optional: requested when the
adapter has it, never required. A pipeline that wants the blend says so;
one that does not must still peel.

Which objects peel is not a property a compiler can see in the graph. A
pass already selects draws with a tag expression. Transparent objects are
the ones tagged `transparent`.

How many layers to peel is a loop bound. A loop bound is a macro, and a
macro that changes the pass list is read when the document compiles, not
as a uniform the shader branches on. The cap is 8 geometry passes: past
that the cost is a different pipeline, not a longer default. The macro
configures the layer count; the expansion is what keeps the pass count
inside the cap.

## Decision

A `pass.peel` document node expands into the peel, the way `pass.shadow`
expands into one pass per light. It samples the opaque pass's depth and
colour — the colour has to be a `resource.color`, because the frame
target cannot be sampled and written in the same chain — and it draws
`DrawSource::Scene` filtered to `transparent`, ignoring whatever tag the
scene source that feeds other passes happens to carry. Opaque, and
anything else, is never peeled.

The layer count is the int macro `wxsl_peel_layers`. Absent, it is 4.
Whatever is set is clamped to `1..=8`. The flag macro `wxsl_peel_native`
selects the path. Absent, the path is the baseline. Both live on
`PipelineConfig.macros`, because the pass list is a function of the
config and one document compiles either way.

The macro counts layers. The geometry-pass budget is eight, on both paths:

* **Baseline.** No float blending. Each layer is two geometry passes. The
  first keeps a depth — `Less` cleared to the far plane for a front
  layer, `Greater` cleared to the near plane for a back layer — and the
  fragment discards anything behind the opaque depth or outside the window
  the previous iteration left in an `rg32float` bounds target. The second
  pass depth-tests `Equal` against that buffer and blends just that
  fragment: under into the front accumulator, over into the back, both
  premultiplied. One pass that both tested depth and blended would
  composite every fragment that passed, in submission order. Two passes
  per layer and a budget of eight means the baseline peels at most four
  layers. A requested count above four is clamped here, not in the macro.
* **Native.** One pass writes `vec2(1 - z, z)` with `MAX` blending into
  `rg32float` (`FLOAT32_BLENDABLE`). The next pass shades every transparent
  fragment, writes the front accumulator where the depth matches the near
  value and the back accumulator where it matches the far value, and
  discards the rest. Two geometry passes peel two layers, so eight layers
  are eight passes. A leftover odd layer is the baseline's front pair
  (depth, then `Equal` shade), so it is not also taken as a back layer
  and it is not composited in submission order. An odd count still fits
  the budget: seven layers are six pair passes plus the two-pass tail.

Between iterations a fullscreen pass writes the new window, keeping the
previous bound where that iteration drew nothing (a cleared depth is not
a peeled surface). A last pass, `wxsl.peel_composite`, composites
`front + back * (1 - front.a)` over the opaque colour. Those fullscreen
passes are not geometry draws and are not what the cap of 8 counts. The
two bounds shaders and the composite are effect rows because that is how
a fullscreen pass is run; they are not palette nodes — `pass.peel` is the
thing an author places.

The new stage rows are `peel_front`, `peel_back`, `peel_depth` and
`peel_resolve`. The first two shade one layer. `peel_depth` writes the
pair and reads no surface. `peel_resolve` writes both accumulators. They
are the first geometry stages whose fragment declares `@group(3)`, in the
same order as the pass's reads, because the pass group is built from
those reads. A `pass.geometry` whose stage setting names one of them is a
document error: the peel draws the transparent tag, and a material pass
does not.

Weighted blended OIT is not a fallback for a missing feature. It is a
different document.

## Alternatives considered

* **One shader path that discards in a loop inside a single pass.** The
  layer count would be a uniform and the worst case would still shade
  every layer for every fragment, with no depth test to throw the losers
  away. The cost model the cap exists for would be invisible.
* **One blended pass per layer on the baseline.** The depth test would
  keep a single winning depth, but every fragment that passed on the way
  there would also blend. The picture would depend on the draw list. The
  second pass, `Equal` against the depth the first pass kept, is what
  makes the layer one fragment. The cost is the four-layer ceiling.
* **Classifying materials by inspecting the graph for alpha.** A graph
  with `alpha = 1` and a graph with a textured alpha are not a partition
  the compiler can maintain. The tag is the author's statement, and it is
  the same statement every other pass already selects on.

## Consequences

* `abi::MATERIAL_STAGES` grows four rows. Every material compiles them.
  `peel_depth` is a second colour-writing stage that reads no surface,
  beside velocity.
* `PipelineConfig` carries a `macros` set. Stock documents do not contain
  `pass.peel`, so an empty set changes nothing they compile.
* `WANTED_FEATURES` includes `FLOAT32_BLENDABLE`, still as an intersection
  with what the adapter offers. The native path is only built when the
  macro is set. A device without the feature still runs the baseline; the
  agreement half of `cargo test -p wxsl --test peeling` skips when the
  feature was not granted.
* Each peeled layer is drawn into its own target and folded by a fullscreen
  pass (`wxsl.peel_under`, `wxsl.peel_over`). Reloading one accumulator
  from two geometry passes does not schedule: a reader waits on every
  writer of that resource, so the earlier load waits on the later write.
* The accumulators are `rgba16float`. The window and the native pair are
  `rg32float`, sampled with `textureLoad` (no filter). The baseline depth
  buffers are `depth32float`, sampled as `texture_depth_2d` the pass after
  they are written.
* Per-target blend is an `Attachment` field. The resolve pass under-blends
  one target and over-blends the other; the pipeline cache key includes
  that blend, because the pass-wide `PassState` blend is one value.
* If this changes, also update the peel test's expected composite and the
  stage table comment in `abi.rs`.
