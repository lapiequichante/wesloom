# 0042. Effect parameters are uniforms, and the descriptor declares them

Date: 2026-09-27

Status: Accepted

Implements plan3's N4, the piece
[ADR 0034](0034-effects-are-first-class-units.md) deferred out loud: "effect
parameters as uniforms — deferred … until a parameter block wants the
buffer plumbing P11 gives graph resources." P11's buffers landed, and so
did the audience: bloom's `THRESHOLD`, `KNEE` and `STRENGTH` were `const`s
in its file, and a re-tune was a new descriptor row.

## Context

An effect's tunable knobs were compile-time constants in the shader file it
ships. That made a re-tune a *text* change — a new `Effect` row with a
different mounted source, registered over the same id — which is the exact
shape the material parameters milestone
([ADR 0023](0023-a-material-declares-its-resources.md)) retired for
materials: "a parameter is now the cheap knob and a `const` is the
expensive one." The pass-level twin wanted the same answer, and it wanted
it in the same places: the layout computed once, the shader struct
*derived*, the variant cache keyed on shape and never on value, the host
writing through the computed offsets. Anything else would have been a
second, slightly worse copy of a mechanism the repo already owns.

The other half of the decision is *where the values live*. A pass is not a
draw: it has no per-object channel, and the pass group is built inside the
graph, which knows neither effects nor buffers. But the pass *label* is
already the name an application addresses a pass by — the execution
policies' `mark_pass(label)` and `pass_run_count(label)` established that —
and a parameter is host state about a pass, which made the label the key.

## Decision

* **The descriptor declares the knobs**: `Effect.parameters` —
  `&'static [EffectParameter]`, one `{ name, default }` per knob, the
  default a `Value` that carries the type by implication. One reviewable
  row per effect, like `inputs` and `outputs`; every existing effect
  declares none, so nothing about them moves.
* **One layout, computed by the machine that already exists**:
  `Effect::param_layout` is `wxsl_core::resources::BufferLayout::uniform`
  over the declaration — ADR 0023's "layout computer has a second customer
  waiting", arriving. The offsets follow the same alignment-descending,
  name-ordered rule, and the host writes through the same
  `BufferLayout::write` a material's parameters use, so there is still
  exactly one thing that knows the offsets.
* **The shader's struct is generated, not stated**: the descriptor's
  shadow — a `struct <effect>_params { … }` and a
  `var<uniform> params:` at the pass group's next binding after the
  inputs and the outputs — is prepended to the module when the variant
  compiles. An effect's file *reads* `params.threshold` and declares no
  block of its own; the two-halves-plus-a-test contract that bindings and
  entry points live under would have been a hand-written mirror here, and
  ADR 0023's whole argument is that a hand-written mirror gets vec3
  alignment wrong silently. An effect with no parameters compiles
  byte-for-byte as it did.
* **The variant key folds the layout, never a value**:
  `ShaderVariants::effect_key` appends `param_layout().signature()` —
  names, types, offsets — which is the material parameters' rule again:
  two descriptors under one id that differ only in a default are one
  variant; a different layout is a different module, because the struct
  the shader reads is generated from it.
* **The binding is the pass group's, appended**: a `PassBinding::Uniform`
  joins the group after the reads and the writes — the next binding the
  effect's shader reaches, visible to vertex, fragment and compute alike,
  so a *compute* effect can take parameters without the frame group
  (which stays ADR 0035's "a real consumer away"). The buffer is not a
  pooled resource: its contents are host state that changes while the
  pass list does not, so the caller hands the actual buffer to
  `RenderGraph::record`, which binds it as the group's last entry.
* **Values live per pass, addressed by label**: the renderer keeps one
  uniform block per pass whose effect declares parameters, keyed the way
  `mark_pass` keys — the pass label. `Renderer::set_pass_param(label,
  name, value)` writes through the layout and uploads at the next frame;
  `pass_param` reads back through the same offsets, which is what a
  slider shows; `pass_parameter_layout` enumerates, which is what one
  will be built from. A block starts at the declared defaults, so a
  pipeline renders as authored before anything is set; a pass list
  replaced under the same label keeps its tuned values, and a changed
  *layout* is what starts a block over.
* **The proof**: bloom is the first effect with knobs, at the values its
  `const`s carried; the gallery's `bloom-tuned` demo is the bloom chain
  tuned live through `set_pass_param` (threshold 0.35, strength 1.6 —
  visibly not the default image, from the same document); the GPU test
  (`effect_params.rs`) moves the threshold eleven times and holds
  `cache_stats().misses` and `pipeline_count()` at zero movement.

## Alternatives considered

* **The shader file states its own params struct, test-pinned** — the
  descriptor-vs-shader contract style of `inputs`. Rejected: the contract
  tests pin *spellings* (a name, an order, an entry point), not byte
  offsets. The trap ADR 0023 exists for is a `vec3f` after an `f32`, and a
  mirror gets that wrong silently, every frame. Generated from the
  declaration, there is nothing to drift.
* **Parameters as settings on pass nodes** — the document pins values the
  way a `param.value` node names a uniform. Deferred, not rejected: the
  declaration is already data and the canvas (P5) will read it; what is
  deliberately not built yet is the document field carrying per-pass
  values, because "new document fields default to today's behaviour" and
  nothing needs one to tune a pass live. When documents want pinned
  initial values, it is a setting beside `policy` compiled into the
  block's starting bytes.
* **One buffer per effect id, not per pass** — smaller, and wrong: two
  bloom passes sharing an effect but wanting different thresholds is the
  ordinary case, the pass-level echo of two objects sharing a material.
* **A fifth bind group for parameters** — amends ADR 0010's four-group
  allocation for no benefit; the parameters are pass-frequency data and
  the pass group is where pass-frequency data is bound. Appending one
  entry keeps every existing binding index stable.
* **Compute effects wait for the frame group** — ADR 0035's stated
  condition for time and camera, unchanged; a parameters-only uniform in
  the pass group is *not* that, needs nothing from the frame group, and
  works today — so compute knobs (a ramp's ease, a bake's radius) are
  expressible without spending the frame-group question.

## Consequences

* Adding a tunable knob to an effect is one descriptor row and reading
  `params.<name>` in its shader. A re-tune is `set_pass_param` — a buffer
  write the next frame presents, with no variant and no pipeline moving.
* `RenderGraph::record` grew a parameter: the callback resolving a pass's
  parameter buffer. Everything that records a pass list by hand supplies
  `&|_| None` — the same one-liner `imports` and `run` always demanded.
* The pass bind group's cache key grew a third element (the pass label's
  hash) for exactly the passes that have parameters: two same-shaped
  passes with different buffers must not share a bind group, and the
  label — unique per pass — is what tells them apart.
* A parameter is the cheap knob and a `const` is the expensive one, now
  at the pass level too; the rule for which one an effect's new knob
  gets is the material rule: does the host tune it at runtime, or is it
  the effect's structure? Bloom's kernel taps stay `const` — the *shape*
  of the blur is structure; what the blur responds to is tuning.
* The gallery's bloom chain documents set their `scene` target to `hdr`
  precision, which they owed since
  [ADR 0039](0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md): an
  8-bit intermediate clamps linear radiance at 1.0, exactly the default
  threshold, and the highlight the demo is about could not exist. With
  the value honest, the default demo shows a halo and the tuned one
  shows the threshold doing its work.
* The editor has no slider to draw yet — the canvas that draws one is
  P5's, and the API it would drive (`pass_parameter_layout`,
  `set_pass_param`, `pass_param`) is the whole of what it needs. This
  item lands the mechanism; the face is queued with the canvas.
  *(Landed since: the pipeline canvas's inspector draws the sliders, and
  a move there still compiles nothing —
  [ADR 0043](0043-the-pipeline-canvas-is-a-mode-and-compiles-on-every-edit.md).)*
* If this changes, also update `wxsl-render/src/effect.rs`'s module doc
  (the contract it states), AGENTS.md's effect bullet, and the
  bind-group table in `docs/architecture.md`.
