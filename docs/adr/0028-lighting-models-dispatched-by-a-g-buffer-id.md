# 0028. Lighting models dispatched by a G-buffer id, with a composable G-buffer layout

Date: 2026-09-12

Status: Accepted

Amends [ADR 0008](0008-surface-graphs-and-a-named-shader-abi.md) (the shading
function is no longer fixed shipped text).

## Context

The ABI shaded every surface with one hand-written function,
`shade_surface` in `shading.wxsl`, whose inner light loop called
`pbr_direct` directly. That was the right shape while there was exactly one
answer to "what does lighting mean", and its sharing between the forward
and deferred paths is what made the two agree pixel for pixel. But three
wants have accumulated against it: stylized surfaces (Lambert, Phong) that
should not pay for GGX; a G-buffer that gains targets only when some
feature needs them rather than always; and — later — graph-authored
lighting models, which cannot land at all while the model call is
hand-written text.

The milestone this closes asked for a *registry* of lighting models, each
one WXSL function of a fixed signature plus a small integer id, with the
deferred G-buffer carrying that id and the lighting pass dispatching over
it; and for `GBUFFER_TARGETS` to stop being a fixed table and become base
targets plus targets the pipeline requests.

## Decision

A **lighting-model registry** lives in `wxsl_core::lighting`. An entry
carries an id, a name, the module and function that implement it, an
optional *extra G-buffer target* with the pack function that fills it, and
a document string. Every model function has one contract:

```text
fn <function>(surface: Surface, ctx: SurfaceContext, light: LightSample, extra: vec4f) -> vec3f
```

`extra` is the texel of the model's requested target, or zero. A model
that outgrows one `vec4f` is a schema change recorded in the registry, not
a per-model special case. The registry entry is the seam: the consumer
cannot tell a hand-written function from a generated one, so graph-authored
models later are a new *producer*, not a redesign.

A **`LightingSet`** is the models a pipeline enables — a set, never a
global, because the set decides the G-buffer's shape. The default set is
one model, the library's PBR, which generates no dispatch and no id
channel: the shape every pipeline had before this ADR. The shipped registry
adds `lambert`, `phong`, and `clearcoat`; clearcoat is the composition
proof, requesting a target only when it is enabled.

**The G-buffer layout is `gbuffer_layout(set)`**: the base targets, then a
scalar id channel if the set dispatches, then each requesting model's
target in id order. `wxsl-render` declares its resources from that one
function; the scheduler checks a `gbuffer`-stage pass's attachment count
against it (`RenderGraph::with_gbuffer_layout`); the generated struct,
pack and lighting-pass bindings read the same list, so the three cannot
drift. The set's cost against WebGPU's 32-bytes-per-sample attachment
budget is computed by the same arithmetic the spec uses
(`gbuffer_bytes_per_sample`) and checked by name in
`Renderer::set_lighting` — which is why the id channel is a *scalar*
target and clearcoat's request is a *pair*: every vec4 target costs 8
bytes whatever its depth, and four vec4 targets is the whole floor.

**The shading function and the lighting pass are generated**, by
`wxsl_core::lighting`, because the one thing that varies with the set is a
*call inside the light loop*, and no fixed shipped module can name models
it has not been told about:

* The forward module gets the whole `shade_surface` pasted in by codegen,
  dispatching *directly* to its material's model — a forward module
  contains only the model that material uses, whatever the set is.
* The deferred lighting pass is generated as a root per set: for one
  model, a direct call and no dispatch at all; for a set, a `switch` over
  the id the unpack read from the G-buffer. An unknown id shades black,
  which is preferable to shading with the wrong model.
* The macro knobs the loop honours (`wxsl_tonemap`,
  `wxsl_debug_normals`, `wxsl_receive_shadows`) are declared by the
  generated text exactly as `shading.wxsl` declared them, so the macro
  machinery — defaults, graph pins, overrides, the variant cache key —
  is unchanged.

A material names its model (`MaterialOptions::lighting`, the scene
document's `lighting` field) **by name, never by id** — ids belong to the
registry, and a document that stored them would silently reshade when one
was renumbered. Resolution happens against the set at compile time; a
name the set does not enable is an error there, and a frame whose
materials were compiled against a different set than the renderer runs is
a named error before anything is recorded.

## Alternatives considered

* **Keep `shade_surface` hand-written and switch models by macro flags.**
  A `@if` per known model in `shading.wxsl`. Loses twice: the library
  enumerates models it was never told about (a user model needs to edit
  the ABI), and the deferred pass needs a *runtime* switch over the
  G-buffer id anyway, so a second dispatch mechanism — and a second place
  to keep in sync with the first — appears regardless.
* **Dispatch at runtime in forward too** (write the id, switch in the
  material). Pays a G-buffer round trip for the one path that never needs
  it, and forward would have to attach pass-group inputs per material.
* **One fixed `lighting` target holding both the id and model data.**
  Couples every model to one target's spare channels and breaks the
  moment two models want data. Per-model requests cost one target each,
  and the budget check is what says when that is too many.
* **A global registry the renderer reads.** Two renderers in one process
  could not run different sets, and the set decides the G-buffer's shape —
  a per-pipeline fact, not an environment variable.

## Consequences

* `shading.wxsl`, `deferred.wxsl` and `lighting_pass.wxsl` are gone; the
  first two's contents are generated text, the last's is generated whole.
  The ABI constants (`SHADE_SURFACE_FN`, `GBUFFER_STRUCT`,
  `PACK_GBUFFER_FN`, `LIGHTING_PASS_MODULE`) survive as the *names* the
  generated text and the renderer agree on.
* `GBufferPrecision` gained `NormalizedScalar` and `HighDynamicRangePair`,
  because the attachment budget is paid per *target* and a target that
  needs one channel should not pay for four. The generated struct's field
  types follow the precision; the pack contract stays `vec4f` and
  narrower targets take its leading components.
* Forward and deferred still shade identically, but the guarantee moved
  from "same hand-written function" to "same generator feeding both" —
  the image-equality tests are what keep that honest.
* The lighting pass variant cache keys on the macro set *and* the set's
  signature; a set change recompiles the pass, not every material —
  though a material compiled against a different set must recompile too,
  and the renderer names that mismatch rather than drawing it wrong.
* Adding a model is: a `.wxsl` file under `shaders/lighting/models/`
  (excluded from node derivation on purpose), a registry entry in
  `wxsl_core::lighting::DEFAULT_MODELS`, and — only if it requests a
  target — a pack function in its module. A set overrunning the
  attachment budget is a named error from `Renderer::set_lighting`.
* If the G-buffer's precisions change, update
  `pipeline::gbuffer_bytes_per_sample`'s cost table with the spec's
  numbers, not guesses — the naive "4 bytes for Rgba8Unorm" is wrong, and
  was, here.
