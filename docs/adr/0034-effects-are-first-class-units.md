# 0034. Effects are first-class units

Date: 2026-09-13

Status: Accepted

Amends [ADR 0033](0033-pipelines-are-documents.md) (`pass.screen` names an
effect in a registry, not a hardcoded enum variant) and extends
[ADR 0009](0009-the-application-supplies-the-shader-library.md)'s rule
from shaders to passes: an application adds effects without touching
`wxsl-render`.

## Context

`PassKind::Screen` took `ScreenShader`, an enum with one variant —
`DeferredLighting` — reachable only by editing `wxsl-render`. plan2's P4
named it the pass-level twin of "a node is to a material": an effect
should be a self-describing unit (what it reads, its shader, its entry
points), listed in a palette, addable by an application, and composable —
with bloom as the proof, and with the effect *chain* through
`resource.color` becoming expressible rather than merely compiled
against (ADR 0033 documented that gap honestly: a chain failed in
`NotPresentable` with the message naming what was missing).

## Decision

* **An effect is data**: `wxsl_render::effect::Effect { id, label,
  description, inputs, vertex_entry, fragment_entry, shader }` — `Copy`,
  static, one reviewable row. `inputs` is the contract both sides compile
  against: each input names a `pass.screen` socket and a kind
  (`GBuffer` expands to one read per layout target plus depth; `Image`
  is one read), in pass-group binding order. The pipeline compiler
  validates the pass's wiring against the declaration; the shader
  declares its `@group(3)` bindings in the same order.
* **A registry, not a table**: `EffectRegistry::shipped()` holds the
  migrated lighting pass — the first effect, generated per lighting set
  exactly as before — and **bloom**, whose WXSL lives beside its
  descriptor under `shaders/bloom.wxsl` and is mounted when the variant
  compiles. *Amended by
  [ADR 0039](0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md): the
  shipped registry also holds `tonemap`, the display transform every stock
  chain ends in, and an effect shipping its own source no longer compiles
  under the materials' macro set.* `Renderer::add_effect` registers an application's own
  (re-registering an id replaces it). The compiler takes the registry as
  a parameter, so `compile_pipeline` is the one call an application uses
  with its own effects.
* **The pass names an id**: `PassKind::Screen { effect: String }`.
  `ScreenShader` is gone. The renderer resolves the id against its
  registry at frame-compile time and errors (`RenderError::
  UnknownEffect`) by name; the document compiler's `UnknownEffect` lists
  the ids that exist.
* **Chains are documents**: the deferred-lighting pass writing into a
  `resource.color`, a bloom `pass.screen` reading it, and bloom's `into`
  unconnected — that document now compiles, and the scheduler orders it
  from the reads. Two spellings of "what bloom reads" are equivalent: a
  `resource.color` wire, or the writing pass's own colour output (it
  stands for the same resource). The one un-compilable shape — sampling
  a pass whose output *is* the presented target — is the new
  `PipelineError::ImageFromPass`, and it says what to wire instead.

## Alternatives

* **Keep the enum, add variants** — the M6 shape: every effect edits
  `wxsl-render`, no palette, no chains. Rejected for the reasons P4
  exists.
* **Effects as screen-domain graphs** (plan.md M7's eventual shape) — a
  screen graph is a *third* graph domain and a milestone of its own. The
  descriptor is deliberately less: an effect is one fullscreen pass with
  declared inputs. When screen graphs land, an effect's `shader` grows a
  graph-backed variant; the document vocabulary does not move.
* **Effect parameters as uniforms** — deferred. Bloom's knobs
  (`THRESHOLD`, `STRENGTH`, …) are `const`s in its file: compile-time,
  like a macro, and a parameter block wants the buffer plumbing P11
  gives graph resources. Until then a "re-tune" is a new `Effect` row
  with a different mounted source, which `EffectRegistry::add` already
  supports by id.

## Consequences

* Adding a post effect is: one shader file, one `Effect` row,
  `add_effect` — no `wxsl-render` edit, no new pass kind, no new match
  arm. The lighting pass survives as generated text inside the same
  mechanism; its module, bindings and entries are byte-for-byte what
  they were, and the parity tests still hold.
* `compile` grew the registry parameter, and `StockPipeline::graph`
  passes the shipped one. An application with its own effects compiles
  its documents itself and hands the graph to `Renderer::set_graph`.
* Bloom is *one pass* — threshold, 13-tap kernel, add — honestly a cheap
  approximation, and its threshold reads the sRGB-encoded frame colour
  rather than pre-tonemap HDR. Moving tonemap out of `shade_surface`
  (plan.md M7) is what makes the physically-right threshold a two-line
  change in this file; the effect is written to expect it.
* The variant cache keys effects under `VariantKind::Effect` by (id,
  macros, lighting-set signature when generated); a background pipeline
  swap requests them like material stages, through the same
  `Request::Effect` channel `swap` always used.
* The demo gallery (`cargo run --example gallery -- --screenshot`)
  exercises the chain end to end: the deferred-bloom demos are the
  shipped preset's document with two nodes added and one rewired,
  compiled by the public `compile_pipeline`.
