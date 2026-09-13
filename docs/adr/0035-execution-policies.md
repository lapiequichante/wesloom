# 0035. Execution policies

Date: 2026-09-13

Status: Accepted

Implements plan2's P10. The proof case it names — the BRDF LUT, "a compute
effect writing a `Persistent` resource with policy `Once`, expressible in
every part except the *once*" — is the demo that lands with this ADR.

## Context

Every pass recorded every frame, so "runs once and produces a persistent
texture" was inexpressible: the BRDF LUT, a static backdrop, a bake an
operation triggers — all of them either re-ran for nothing or needed
bespoke bookkeeping outside the pass list. The scheduler was policy-blind
because the pass list had nothing to say. And the proof case had a second
honest gap: a *compute effect* did not exist — `PassKind::Compute` carried
an entry point and a workgroup count as raw strings and numbers, but the
renderer's record arm was a stub, and compute had no effect-descriptor
story at all.

## Decision

* **`Policy` on the pass**: `PerFrame` (the default, and every pass
  list's behaviour before this existed), `Once`, `OnResize`, `OnDemand`.
  `PassDesc.policy` carries it; the pipeline document's pass nodes
  expose it as the `policy` setting, so P3's vocabulary gains one row
  rather than a concept.
* **The stable-storage rule makes skipping safe**: a pass of any
  non-default policy may write only `Persistent { history: 0 }`
  resources — the scheduler rejects anything else
  (`GraphError::PolicyNeedsStableStorage`, naming the frame's target
  separately, since that is the likeliest way to arrive). With the rule
  in force, a skipped pass leaves behind exactly what its last run
  wrote: *due* is a per-pass question with no propagation, and the
  renderer's frame loop answers it from `Policy::due(ran, size_changed,
  demanded)` — pure, device-free, tested without a GPU. A reallocation
  (the pool's generation moving) re-runs `Once` passes, because their
  textures died with it; `OnResize` really means "re-run when my
  inputs' shapes change", of which the target's size is what a pass
  list can see.
* **The renderer keeps the run bookkeeping**: cumulative per-pass run
  counts (`Renderer::pass_run_count`, by label — the name a document
  author knows), and `Renderer::mark_pass(label)` for the `OnDemand`
  half. `RenderGraph::record` takes the resulting `run` predicate; a
  pass it rejects is not recorded at all — no encoder opened, no bind
  group built.
* **The document compiler promotes**: a policy'd pass writing into a
  chain's `resource.color` gets that target promoted to stable storage
  automatically — a derivation, like depth clear-vs-load, not a new
  knob. Documents carry less than pass lists on purpose.
* **Compute is an effect kind**: `EffectKind::{Screen, Compute}`,
  replacing `PassKind::Compute`'s raw `entry`/`workgroups` (and the
  never-used `Dispatch` enum) with `PassKind::Compute { effect: String }
  ` — the same id-resolution a screen pass does. The effect declares its
  non-attachment writes (`Effect::outputs`, bound after the inputs in
  the pass group, write-only storage textures); its workgroup count is
  the effect's own business, because it knows its shader's
  `@workgroup_size`. The renderer builds one `wgpu::ComputePipeline` per
  (variant, pass shape), laid out with the pass group alone.
* **The proof ships as descriptors, not registry rows**: `BRDF_LUT`
  (compute, `Once`, a 64×64 split-sum LUT baked by a pure function of
  its coordinates — which is *why* `Once` is the honest policy) and
  `LUT_VIEW` (screen, displays it) live in `shaders/` beside bloom's,
  registered by applications and tests with `add_effect`. No stock
  pipeline reads the LUT yet; the IBL that would is M7's.

## Alternatives

* **Skip as "record a no-op"** — open each pass and issue nothing.
  Rejected: it pays the encoder cost the policy exists to save, and it
  blurs the meaning of a recorded frame.
* **Policies on the document only, engine blind** — the compiler could
  emit a static pass list with a side table. Rejected: the policy is
  pass data like its state or its views; a second list that must agree
  with the first is the drift P3 was ending.
* **General invalidation graphs** ("re-run when my inputs' *contents*
  change") — content-driven dirtying is a different, larger system. P10
  is deliberately the shape part: shapes and marks, both observable.
* **Compute stays raw strings** — `entry`/`workgroups` on the pass. Two
  ways to compute would drift exactly as `ScreenShader` did before ADR
  0034; the effect kind is the same lesson applied once.

## Consequences

* The BRDF LUT is expressible end to end and observable: a GPU test
  renders three frames and asserts the bake ran once, a resize and it
  ran twice, `OnDemand` sleeps until marked and stops after. The run
  counts are the *only* new API a test needs.
* The `once`-policy document shape — bake into `resource.color`, present
  it per frame — compiles and schedules with no `history` setting,
  because the compiler promoted the target.
* Storage-texture write bindings joined the pass group (reads, then
  writes), which is the piece P11's buffers reuse; the effect contract
  (inputs then outputs, in binding order) now covers compute as well as
  screen.
* `Dispatch` (direct and indirect workgroup counts) is gone. Indirect
  dispatch waits for a consumer — a GPU-driven particle system — and is
  an effect-descriptor field when it lands.
* Compute pipelines see only the pass group. A compute effect that wants
  the frame group (camera, time) is a real consumer away from getting
  it.
