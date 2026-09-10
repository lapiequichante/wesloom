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

**Parameter** — a value a material's graph declares (`param.value`) that
lives in a uniform buffer rather than in the shader text, so the host
changes it with a buffer write and no recompile. The counterpart to a
*constant*, which is inlined and therefore costs a new shader variant when
it changes. See
[ADR 0023](adr/0023-a-material-declares-its-resources.md).

**Interface** — what a material needs bound before it can draw: its
parameters and their computed offsets, its textures and samplers, and the
block it expects the application to supply
(`wxsl_core::resources::MaterialInterface`). Computed from the graph, and
the single source both the generated WGSL and the bind groups come from.

**Setting** — a string-valued property of a *node instance* that changes
what it compiles to, such as the name of the parameter a `param.value`
declares (`wxsl_core::node::SettingDef`). Distinct from a node's label and
colour, which are metadata a reader chooses and codegen never sees
(ADR 0019).

**Tags** — open-ended labels a material or an instance is authored with
(`opaque`, `transparent`, `outlined`). A geometry pass draws a *tag
expression* over the draw list, so the material says what it is and the
pass says what it draws.

**Material stage** — which entry point a material graph is compiled for,
and therefore what its fragment stage writes: `forward_lit` (a final
colour), `gbuffer` (the G-buffer struct) or `depth_only` (nothing at all —
no fragment stage). A row in `wxsl_core::abi::MATERIAL_STAGES`, named by a
pass, never by a graph. This replaced the two-valued `RenderPath`, which
had room for exactly two entry points — see
[ADR 0022](adr/0022-material-stages-replace-the-render-path-enum.md).

**Pipeline** — a render graph. `StockPipeline::{Forward, Deferred}` are the
two this crate ships: forward is a depth prepass plus a shading pass,
deferred is a G-buffer pass plus a fullscreen lighting pass. "Which
pipeline" and "which stage" are separate questions, which is why they are
separate types.

**Shader variant** — a specific compiled WGSL output for one
`(generated source, macro values, stage)` combination, cached by
`wxsl_render::variants`. Switching pipeline or flipping a macro variable
asks for a different variant of the same material. The key holds the
*stage* rather than the pipeline, which is what makes the second swap
between two pipelines free.

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
