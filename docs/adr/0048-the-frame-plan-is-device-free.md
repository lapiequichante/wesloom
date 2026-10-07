# 0048. The frame plan is device-free

Date: 2026-10-07

Status: Accepted

Amends [0002](0002-cargo-workspace-crate-boundaries.md),
[0021](0021-a-declarative-render-graph-and-a-scene-document.md) and
[0034](0034-effects-are-first-class-units.md). Implements plan4 B1.

Also amends the placement of config and presets in
[0030](0030-a-pipeline-config-instead-of-threaded-parameters.md) and
[0033](0033-pipelines-are-documents.md).

## Context

The planned Dawn backend needs the same effects, pipeline compiler,
scheduler and layouts as wgpu. These currently live in `wxsl-render`,
which makes using even their pure parts pull in a GPU implementation.

## Decision

Introduce `wxsl-frame` for frame descriptions and device-free planning.
Its only workspace dependency is `wxsl-core`; it must not depend on wgpu,
a renderer, the editor or the stdlib. `wxsl-render` consumes this crate
and owns GPU allocation, recording and the mapping to wgpu types.

`wxsl-frame` owns pass/resource descriptors, the scheduler, pipeline config
and document compiler, presets, effects, the capability contract, and
environment data/host layouts. The extraction landed in two compilable
steps; existing renderer module paths re-export or adapt the shared APIs.
Document spellings and scheduling behavior remain compatible.

The neutral vocabulary uses WebGPU format/state names and usage flags
(`bitflags`, with no device dependency). Conversion to native wgpu types
lives in `wxsl-render::types::WgpuType`, with exhaustive enum mappings.
The renderer wraps the shared `RenderGraph` to keep recording beside the
device while delegating planning to the shared implementation. Indirect
draws name a buffer `ResourceId`, replacing their embedded wgpu handle;
the scheduler accounts for that dependency and its indirect usage.

## Alternatives considered

- A wgpu-free feature on `wxsl-render`: weaker boundary, with device
  types still liable to leak into public planning APIs.
- A second planner in C++: duplicates the rules the backends must share.
- Moving everything at once: mixes the neutral-type migration with the
  module extraction and makes regressions harder to isolate.

## Consequences

- The frame crate can inspect effect interfaces and generate their
  parameter declarations without compiling a renderer.
- Effect sources live beside the declarations in `wxsl-frame/shaders`;
  they are shared by backends rather than copied into each implementation.
- CI checks the frame crate independently and rejects renderer/GUI
  dependencies in its normal dependency tree.
- Hand-built pass/resource state now uses `wxsl-frame::types`; the wgpu
  adapter is explicit. Native `wxsl-render::TargetConfig` constructors
  remain available and convert into the shared config. `Renderer::set_graph`
  and `request_graph` accept shared graphs as well as their wgpu wrapper.
- Camera/light values and the existing host layout mirrors move together
  with their tests; `glam` and `bytemuck` remain device-free dependencies.
  Replacing the fixed mirrors with generation is still plan4 B6.
- `cargo run -p wxsl-frame --example plan_frame` compiles and schedules
  both presets without a GPU or WXSL compiler. Preset-parity and capability
  tests run in this crate; the renderer's mapping tests cover native enum
  round trips and usage flags.
