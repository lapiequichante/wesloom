# Plan 3: the screen domain, the canvas, and the contracts

Written when plan2's last core-data-model proposal (P12) landed. It is the
queue for everything still open across the three planning docs: plan.md's
milestones M7–M11, plan2's proposals P5, P7 and P8, and the deferred work
that ADRs 0034–0037 each named honestly enough to be queued here by name.
It follows plan2's discipline — every item lands with an ADR and a
runnable demo, or it did not land. Two items joined after the plan was
written: N9, the editor catching up with everything the last stretch
shipped, and N10 — the WebGL2 decision, which reopens one of plan.md's
accepted costs on purpose.

Status: nothing in this file has landed. The shipped state it starts from
is plan.md's M0–M6, plan2's P1–P4 and P9–P12 (ADRs 0029–0037), and the
editor through its MSDF/UI layer (ADR 0013/0014). Each numbered item
becomes an ADR when it lands and moves its reasoning there, exactly as the
earlier plans' items did.

## Where the three plans stand

| From | Still open | Reshaped by |
|---|---|---|
| plan.md | M7 (screen domain), M8 (peeling), M9 (bake), M10 (relative-to-eye), M11 (code editor) | plan2's P3/P4 — passes and effects are data now |
| plan2 | P5 (pipeline canvas), P7 (material config), P8 (identity/contracts) | P10–P12 — the vocabulary they were waiting for exists |
| ADRs 0034–0037 | the deferred halves each ADR named | queued below as N-items with the ADR that owes them |

## What changed shape since the plans were written

* **M7 lost its plumbing half.** The effect seam, the chains, the storage
  writes and the compute dispatch all exist (ADRs 0034/0035); bloom ships
  as one descriptor row. What remains of M7 is the *graph-authored* half —
  the `Screen` graph domain that makes a post effect a node graph instead
  of a fixed `.wxsl` file — plus the two moves the shipped effects are
  explicitly waiting for: tonemap leaving `shade_surface` (bloom's
  sRGB-space threshold is documented dishonesty until then) and IBL
  ambient, which is the reader the Once-baked BRDF LUT has been sitting
  in `shaders/brdf_lut.wxsl` for. The motion half (Velocity stage,
  previous-frame transforms, TAA, motion blur) is independent enough to
  be its own item.
* **M8 became what plan2 predicted**: "another preset document plus two
  stages". The interesting residue is the portability rule's first real
  bite — dual depth peeling wants float blending, which is the one place
  the WebGPU baseline and native paths genuinely diverge, and the msdf
  rule (two implementations agree, by test) applies.
* **M9's mechanism pre-exists twice over.** A bake is an effect over a
  material subgraph (plan2's words), and its invalidation rule is the
  Policy machinery — a bake is `once` or `on demand`, not a new
  invalidation system. What is actually new: generating an effect's
  shader *from a material subgraph*, and the "baked" node that is
  transparently either the subgraph or a sample of its bake.
* **P7's trigger arrived.** The plan said to consolidate per-material
  configuration "when the second knob arrives rather than speculatively".
  Features (ADR 0037) was the fourth and fifth knob: `MaterialOptions`
  now carries macros, shadow flags, a model name and feature channels,
  and `CodegenOptions.lighting` carries a `MaterialLighting` with a plan
  inside it. The consolidation is overdue, not speculative.
* **P8's first instance already shipped.** The feature-channel handshake
  check (a material pinning a feature its pipeline does not carry is a
  named error) is the capability check P8 describes, for the one case
  whose facts were computed. P8 generalizes it: namespaced ids, document
  schema versions, and one `RenderSetup::check` over published metadata.
* **The web question was answered.** plan.md accepted
  `DownlevelFlags::VERTEX_STORAGE` on the strength of "WebGL is not a
  target". The decision reversed: WebGL2 is a target. plan.md's cost
  note is reopened by N10 below, and the portability rule gains a third
  column rather than a rewrite — what the engine cannot lower to
  ES 3.0 is a short, named list, each with a fallback decided in the
  item rather than discovered in a browser console.
* **The document vocabulary has known holes, not mysteries.** A compute
  pass has no document node (ADR 0036 records why); an effect cannot
  declare a second image input (`pass.screen`'s socket set is fixed);
  effect knobs are `const`s rather than uniforms (ADR 0034 deferred them
  pending P11's buffers — which have since landed). Each is an N-item
  below.

## Proposals

Ordered by (payoff ÷ cost). The M- and P-numbers are the ones the earlier
plans and the ADRs use; the N-series is new work those ADRs queued — N9
and N10 at the end are newer still: the editor's catch-up, and a decision
made after this file was written.

### P7 — One `MaterialConfig`

`CodegenOptions.lighting`, `MaterialOptions` (macros, shadow flags, model,
features) and the scene document's `lighting` field are several spellings
of one fact plus resolution steps in two places. One `MaterialConfig`
(macros, model, shadow flags, tags, feature channels) carried from
document → options → codegen, resolved in one place, keeps each new knob
at one site — the same argument P2 made for pipelines, now owed to
materials.

* Cost: small — a refactor of signatures that exist, with the handshake
  checks moving into the one resolution point.
* Payoff: the next per-material knob is a field, not another
  options-struct; the P8 capability metadata has one place to read.
* ADR: "A material's configuration is one value, resolved once."

### N1 — The shipped effects complete their halves

The two moves ADR 0034/0035 wrote down as future work, and the smallest
visible win on this list:

* **Tonemap leaves `shade_surface`.** The filmic curve becomes one screen
  effect at the end of a chain instead of a macro inside the shading
  function. Bloom's threshold then reads pre-tonemap HDR colour — the
  physically correct place — and the `const` edit its shader has been
  waiting for lands. The forward and deferred paths emit *linear* and
  present through the same chain, which is the cleanup the ABI has wanted
  since the sRGB-encoding decision.
* **IBL ambient consumes the BRDF LUT.** `ambient_environment` grows a
  specular-ambient term that samples the split-sum LUT (baked `once` by
  the effect that has been sitting beside bloom since ADR 0035) against
  each surface's roughness and N·V. The demo gains a sky; the LUT gets
  its reader; the execution-policy proof stops being a proof and becomes
  infrastructure.

* Cost: small-medium. The tonemap move touches the generated shading
  function's template and every test that asserts on it; the IBL half is
  one function plus a sky approximation, wired through the frame group.
* Done when: disabling the tonemap effect changes the image only by
  curve; a metallic sphere shows sky reflection; the gallery's bloom
  threshold behaves on HDR values.
* ADR: "Tonemap is an effect, and ambient reads the LUT."

### M7 — The screen domain

The data end-game ADR 0034 named: an effect authored as a *graph* is pure
data end to end.

* `GraphDomain` on the graph; a screen ABI (`SurfaceContext`-shaped: uv,
  the bound inputs, frame time) beside the surface ABI; the node registry
  filters definitions by domain — the mechanism `context_read` vs
  `vertex_context_read` already established.
* `EffectShader` grows a graph-backed variant beside `Lighting` and
  `Source`; the pipeline compiler treats a graph effect exactly as it
  treats a file effect today, because the descriptor is the seam.
* The built-in effects that fit become screen graphs (bloom's threshold
  and composite; FXAA arrives here as the first new one); the ones that
  do not (multi-pass chains with internal transients — ADR 0034 noted the
  compiler would synthesize sub-documents) stay descriptors until that
  scoped-name-allocation question is forced.
* The editor authors post chains as nodes — but that is P5's line below;
  this item lands the domain, the ABI, and one graph-authored effect.

* Cost: the largest single item here — a new ABI and a third graph
  domain — but the effects work removed everything downstream of it.
* Done when: an effect in the shipped registry is a graph a user could
  have authored, and FXAA is in the palette.
* ADR: "Screen-domain graphs: postprocess is a material over the frame"
  (plan.md's own title, still the right one).

### N2 — Motion: the Velocity stage, and TAA

The last slice of plan.md's M7, split out because it touches geometry and
instance rows rather than the screen ABI:

* The `Velocity` stage row (plan.md's table has carried its empty chair
  since M2): screen motion per fragment, which needs the transform at
  `t-1` in the instance rows — M5 settled the graph half
  (`wxsl_previous_frame`), the row is what is missing.
* TAA on top: a persistent history ring (exists), a velocity-aware
  reprojection, and the disocclusion question answered honestly. TAA pays
  twice — stable shadows and (later) SSR noise.
* Motion blur as the first consumer that is not TAA.

* Cost: medium. The instance row grows a second matrix (the ABI's stride
  rule from ADR 0024 applies — a new array beside, never a widening);
  the stage is one row plus the pass that fills the velocity target.
* Done when: the gallery's spinning cube under TAA shows no edge
  shimmer, and a moving cube blurs.
* ADR: "Velocity is a stage; TAA is a policy'd chain."

### N3 — Compute and buffers in documents

ADR 0036's deferred half, now that the engine side exists:

* A `pass.compute` node: the vocabulary's socket set is the design
  question — an effect's inputs and outputs must map onto fixed sockets,
  which breaks down exactly when an effect wants two images. The honest
  fix is letting a *compute* node grow sockets from its effect's
  declaration the way `pass.screen` maps its fixed three; the
  canvas/palette machinery needs to show them.
* A `resource.buffer` node beside it (size, and persistence when a
  consumer asks).
* Indirect dispatch (`Dispatch` died in ADR 0035) waits for its consumer
  — a GPU-driven particle or culling demo — and lands as an effect
  descriptor field.

* Cost: medium — the socket-mapping mechanism is the real work; the two
  node rows are table entries once it exists.
* Done when: the buffer-ramp demo compiles *from a document* instead of
  a hand-built graph, and the ADR 0036 note about `pass.compute` is
  deleted.
* ADR: "Compute and buffers join the document vocabulary."

### N4 — Effect parameters

Bloom's `THRESHOLD` and `STRENGTH` are `const`s; a "re-tune" is a new
descriptor row (ADR 0034 said so out loud). P11's buffers exist now, so:

* An `Effect` declares parameters (name, type, default); they land in a
  small uniform block bound with the pass group, and the editor/canvas
  exposes them as sliders — the material parameters' story, pass-level.
* Compute effects get the frame group (or a parameters-only group 0
  variant) when the first compute effect wants time or camera — ADR
  0035's stated condition.

* Cost: small-medium. The binding plumbing is the pass group's; the
  variant key must fold parameter *layout*, not values (the material
  parameters' precedent).
* Done when: a slider moves bloom's threshold live, no recompile,
  `cache_stats` unmoved.
* ADR: "Effect parameters are uniforms; the descriptor declares them."

### P5 — The pipeline canvas

The editor's second canvas, over the pipeline registry — plan2's own
guard rail: "a pipeline graph is a second canvas over the same model, not
a second editor". Deferred in plan2 until "P3/P4 give it something honest
to show"; the honest thing now includes policies, features, and (after
N3) compute. On every edit: validate, compile, `set_graph` — the preview
runs the real renderer, so a pipeline edit is live by construction.

* Palette = the pipeline node registry plus the effect registry; the
  problem panel names compile errors by node; a G-buffer/plan inspector
  shows the channel plan (P12's data, finally with a face).
* No new widget vocabulary; the cost is state management, which plan2
  already scoped honestly.

* Cost: medium; the bulk is editor work, as always.
* Done when: the subsurface demo's pipeline is *edited* in the editor —
  a bloom node dropped onto the deferred preset, live.
* ADR: probably none (no new boundary) — unless the two-canvas state
  model wants one, as plan2-architecture noted.

### P8 — Identity and contracts

Namespaced ids (`package::name`) for nodes, effects and models, with the
registries rejecting un-namespaced registrations from outside the shipped
set; a schema version on every serialized document (node format, scene,
pipeline) plus an ABI revision the documents pin; and published capability
metadata — provided stages, the channel plan, the lighting set, required
effects — with one device-free `RenderSetup::check(&pipeline, &scene)`
that returns every incompatibility by name. The feature-handshake check
is this proposal's proof of concept; the facts are already computed, they
are just not published as one value.

* Cost: small — days — and it is what turns "data-oriented" into
  "interoperable".
* Done when: a scene authored against a pipeline whose plan lacks its
  feature says so at load, by name, before anything is built.
* ADR: "Identity, versions, and the capability check."

### N5 — Bake passes (plan.md's M9)

Precompute part of a material into a texture and sample it back:

* A bake is an *effect over a material subgraph* — the descriptor's
  shader is generated from the subgraph the way a material module is.
* Invalidation is the Policy machinery, not a new system: a static bake
  is `once` (the LUT's shape), a re-bakeable one is `on demand` with the
  operation that dirties it calling `mark_pass`.
* Material side: a "baked" node transparently either the subgraph or a
  sample of its bake, so toggling costs no edit.

* Cost: medium. The shader-from-subgraph generation is the new part;
  everything around it is policy'd passes and stable storage, which
  exist.
* Done when: an expensive noise-driven PBR term bakes to a texture, and
  toggling the bake changes cost but not image (plan.md's own bar).
* ADR: "A bake is an effect over a material subgraph."

### M8 — Depth peeling

Transparency by dual depth peeling, drawn from the `transparent` tag:

* The `PeelFront`/`PeelBack` stage rows (owed since M2's table); a peel
  pass node with a layer count; the composite as an effect.
* The portability rule's first real bite: classic dual peeling blends
  `rg32float`, which the WebGPU baseline cannot. Native path: the blend
  trick, one geometry pass per layer. Baseline path: two ping-ponged
  `rg32float` targets compared in-shader, two passes per layer. Two
  implementations, so the msdf rule applies: a test asserts the two
  agree.
* Weighted blended OIT is a different *pipeline*, not a fallback — which
  is the argument for the pipeline being a document.

* Cost: large. The stages are medium; the two paths and their agreement
  test are the rest.
* Done when: four overlapping transparent surfaces composite correctly
  on both paths, within tolerance.
* ADR: "Dual depth peeling, and the baseline/native split for blendable
  float targets."

### N6 — The subsurface model

ADR 0037 shipped the channel, the pack, the round trip and the checks —
and said honestly that no model reads it. The first real subsurface
lighting model is the feature's consumer:

* A `subsurface` entry in `DEFAULT_MODELS` reading `extras.subsurface`
  under its feature macro, so it costs nothing when the feature is off.
* If the model needs per-fragment *surface*-driven strength rather than
  the macro constant, that is the `Surface` field schema change ADR 0037
  recorded — it lands with this item, deliberately, because this is the
  consumer that justifies it.

* Cost: small for a constant-strength model; the schema change is the
  escalation path, taken only if the model needs it.
* Done when: the gallery's subsurface demo stops being pixel-identical
  to deferred — the honest gap the demo's blurb names today.
* ADR: the model lands as a registry entry; the schema change (if taken)
  amends ADR 0037.

### N7 — Lighting at scale

The cluster of debts plan.md carried under M5's "still owed" and its
hard-parts table, one item because they share the frame group's tightest
corner:

* **Shadow quality**: an atlas with per-light resolution (a shadow wants
  more texels the closer its light is), instead of fixed-resolution
  array slices — one differently-shaped binding plus a rect in the light.
* **Shadow kinds**: cascades that follow the camera; point-light shadows
  (six faces to a light — today a point light asking for shadows gets
  none rather than a wrong one).
* **Light count**: `MAX_LIGHTS = 4` — a light list built by a compute
  pass, or clustered lighting. plan.md's own note stands: the render
  graph makes this additive (a compute pass and a bind group, not a
  redesign), and the policies make the build pass `per frame` for free.
* The demo's missing ground plane — the reason `pbr_cube` shows no
  shadow at all — is a one-line fix that rides along.

* Cost: large in total, but each of the four is contained; the atlas is
  the one that reshapes a binding.
* Done when: the gallery shows a shadowed ground plane, a cascade
  following the camera, and sixteen lights.
* ADR: one per landing piece (atlas; cascades; light lists) — they are
  three decisions wearing one trench coat.

### M11 — The code editor

Unblocked at any time; the plan.md write-up stands unchanged:

* Multi-line editing in the immediate-mode layer — cursor, selection,
  keyboard navigation, scroll-to-cursor, per-buffer undo (the editor has
  no undo at all; a text buffer is where that stops being tolerable).
* Compiler diagnostics in the gutter, from spans the compiler already
  carries.
* The runtime half of M0: saving a buffer calls
  `node_from_source` and the function appears in the palette in the same
  session — "write a function, get a node".

* Cost: large, and it is UI work rather than shader work — plan.md said
  so and nothing changed.
* Done when: a function written in the editor becomes a usable node in
  the same session, and a syntax error is reported on its own line.
* ADR: probably none; a different text model for `ui` would want one.

### M10 — Relative-to-eye rendering

The ABI hook landed in M5 and is tested; what remains is the host half:
model matrices pre-translated by the camera on the CPU in `f64`, and the
camera's translation removed from the view matrix. Until then the flag
buys nothing — the numbers are absolute `f32` by the time they reach the
shader. Done when a scene at a 10^7-unit offset renders without jitter
and the near-origin image is unchanged (the second half already holds).
Cost: small and self-contained; a pure host-side change with an existing
acceptance criterion.

### N8 — Release readiness

The items `todo.md` has carried, plus the one decision behind them:

* **The document format story.** `serde_json` is a development
  convenience; decide whether documents ship as JSON compiled ahead of
  time, or in a compact own format, or stay JSON with the dependency
  behind a feature. The decision is really "what does a shipped
  application embed", and it gates binary size and compile time more
  than anything else on this list.
* **Compile-every-shader before shipping**: a production gate that
  compiles the whole corpus (corpus gate + every shipped preset under
  every config) with minimal libs linked, so a release cannot ship a
  generator that panics on a driver.
* **The web target.** Decided after this plan was written: WebGL2 is a
  target, and the work has its own item — N10 below — with the ADR that
  amends plan.md's accepted cost. Nothing is left here to force.

* Cost: small for the gate, medium for the format decision; the web
  target's bill is N10's, and it is scoped there.
* ADR: "What a shipped application embeds" (the format + gate); N10
  carries its own.

### N9 — The editor catches up

The engine gained policies, buffers, feature channels and a channel plan
in the last stretch; the editor's chrome still shows only what M6 knew —
a toolbar, a palette, an inspector, a code panel with three tabs, a
status bar. Almost everything below reads data that already exists. This
is the cheap half of P5, landable without the canvas, and it slots in
wherever a pause wants filling, M11-style.

*Panels that read what the engine now knows:*

* **A features panel.** The shipped feature registry (`FEATURES`) as
  checkboxes driving `Renderer::set_features` live, over a byte-budget
  bar drawn from `gbuffer_layout_bytes_per_sample` — 28 of 32 today.
  The budget function exists; the editor just draws it. Toggling a
  channel and watching the plan, the budget and the image move in one
  frame is the demo P12 never had.
* **A policy inspector.** One row per pass: label, policy, runs, last
  size, due. "Run now" is `Renderer::mark_pass(label)`; reset is
  `reset_runs()`. This is the only face P10 has ever had — and N5's
  bake UI reuses it as its invalidation button.
* **The resolved material config** in the inspector: model, macros,
  feature channels, uniform parameters with live `drag_value` sliders
  (the widget exists; M3 landed the parameters). P7's one config, with
  a face.

*Modes:*

* **Debug views** — a View menu isolating one G-buffer channel (albedo,
  normal, depth, metallic-roughness, emissive, subsurface), plus
  overdraw and wireframe. The first mode builds the only new seam — a
  debug pipeline derived from the current one's plan, final pass
  swapped — and every mode after is a row. The channel plan is data, so
  the menu is data-driven: channels the plan does not carry are greyed
  out, not missing.
* **A/B split** — the viewport divided between two pipelines with a
  draggable edge. Forward/deferred parity is already asserted by test
  (mean diff 0.0002); this shows it live.
* **Pass stepping** — pause, run one pass at a time, show any
  persistent target in a corner viewport; the screenshot harness the
  GPU tests use is the same code. RenderDoc in miniature, and honest
  precisely because passes are data.
* **Hot reload** — watch the shader files; on change, recompile and put
  the spans in the problem panel. Half of M11's payoff, none of its
  text-editing cost.

*The one new engine capability:* pass timings via timestamp queries —
`cache_stats` shows the variant cache, but nothing shows where a frame
went. Small, but it is render-side, and the WebGL2 story for timer
queries is messy: it lands native-first, with an honest absence in the
web column.

* Cost: small per panel, medium in total — editor work, as always. The
  debug-view pipeline is the only piece that touches wxsl-render.
* Done when: the subsurface channel has an isolation view, a features
  checkbox re-renders live while the budget bar moves, and a policy
  row's "Run now" visibly re-runs the LUT bake.
* ADR: probably none — the debug view is a derived pipeline, not a new
  boundary. If it wants to fork the pipeline build, that is the signal
  to stop and write one.

### N10 — WebGL2 is a target

The decision plan.md deferred — "WebGL is not a target, so
`VERTEX_STORAGE` is a cost only if that ever changes" — has been made:
it changed. WebGL2 is a target, which reopens that accepted cost on
purpose, by plan rather than by drift. What the third column actually
takes:

* **The shader backend's missing leg.** WXSL → WGSL exists; WGSL →
  GLSL ES 3.00 is the new piece, and the honest route is naga
  (wgsl-in, glsl-out) behind the existing compile seam. The generator
  stays untouched, and the corpus gate gains an ES column that compiles
  every shipped shader twice. "Logic in Rust, text in files" survives
  because naga consumes the emitted WGSL, not the generator's internals.
* **Three shipped shapes cannot lower to ES 3.0**, each with a fallback
  decided here rather than discovered later:
  * **No compute.** P10's compute effects (the LUT bake, the ramp fill)
    get fragment lowerings — a LUT bake is trivially a fullscreen pass
    writing the same texture (float render targets ride
    `EXT_color_buffer_float`, near-universal and recorded honestly).
    The Policy machinery is untouched: due-ness lives in the scheduler,
    which is device-free by guard rail. Rule from here on: *every
    compute effect registers its fragment fallback beside it, or names
    itself native-only* — at landing time, not port time.
  * **No storage buffers.** P11's buffers lower by use: read-only data
    to a UBO, read-write scratch (the ramp) to a texture ping-pong or a
    CPU upload. The graph's `ResourceShape::Buffer` gains a lowering
    table; the never-aliased rule survives untouched.
  * **Vertex-stage storage.** The instance-transform row — plan.md's
    accepted cost, now real. Two shapes: per-instance vertex
    *attributes* (ES 3.0 instanced arrays; the shape ADR 0010's dynamic
    offset was already near) adopted for *everyone* — one
    implementation, no fork, but a mat4 spends four vertex-attribute
    slots, and N2's previous-frame matrix would make it eight of
    sixteen — or a second, web-only instance implementation, which owes
    the msdf-rule agreement test forever. The ADR picks one; the money
    here is on attributes-everywhere if the slot math survives N2,
    because one implementation beats two.
* **The device asks less.** With the web feature on, the requested
  downlevel flags drop `VERTEX_STORAGE`; the background pipeline swap
  degrades to blocking exactly as already designed, with the progress
  indicator keeping it honest. One test suite creates its device under
  WebGL2-equivalent downlevel flags, so a violation is a red test the
  day it is written, not a console error in someone's browser.
* **Where it lands:** a `wasm32` feature on wxsl-render and the editor
  (winit/web-sys); the boundary checks gain a wasm check beside
  `--no-default-features` and `--features editor`.

What survives untouched is the quiet payoff of the last year:
documents, graphs, the scheduler, the policies, the editor's MSDF UI —
none of it knows or cares which backend is underneath.

* Cost: large in total but flat — lowering tables, a build target, and
  one real decision (the instance shape). Sequencing note: that
  decision gates N2's second matrix, so the ADR lands before N2 even if
  the port waits.
* Done when: the gallery runs in a browser under WebGL2 — forward and
  deferred, the LUT baked by its fragment fallback — and the corpus
  gate's ES column is green in CI.
* ADR: "WebGL2: the third column" — amends plan.md's accepted-cost note
  in place.

## Suggested order

1. **P7** — a day-class consolidation that pays immediately and un-threws
   the options plumbing every later item touches.
2. **N1** — tonemap out, IBL in: small, visible, and it completes the
   loop ADRs 0034/0035 opened (the LUT gets its reader; bloom gets its
   honest threshold).
3. **M7** — the screen domain, the spine of the remaining plan. After it,
   an effect can be a graph, and P5 has a full story to show.
4. **N3**, then **N4** — compute and buffers in documents, then effect
   parameters. The vocabulary the canvas will draw should exist before
   the canvas does, for the same reason P9 ran before P3.
5. **P5** — the pipeline canvas, over a finished vocabulary.
6. **P8** — contracts, before M8's second pipeline shape and before any
   cross-author exchange is invited.
7. **N5** (bake), **N2** (motion/TAA) — independent of each other; bake
   reuses the policy machinery, motion adds the Velocity stage.
8. **M8** (peeling) — the largest remaining render feature, and the one
   that wants P8's contracts published first.
9. **N6** (the subsurface model), **N7** (lighting scale) — feature work
   on top of a finished frame.
10. **M11**, **M10**, **N9** — the editor's code editor, the precision
    host work, and the editor catching up; all unblocked at any time, so
    they slot in wherever a pause wants filling. N9's panels are the
    cheapest visible wins on this list — everything they show already
    exists.
11. **N10** — the *ADR* lands before N2, because the instance-shape
    decision gates the previous-frame matrix, and the downlevel corpus
    gate runs alongside from the start; the port itself follows P8,
    before M8 and N2 spend more of the budget the third column prices.
12. **N8** — the release gate and format decision run alongside from the
    start.

This order finishes the *authoring* story first (config, screen domain,
documents, canvas, contracts) and then spends its render-feature budget
(bake, peeling, lighting scale) on top of it — the same shape plan2 had:
make the next features cheap before making the features.

## Risks, and how each is de-risked

| Risk | Mitigation |
|---|---|
| **The screen domain forks the node model** — a third registry, a second canvas, special rules. | It reuses `wxsl-core`'s graph, `ValueType`'s handle trick, and the registry's domain filter — the mechanism `context_read` vs `vertex_context_read` already is. A screen node needing a mechanism materials lack is a signal to generalize, per plan.md's own guard rail. |
| **Multi-pass effects with internal transients** (a real bloom pyramid) need the compiler to synthesize sub-documents with scoped name allocation. | Deferred honestly twice now (plan2-architecture, ADR 0034). When forced, the scheduler already orders and aliases; only name allocation is new. The single-pass shipped bloom is the honest fallback that keeps the seam working meanwhile. |
| **TAA ghosting** — reprojection wrong, disocclusions smear. | The history rings and the velocity target exist; the acceptance test is the gallery's spinning cube, which has ground truth. TAA ships behind a policy'd chain, so shipping it is a document edit, not a code path. |
| **Two render implementations drift** (M8's blend split; N10's web column — the "later" arrived). | The msdf rule, stated now: every second implementation owes a test asserting the two agree, before the feature lands. |
| **Variant explosion grows again** (stages × macros × models × feature plans × screen graphs). | Key precisely, compile lazily, warm explicitly — `cache_stats()` and the swap progress indicator exist; P8's published metadata is what tells a pipeline *up front* which combinations it needs. |
| **The editor becomes two editors.** | P5 reuses the canvas component, the palette, the problem panel and the undo-less immediate-mode layer as-is. The pipeline registry is a second *instance*, not a second framework. |
| **The WebGL2 column growing by half-measures** — an effect lands compute-only with no fallback named, and the port rediscovers each one late. | N10's rule applies at landing time: every compute effect registers its fragment fallback beside it or names itself native-only, and the downlevel corpus gate runs from the start, so the first violation is a red test, not a browser console. |

## Guard rails

Carried unchanged from plan2, because they are what kept it honest:

* No GUI toolkit; the editor still draws itself with the renderer.
* One graph model — a new domain or canvas reuses `wxsl-core`'s, never
  forks it.
* The scheduler stays pure and device-free; documents compile to
  `RenderGraph`, and everything checkable is still checked there.
* Generation stays *logic in Rust, text in files*: a generator that owns
  more than its holes has taken text that belongs in a template.
* Analyses run on the graph, never on generated text; the WXSL text is
  the emit format, not the analysis substrate. (The instruction-IR
  question was answered in plan2 and stays answered: a real IR waits for
  a second backend that needs one.)
* New document fields default to today's behaviour — every graph that
  exists compiles unchanged.
* Every proposal here lands with an ADR and a runnable demo.

Two added by what this stretch of work taught:

* **An ADR's "waits for its first consumer" is a queue entry, not a
  graveyard.** Every deferred half in ADRs 0034–0037 is an N-item here
  by name; the same discipline applies to whatever 0038+ defer.
* **Two implementations owe an agreement test before they land** — the
  msdf rule, generalized now that M8 and N10's web column both make it
  load-bearing for rendering.
