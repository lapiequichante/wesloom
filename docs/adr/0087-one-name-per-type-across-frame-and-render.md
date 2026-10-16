# 87. One name per type: the device-free originals take their own names

Date: 2026-10-16

Status: Accepted

## Context

Plan6 C3 asked that each type have one canonical path before C4 freezes
the stable tier. `wxsl-render` adapts `wxsl-frame` rather than only
re-exporting it (ADR 0048), and three adapters kept their original's name:

- `TargetConfig`: `wxsl_render`'s holds `wgpu` types, `wxsl_frame`'s the
  device-free ones, and `PipelineConfig::target` is the latter;
- `RenderGraph`: `wxsl_render`'s is a newtype that records on `wgpu`
  around `wxsl_frame`'s;
- `GraphError`: the scheduler's (`wxsl_render::graph`, from `wxsl_frame`)
  and the graph model's (`wxsl_core::error`) are unrelated types.

`wxsl_render::pipeline_doc` also re-exported `wxsl_frame`'s by glob, and
its own `compile` shadowed the glob's.

## Decision

The `wgpu` adapters keep the stable names; the device-free originals, in
the unstable `wxsl-frame` (ADR 0078), are renamed:

- `wxsl_frame::pipeline::TargetConfig` → `TargetDesc`, like the crate's
  other device-free descriptions (`PassDesc`, `ResourceDesc`, `SlotDesc`);
- `wxsl_frame::graph::RenderGraph` → `FrameGraph`;
- `wxsl_frame::graph::GraphError` → `ScheduleError`, reached from
  `wxsl_render` as `graph::ScheduleError` and still carried by
  `RenderError::Graph`.

`wxsl_render::pipeline_doc` re-exports `PipelineError` and
`document_registry` by name, so its `compile` is the only one there.

## Alternatives considered

- **One type each, the device-free one.** `Renderer::new` and
  `RenderGraph::new` would take `wxsl_frame`'s formats, so every caller
  holding a `wgpu` surface format would convert it, and the orphan rule
  stops `wxsl-render` from adding that conversion to `wgpu`'s or
  `wxsl-frame`'s types. The wrapper also carries the recording methods.
- **Rename the adapters instead.** They are the stable names programs
  already use.

## Consequences

- `wxsl_render` users see no change except `graph::GraphError`, now
  `graph::ScheduleError`.
- `wxsl-frame` and `wxsl-ffi` users rename. The C ABI and the documents
  do not name these types and are unchanged.
- Older ADRs keep the names they were written with; `RenderGraph::schedule`
  in them is now `FrameGraph::schedule`.
