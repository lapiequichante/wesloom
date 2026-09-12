# 0033. Pipelines are documents

Date: 2026-09-12

Status: Accepted

Amends [ADR 0021](0021-a-declarative-render-graph-and-a-scene-document.md)
(the pass list is data; this decides who authors it) and
[ADR 0022](0022-material-stages-replace-the-render-path-enum.md)
(`StockPipeline` keeps its name; what it stands for is now a file).

## Context

The render graph made a pipeline *schedulable as data*, but the two
shipped ones were still authored as hand-written Rust:
`forward_graph` and `deferred_graph` built their pass lists in functions.
`PassKind::Screen` took a hardcoded enum naming the one fullscreen shader
that existed. plan2's review called this what it was — "everything looks
hard-coded" — and [request.md](../../../request.md) asks for the pipeline
to be a graph the way the material already is: sources, passes,
resources and a present step, composed as data, with the forward and
deferred pipelines as two *instances* rather than two functions.

[plan2-architecture.md](../../../plan2-architecture.md) sketched the
layers — document in `wxsl-core`, compiler in `wxsl-render`, presets as
shipped files, canvas in the editor — with the instruction that nothing
in it is decided until it lands.

## Decision

A pipeline is a `wxsl_core::graph::Graph` — the same type the material
canvas edits — over a different node registry, `wxsl_core::pipeline`:

* **The vocabulary is nine document nodes**, each a
  `NodeBody::Document` definition: `source.scene` (a draw list filtered
  by a tag expression), `source.lights` (the shadow-map array),
  `resource.gbuffer` (the enabled set's G-buffer, depth included),
  `resource.color` / `resource.depth` (a target, with precision, scale
  and history), `pass.geometry` (a material stage over a draw list),
  `pass.shadow` (which expands to one pass per light slot), `pass.screen`
  (a fullscreen effect, named by id), and `present` (the terminal; one
  per document). Edges carry render-graph resources — `DrawQueue`,
  `ShadowMaps`, `GBuffer`, `ColorTarget`, `DepthTarget` — new
  `ValueType`s outside `ALL` and `RESOURCES`, the same handle trick the
  texture sockets use, one domain over.
* **The compiler is one pure function**, `wxsl_render::pipeline_doc::
  compile(document, registry, config) -> Result<RenderGraph, PipelineError>`.
  No device. It derives what documents deliberately leave unsaid: depth
  from a resource is *cleared*, depth from another pass's output is
  *tested against without being written* (`LessEqual`, no depth write) —
  the prepass idiom as a wire; the pass writing the frame's target clears
  to the config's clear colour, intermediates to transparent; a stage's
  wiring *is* its attachment list. Every error names the document node by
  its display label; the scheduler stays untouched as the last line.
* **The stock pipelines are preset files**:
  `crates/wxsl-render/assets/presets/{forward,deferred}.pipeline.json`,
  embedded at compile time. `StockPipeline` keeps its name and its
  `graph(&config)` signature; what that method does now is parse the file
  and compile it. The hand-built `forward_graph` / `deferred_graph` stay
  as *reference implementations*, and the parity tests assert
  document-compiled ≡ hand-built field for field — the msdf rule, two
  implementations must agree — plus a round-trip test asserting each
  shipped file parses back to the document the builder builds.
* **`GBufferPrecision` becomes the document's precision vocabulary**
  (`standard`/`hdr`/`scalar`/`pair`); the document never names a
  `wgpu` format, and `gbuffer_format` remains the mirror. Screen effects
  are likewise names, in `pipeline_doc::EFFECTS` — one row for now, the
  deferred lighting pass migrated out of the `ScreenShader` enum it was
  reachable only through. P4 generalizes that table into an
  application-supplied registry.
* **Knobs that are the config's stay the config's.** The lighting set
  decides the G-buffer's shape (`resource.gbuffer` expands to whatever
  the enabled set requests); the shadow array is `MAX_LIGHTS` layers at
  `SHADOW_MAP_RESOLUTION` because the frame group binds it by that shape.
  A document wanting a different frame group is asking for a different
  ABI, not a different graph.

Two deliberate deviations from the plan2-architecture sketch, both
recorded because they were considered:

* **The presets live in `wxsl-render`, not the facade.** The sketch put
  them in `crates/wxsl/assets/`, but `StockPipeline::graph` must work
  from `wxsl-render`, and reaching into a sibling crate's files at
  compile time would be a dependency edge by another name. The files
  ship beside the compiler that validates them; the facade re-exports
  the type as before.
* **`PipelineConfig` stays in `wxsl-render`** (ADR 0030), rather than
  moving to `wxsl-core::pipeline` as the sketch proposed: its fields
  name `wgpu` concepts (`TargetConfig`'s format and clear colour), and
  the document vocabulary is the part a canvas or a file needs. The
  split is "shape is core, knobs are render-side" — the scene's split,
  again.

## Alternatives considered

* **A sibling document model** (a dedicated `PipelineDoc` struct beside
  `Graph`). Rejected: forking the data model forks the canvas, the
  serialization, the hit-testing and the undo story three steps later —
  the repo's own guard rail argues the other way, and the spike of
  expressing the deferred preset as a `Graph` went smoothly. The one
  mechanism a pipeline document needed that materials lacked — optional
  inputs on non-terminal nodes ("unconnected `into` means the frame's
  target") — was generalized into `NodeBody::Document` rather than
  special-cased.
* **Deriving attachments fully from the scheduler.** Rejected: the
  scheduler checks a *built* pass list; the mistakes a canvas user makes
  (no G-buffer wired into a `gbuffer` pass, two passes writing the
  target, presenting an intermediate) are about nodes the scheduler
  never saw, and naming those is the compiler's job. Both stay.
* **Compiling only the shipped vocabulary, no document API.** Rejected
  as not enough: the value is applications building pipelines without
  forking the renderer, which `compile` + `Renderer::set_graph` already
  allows.

## Consequences

* `StockPipeline::graph` can now fail in principle (a broken embedded
  preset) and panics; the parity and round-trip tests exist so that a
  mistake reaches a test, not that `expect`.
* With one shipped effect, an effect *chain* is not yet expressible —
  `resource.color` and `pass.screen`'s `into` compile against it, but the
  only effect writes the frame's target, so a chain today fails in
  `NotPresentable` naming the fix. P4's effect registry is what makes
  chains live; nothing here needs reworking for it.
* A material pass renders from the camera and a `pass.shadow` from its
  light — `source.camera` from the sketch is omitted until a second
  camera exists; adding it is one node when the frame group grows one.
* Documents never carry `Arc<wgpu::Buffer>`, so indirect draws remain a
  hand-built-graph feature until a document node can name a buffer (P11).
* The editor's pipeline canvas (P5) reads this vocabulary, the
  `PipelineError`s land in the problem panel, and plan.md M8 becomes "a
  preset document plus its stages".
* If this changes, also update `plan2.md`'s status, `docs/architecture.md`'s
  pipeline section, and the preset-parity tests in `pipeline_doc`.
