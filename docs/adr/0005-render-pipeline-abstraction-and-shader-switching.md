# 0005. Render pipeline abstraction with automatic forward/deferred shader switching

Date: 2026-09-08

Status: Accepted, amended by [0021](0021-a-declarative-render-graph-and-a-scene-document.md), [0022](0022-material-stages-replace-the-render-path-enum.md)

## Context

The renderer must support both forward and deferred rendering, and switching
between them must not require the graph author to author two different
graphs, or the application to hand-manage two sets of shader source. A
material graph that reads "surface color, normal, roughness, metallic" is
the same *material* regardless of which pipeline consumes it — what differs
is what the generated shader's entry points and outputs look like (a
forward fragment shader writes final lit color directly; a deferred
fragment shader writes a G-buffer, and lighting happens in a later pass
that forward rendering doesn't have at all).

Recompiling a graph to WGSL from scratch on every pipeline switch would
work but throws away cacheable work, and — more importantly — leaves no
single place where "which shader source corresponds to this graph, on this
pipeline, with these features enabled" is decided consistently.

## Decision

- `wxsl-render::path` defines a `RenderPath` enum (initially `Forward`
  and `Deferred`). A `RenderPath` is a property of the active pipeline, set
  by the application, not stored on a graph.
- A compiled graph is asked to produce WXSL for a *given* `RenderPath` (and
  the currently active feature set); `wxsl-core::codegen` expresses the
  per-path differences using WXSL's `@if`/`@elif`/`@else` conditional
  compilation (ADR 0003) inside one module, rather than maintaining
  hand-diverged WXSL per path. Concretely, a material graph compiles to one
  WXSL module with conditionally-compiled entry points/outputs; which
  branch is active is selected by the condition passed to the WXSL
  compiler for the pipeline currently being built, not by picking between
  separate files.
- `wxsl-render::variants` owns a cache keyed by
  `(graph content hash, RenderPath, active feature set)` mapping to
  already-compiled WGSL / `wgpu` shader modules. Switching the active
  `RenderPath` at runtime looks up or lazily compiles the variant for the
  new path; it does not require the caller to know that recompilation may
  be happening.
- `wxsl-render::pipeline` defines the trait forward and deferred
  pipelines both implement, so an application can hold "the current
  pipeline" as one trait object / enum and swap it without touching call
  sites that just want "render this scene with the current pipeline."

## Alternatives considered

- **Two independent graph compilers, one per render path.** Rejected: this
  is precisely the "author the same material twice" outcome the project is
  meant to avoid, and it invites the two paths' shading models to drift
  apart silently.
- **Recompile from the graph's own representation on every switch, no
  variant cache.** Rejected as a starting design because it makes runtime
  pipeline switching a potential stutter (shader compilation is not free)
  with no structural place to fix that later; the cache costs little to
  design in now and can start as trivially small (e.g. one entry) without
  losing the key shape if it needs to grow.

## Consequences

- Every node's WXSL implementation needs to either be genuinely
  path-agnostic (most math/color utility nodes, including the base node
  library, ADR 0007) or explicitly branch on `RenderPath` via WXSL
  conditional compilation where the two paths' outputs actually differ (surface/output
  nodes near the end of a material graph).
- The variant cache's key must include the active feature set, not just the
  graph and path — a graph compiled with one optional feature enabled is
  not interchangeable with the same graph compiled without it.
- Adding a third render path later (e.g. a visibility-buffer path) means
  extending `RenderPath` and auditing which nodes' conditional branches
  need a new arm — a search for `RenderPath` usages, not a parallel new
  compiler.

## Amendment (ADR 0021)

[ADR 0021](0021-a-declarative-render-graph-and-a-scene-document.md) keeps
this decision's core — one graph, many pipeline shapes, the difference
expressed as conditional translation — and moves where the choice is
recorded. A `RenderPath` is no longer a property of the *renderer* that both
pipeline structs read; it is a field of a `PassDesc`, so one frame can
contain passes wanting different variants of the same material. The
`Pipeline` trait, `ForwardPipeline` and `DeferredPipeline` are gone: a
pipeline is a list of passes, and the two shipped ones are built by
`pipeline::forward_graph` and `pipeline::deferred_graph`.

`Renderer::set_path` and the variant cache behave exactly as described
above, and the demo's two paths still agree to 0.0001.

## Amendment (ADR 0022)

The *idea* here — one graph, several pipeline shapes, the differences
expressed as conditional translation rather than as a second graph — is
what generalizes, and it survives unchanged. The *implementation* did not:
`RenderPath` was a two-valued enum selecting one of two `@if`-gated
fragment entry points, and there was no room in that shape for a third.

[ADR 0022](0022-material-stages-replace-the-render-path-enum.md) replaces
it with `abi::MATERIAL_STAGES`, a table. A graph compiles once per *stage*,
each stage emitting its own module with its own entry point;
`abi::FEATURE_DEFERRED` is gone because there is nothing left to gate.
What is left of `RenderPath` is `StockPipeline`, which names one of the two
pass lists this crate ships and has nothing to do with shader variants.
