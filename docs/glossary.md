# Glossary

Terms used elsewhere in this repo's docs without re-explanation.

**WGSL** — WebGPU Shading Language: the shader language `wgpu`/WebGPU
consumes natively. No imports, no conditional compilation.

**WXSL** ("WGSL Extended") — a superset of WGSL adding imports/modules and
`@if`/`@elif`/`@else` conditional compilation, developed at
[wesl-lang.dev](https://wesl-lang.dev). Compiles down to plain WGSL via the
`wxsl-lang` crate / `wesl-cli` tool. See
[ADR 0003](adr/0003-wesl-as-the-shading-language.md).

**Node graph** — the user-facing/data-model representation of a shader as
connected nodes and sockets (`wxsl_core::graph`). Compiles to a WXSL
module. Distinct from a *render graph*, and the two are easy to confuse
because both are called "the graph" in conversation: a node graph is what a
material *is*, a render graph is how a *frame* is put together.

**Render graph** — the list of passes and the resources they read and write
that make up one frame (`wxsl_render::graph::RenderGraph`, built from
`wxsl_render::pass::PassDesc`). The engine orders the passes by their
dependencies, allocates and reuses the transient targets, rotates the
persistent ones, and records them. A *pipeline* is a render graph; forward
and deferred are two of them. See
[ADR 0021](adr/0021-a-declarative-render-graph-and-a-scene-document.md).

**Scene** — the document: meshes, materials and instances, as pure
serializable data (`wxsl_core::scene::Scene`). It says what exists, never
how it is drawn — the pipeline belongs to the renderer, not to the
document. Not to be confused with the *environment*.

**Environment** — camera, lights, ambient, exposure and time
(`wxsl_render::environment::Environment`). The other half of a frame,
alongside the draws. Called `Scene` until M1, which is why the rename
happened.

**Tags** — open-ended labels a material or an instance is authored with
(`opaque`, `transparent`, `outlined`). A geometry pass draws a *tag
expression* over the draw list, so the material says what it is and the
pass says what it draws.

**Render path** — which broad rendering strategy a pass uses: `Forward`
(shading happens in the same pass that determines visibility) or `Deferred`
(visibility/material data is written to a G-buffer first, then shaded in a
later pass). A property of the pass, not of a graph — see
[ADR 0005](adr/0005-render-pipeline-abstraction-and-shader-switching.md).

**Shader variant** — a specific compiled WGSL output for one
`(generated source, macro values, RenderPath)` combination, cached by
`wxsl_render::variants`. Switching render path or flipping a macro
variable asks for a different variant of the same material.

**Shader ABI** — the fixed WXSL vocabulary a generated material module is
written against: the `SurfaceContext` it is given, the `Surface` it returns,
the uniform bindings, the vertex stage, the lighting function and the
G-buffer layout. Named in `wxsl_core::abi`, implemented in
`wxsl-stdlib`'s `shaders/wxsl/`. See
[ADR 0008](adr/0008-surface-graphs-and-a-named-shader-abi.md).

**Surface** — what a material graph produces: base colour, metallic,
roughness, normal, emissive, occlusion and alpha at one point. A graph
describes a surface, not a whole shader — the entry points, light loop and
G-buffer packing are around it, not in it.

**Macro variable** — a graph-level knob that changes the *shape* of the
generated shader rather than a value flowing through it: a flag becomes a
WXSL `@if` condition, a number becomes a `const` declaration (so it can be a
loop bound or array size). Declared by node definitions, pinned in the node
format, part of the variant cache key. `wxsl_core::macros`.

**Node format** — the serialized form of a graph (`serde`, JSON in the
demo): nodes with stable ids and their pinned socket values, edges between
sockets, and the graph's macro variables. See
`crates/wxsl/assets/pbr_cube.wxsl.json`.

**LYGIA** — an existing, mature, multi-language (GLSL/HLSL/MSL/WGSL/CUDA)
granular shader function library by Patricio Gonzalez Vivo
([lygia.xyz](https://lygia.xyz)), licensed under the Prosperity Public
License 3.0.0 (not MIT/Apache). `wxsl` does not depend on or port it —
see [ADR 0007](adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md)
for why `wxsl-stdlib` is an original library instead.

**ADR** — Architecture Decision Record; see `docs/adr/README.md`.

**Facade crate** — `wxsl`, the crate most consumers depend on directly;
re-exports the other workspace crates behind Cargo features rather than
being depended on itself.
