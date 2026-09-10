# 22. Material stages replace the render-path enum

Date: 2026-09-10

Status: Accepted, amended by [0025](0025-a-material-graph-spans-shader-stages.md)

Amends [0005](0005-render-pipeline-abstraction-and-shader-switching.md),
[0021](0021-a-declarative-render-graph-and-a-scene-document.md)

## Context

ADR 0005 established the idea that carries this project: one material
graph, several pipeline shapes, the differences expressed as conditional
translation inside a single WXSL module. The *implementation* of that idea
was a two-valued enum. `RenderPath::{Forward, Deferred}` bound one flag,
`abi::FEATURE_DEFERRED`, which selected one of exactly two `@if`-gated
fragment entry points that `codegen::write_entry_points` printed as a
format string.

There is no room in that shape for a third entry point, and the plan needs
at least six: a depth-only stage for a prepass, a shadow stage with a bias,
two peel stages for transparency, a velocity stage for TAA. It also could
not express the thing ADR 0021 had just made possible — a *single* pass
list containing two geometry passes that want different variants of the
same material, which is precisely what a depth prepass is.

The enum was also doing two jobs at once. "Which pass list am I running"
and "which shader variant does this pass draw with" were the same value,
and they come apart the moment a pass list has more than one geometry pass.

## Decision

**A stage is a row in a table, and a pipeline is a list of passes that each
name one.**

### `abi::MATERIAL_STAGES`

A table in `wxsl_core::abi`, like `GBUFFER_TARGETS`: each row gives a
stage's name, the fragment entry point to emit for it, and what that entry
returns (`StageOutput::{Color, GBuffer, Nothing}`). `MaterialStage` is a
newtype over the table index, so adding a stage is a row plus the constant
that names it — not a new arm in every match in the workspace.

Three rows ship: `forward_lit`, `gbuffer`, `depth_only`. The stages the
plan still owes — `Shadow`, `PeelFront`/`PeelBack`, `Velocity` — are not
stubbed out, because each needs something that does not exist yet (a depth
bias, the peel test, a previous-frame transform) and an empty row would be
a lie about readiness.

`StageOutput::color_targets()` is what the render graph validates a pass
against, so "this pass attaches one target and its stage writes three" is a
named error naming the pass, rather than a `wgpu` entry-point signature
complaint.

### One generated module per stage, and no flag

`CodegenOptions` gains a `stage`, and codegen emits *that stage's* entry
points — not every stage's, gated. `abi::FEATURE_DEFERRED` is deleted:
there is nothing left to gate. `Material` holds one `GeneratedShader` per
stage, generated up front, because codegen is string building; the
expensive half stays lazy in the variant cache.

Two consequences fall out of per-stage modules that gating would not have
given:

* A stage imports only what its entry point calls. The depth-only module
  imports neither the shading function nor the G-buffer.
* The variant cache's source hash already separates stages, before its
  `stage` field even looks.

The variant key gains the stage anyway, because a key that says what it
holds is worth more than a byte — and because it is the *stage*, never the
pipeline, that makes a swap cheap: two pipelines both wanting `gbuffer`
share the compiled result.

### `RenderPath` retires into `StockPipeline`

What is left of it is "which of the two pass lists this crate ships", which
is a different question from "which variant". `StockPipeline::{Forward,
Deferred}` lives in `wxsl_render::pipeline` and its only job is to build a
`RenderGraph`. `Renderer::set_path` becomes `set_pipeline`.

### The forward pipeline gains a depth prepass

Which is what proves the mechanism rather than merely describing it. The
forward pass list is now two geometry passes over one depth texture:
`depth_only` first — a pipeline with **no fragment state at all** — then
`forward_lit` testing `LessEqual` without writing depth.

`LessEqual` and not `Equal`: WGSL promises nothing about two pipelines
running the same vertex code producing bit-identical clip positions unless
the builtin is marked `@invariant`, and `Equal` turns a one-ulp difference
into a hole in the surface.

### Swapping pipeline without dropping a frame

`Renderer::request_pipeline` compiles the stages the new pass list needs
and the cache lacks on a worker thread, while the *previous* pipeline keeps
presenting; the swap lands in a single frame once they have all arrived.
`Renderer::swap_progress` is what a `compiling 3/7` indicator reads, and
the editor's `D` key is now exactly this.

Only the WXSL-to-WGSL half runs on the worker — it is a pure function over
text with no device in it. Turning each result into a `wgpu` module happens
on the render thread as it arrives, one poll at a time, so that cost is
spread over the frames the swap was taking anyway. On a single-threaded
target there is no worker and the swap blocks, which is what the indicator
is for.

`set_pipeline` stays, for the caller who would rather have the hitch than
the state machine.

## Alternatives considered

* **Keep one module, gate every stage's entry point with `@if`.** The
  faithful extension of ADR 0005. Rejected: it makes the flag set grow with
  the stage count (or forces integer comparison into `@if`), it compiles
  every stage's code whether or not the pipeline wants it, and it has
  nowhere to put M5's partitioning, where a stage needs a genuinely
  different *body*, not just a different signature.
* **A `MaterialStage` enum rather than a table index.** More idiomatic
  Rust, and it makes the compiler enumerate the arms. Rejected because the
  table has to exist anyway — codegen, the graph validator and the renderer
  all read the same three facts about a stage — and two declarations of the
  same list is exactly the drift ADR 0008 exists to prevent. The index
  newtype keeps `Copy`, `Hash` and exhaustive constants without the second
  list.
* **Emit only the part of the graph a stage needs, now.** The depth-only
  module would then be a vertex entry and nothing else. Rejected as M5's
  job: partitioning needs the vertex-output and discard nodes that do not
  exist yet, and doing a degenerate version now would make
  `every_node_in_the_library_compiles_for_every_stage` vacuous on the stage
  most likely to be wrong.
* **Compile the whole swap on the render thread, spread over frames.** No
  threads, no `Send` requirements, works on wasm unchanged. Rejected
  because the WXSL compile is the large half and a frame budget cannot be
  divided into it — a single node-heavy material would blow the frame
  whatever the schedule.
* **`Equal` depth compare in the shading pass.** The textbook prepass, and
  slightly cheaper. Rejected: it is only correct with `@invariant` on the
  vertex position, which this ABI does not yet emit. `LessEqual` costs
  nothing measurable and cannot produce the failure.

## Consequences

* **The forward pipeline compiles two stages, not one**, so a first frame
  costs one more compile and the demo reports four variants where it
  reported three. `switching_pipeline_reuses_cached_shaders` pins the new
  numbers.
* **A depth-only module still carries the whole material function**, so a
  macro change that only affects colour recompiles the prepass too:
  `a_macro_change_compiles_a_new_variant` sees four variants where two
  would do. That number is the measure of what M5's partitioning is worth,
  and the test says so.
* **`abi::FRAGMENT_ENTRY` and `abi::FEATURE_DEFERRED` are gone.** An entry
  point is `stage.fragment_entry()`, which is `None` for a stage that has
  none — and a `None` there is what makes `fragment: None` in the pipeline
  descriptor, which is what a depth prepass *is*.
* **`Material::shader` and `Material::wxsl` take a stage.** So does
  `ShaderVariants::material`. `Renderer::display_stage` is what a code
  panel should show — the stage that writes colour — because a forward
  pipeline now has two geometry stages and only one of them is interesting
  to read.
* **`wxsl-render` spawns a thread.** The first in the workspace. It is
  bounded (one per swap, ending when its requests are done), it holds only
  a cloned `ShaderLibrary`, and it touches no `wgpu` object.
* A swap whose compile fails leaves the previous pipeline running and
  reports the error, rather than swapping to something that will fail every
  frame.
* **Still owed to M5**: stage partitioning, and the `Shadow` stage that
  falls out of it. `depth_only` is the shape a shadow pass will take once
  it has a bias and an alpha subgraph to discard with.
* If the stage vocabulary changes, `docs/architecture.md`'s "How a frame is
  drawn" section, `docs/glossary.md`'s *material stage* entry and
  `AGENTS.md`'s ABI note are the three places outside the code that
  describe it.

## Amendment (ADR 0025)

This ADR left one thing open and one thing owed.

The open thing: *which part* of a graph a stage needs. It is now
answered. A stage compiles the partitions reachable from the terminals it
needs — `MaterialStage::needs_surface` is the whole rule — so the
depth-only module no longer carries the material function, and the cost
this ADR recorded is gone.

The owed thing: `MaterialStageDesc::fragment_entry` was an
`Option<&'static str>`, on the theory that whether a stage has a fragment
program is a property of the stage. It is not: a depth or shadow stage
needs one exactly when the *material* discards. So the table field is now
always a name, and `GeneratedShader::fragment_entry` is the `Option` —
per material, and what the pipeline is built from.

`MaterialStage::SHADOW` is a fourth row in the table, added the way this
ADR intended rows to be added: one row plus the constant that names it.
See [0025](0025-a-material-graph-spans-shader-stages.md).
