# Glossary

Terms used elsewhere in this repo's docs without re-explanation.

**WGSL** — WebGPU Shading Language: the shader language `wgpu`/WebGPU
consumes natively. No imports, no conditional compilation.

**WESL** ("WGSL Extended") — a superset of WGSL adding imports/modules and
`@if`/`@elif`/`@else` conditional compilation, developed at
[wesl-lang.dev](https://wesl-lang.dev). Compiles down to plain WGSL via the
`wesl` crate / `wesl-cli` tool. See
[ADR 0003](adr/0003-wesl-as-the-shading-language.md).

**Node graph** — the user-facing/data-model representation of a shader as
connected nodes and sockets (`wesloom_core::graph`). Compiles to a WESL
module; distinct from a *render graph*, which this project does not
currently have a concept of.

**Render path** — which broad rendering strategy a `wesloom-render`
pipeline uses: `Forward` (shading happens in the same pass that determines
visibility) or `Deferred` (visibility/material data is written to a
G-buffer first, then shaded in a later pass). A property of the active
pipeline, not of a graph — see
[ADR 0005](adr/0005-render-pipeline-abstraction-and-shader-switching.md).

**Shader variant** — a specific compiled WGSL output for one
`(graph, RenderPath, feature set)` combination, cached by
`wesloom_render::variants`.

**LYGIA** — an existing, mature, multi-language (GLSL/HLSL/MSL/WGSL/CUDA)
granular shader function library by Patricio Gonzalez Vivo
([lygia.xyz](https://lygia.xyz)), licensed under the Prosperity Public
License 3.0.0 (not MIT/Apache). `wesloom` does not depend on or port it —
see [ADR 0007](adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md)
for why `wesloom-stdlib` is an original library instead.

**ADR** — Architecture Decision Record; see `docs/adr/README.md`.

**Facade crate** — `wesloom`, the crate most consumers depend on directly;
re-exports the other workspace crates behind Cargo features rather than
being depended on itself.
