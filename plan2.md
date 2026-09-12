# Plan 2: making features cheap — pipelines as data, shaders as files

Written after M6 landed. It is a *retrospective that turned into a plan*:
M6 should have been a small feature ("let a material pick a lighting
model") and took a full milestone's effort. This file asks why, separates
what was inherent cost from what was debt, and proposes the work that
makes the *next* features fast. It also carries the goal the plan it
sits beside has not reached yet: **authoring pipelines as data, in the
editor, and chaining them** — so it deliberately overlaps plan.md's
section 4 ("The pipeline graph, as nodes") and M7, and should be read as
the accelerated, debt-first route to them.

Status: P6, P2, P1 and P9 have landed — ADRs 0029, 0030, 0031 and 0032.
The rest is proposal; each numbered item becomes an ADR when it lands. The architecture behind P2–P5 — where each layer
lives, what it holds, and what it costs — is written out in
[plan2-architecture.md](plan2-architecture.md).

This file also absorbs [request.md](request.md), the statement of the
destination: a programmable, data-oriented rendering framework rather
than a material editor, with shader graphs, render graphs, passes and
resources as separated concepts. Its closing paragraph asks for an
honest review of where the current design prevents that model — that
review is *"request.md against the current design"* below, and it is
what added P9–P12 to the proposal list.

## Why M6 was slow — an honest breakdown

Four things consumed the time, in roughly equal parts:

1. **Inherent: the ABI was fixed shipped text.** The one thing M6 needed
   to change — a function *call inside the light loop* — lived in
   `shading.wxsl`, and no shipped module can name things it was never
   told about. The light loop, ambient, tonemap and the whole G-buffer
   pack/unpack had to move into Rust string generation. That cost is now
   paid once, but it was paid by translating ~120 lines of readable WXSL
   into `format!` templates — the single largest chunk of the milestone.
2. **Inherent: real domain constraints.** WebGPU's attachment budget
   (every vec4 target costs 8 bytes per sample, so the base G-buffer has
   nearly spent the 32-byte floor) genuinely forced the precision variants
   and the budget check. Not debt; the kind of thing a milestone exists
   to find.
3. **Debt: no early gate for shader text.** Generated WXSL was only
   exercised when a GPU test ran, so a typo in a template surfaced as a
   `wgpu` validation *panic* (message half-swallowed) minutes after the
   change that caused it, instead of as a compiler diagnostic seconds
   after. Every iteration on the generator paid GPU-test latency for what
   is a pure-text check.
4. **Debt: everything downstream of a pipeline is threaded by hand.**
   Adding one knob (the lighting set) meant touching `StockPipeline`,
   `deferred_graph`, `Renderer::new`/`rebuild`/`set_lighting`,
   `request_pipeline`, the variant cache keys, the graph validation and
   the tests — seven sites for one concept. The stock pipelines
   themselves are bespoke Rust functions with hardcoded pass lists, and
   `PassKind::Screen` takes a *hardcoded enum* of screen shaders. This is
   the "everything looks hard-coded" feeling, and it is correct.

Point 1 also produced the most bugs-per-line of the milestone (macro
redeclarations, escaping mistakes) for a structural reason worth naming:
**shader text embedded in Rust strings violates the repo's own best
decision** — ADR 0020, "the file is the truth". The node system learned
this; the ABI half of the shader stack has not.

## request.md against the current design

request.md describes the destination and asks where today's design
blocks it. Read against the code, the answer is: **most of the
architecture already exists under other names, four things genuinely
block it, and one ask deserves a reasoned no-for-now.** The one fear
named explicitly — "add special cases for forward/deferred rendering"
— is the one already answered: the material graph produces one
`Surface output` with semantic optional channels (`SURFACE_FIELDS`:
base_color, metallic, roughness, normal, emissive, occlusion, alpha),
no graph contains `if forward`, the same subgraph compiles for every
stage (ADR 0022/0025), and the G-buffer pack is generated per lighting
set (ADR 0028) while forward generates none.

What already exists, mapped:

| request.md asks | What exists | Still owed |
|---|---|---|
| Shader graph ≠ render graph | Already two types: `wxsl_core::graph::Graph` (values, the canvas) and `wxsl_render::RenderGraph` (passes, resources, schedule). They meet only in codegen and `PassDesc`. | Authoring the second as data — P3 |
| MaterialOutput decoupled from the G-buffer | It is: one surface terminal, semantic channels, per-stage compilation, generated pack | Material-declared channels — P12 |
| Render graph derives dependencies and resource lifetimes | `RenderGraph::schedule` is pure and device-free; transient aliasing; `Persistence::Persistent { history }` rings are the history resources; `Read::previous`; imports | Passes as data — P3; policies — P10 |
| Compute passes are first-class | `PassKind::Compute` with entry, workgroups, reads/writes, recorded as a real compute pass | Shader graphs driving compute — the third graph domain, with M7 |
| A pass describes inputs/outputs/state | `PassDesc { kind, view, color, depth, state, reads, writes }` is that struct; `reads` become the pass bind group automatically | Who authors it — P3; self-description — P4 |
| Vertex position, discard, MTR, no manual varyings | `Vertex output` (object-space displacement, shared into shadows), `Discard`, the multi-target G-buffer stage | Explicit stage control — P9 |
| Lighting as a graph, post chains, arbitrary pre/post passes | The lighting-model registry entry was built as the seam for graph-authored models (ADR 0028) | Not gaps — P3/P4 and M7, already proposed |
| Validation before WGSL, errors naming the graph | Graph-level typing and cycles at edit time; M6's checks name sets and signatures | Port-level errors as the standard — P3's compiler |

The four real gaps:

1. **Stage membership is wiring, not analysis.** A node's stage is
   decided by what it feeds: the vertex stage is reachable only through
   the `Vertex output` and `Interpolant output` terminals, and every
   cross-stage value is hand-declared — name the interpolant, wire the
   vertex side, read it back with `input.attribute`. request.md's
   Auto/Vertex/Fragment model with automatic varyings is a new compiler
   analysis. It is an evolution, not a rewrite: `context_read` vs
   `vertex_context_read` already is a per-node stage-eligibility rule,
   and the interpolant mechanism is exactly the node pair the analysis
   would synthesize. **P9.**
2. **Passes have no execution policy.** Every pass records every frame,
   so "runs once and produces a persistent texture" (the BRDF LUT) is
   inexpressible — every part of it exists except the *once*. **P10.**
3. **Graph resources are textures only.** `ResourceDesc` is
   texture-shaped; the buffers a frame uses (draw queue, uniforms) are
   implicit ABI infrastructure the scheduler cannot see, so
   request.md's storage-buffer and uniform resources cannot participate
   in dependency reasoning. **P11.**
4. **The IR: a position, not a gap.** request.md asks for an
   intermediate representation between the graphs and WGSL and for
   nodes decoupled from shader strings. Today `NodeBody::Expr`/`Call`
   emit WXSL text and `wxsl-lang` is the backend — the text *is* the
   IR. The honest position: a backend-independent instruction IR
   (naga-shaped) is a milestone-scale rewrite of node codegen whose
   benefit needs a second backend to exist, and every analysis
   request.md wants (stages, resources, channels) is *simpler* run on
   the graph before emission than on an instruction stream. So: the
   analyses land at graph level (P9, P11, P12), text stays the emit
   format, and a real IR waits for a second backend that needs it.
   This is the one request.md ask this plan deliberately does not take
   literally.

## Proposals

Ordered by (payoff ÷ cost). P1 and P2 are quick wins that pay for the
rest; P3+P4 are the pipeline abstraction; P5 is its UI; P6 is the
developer-experience floor; P8 is cross-author interop; P9–P12 are
what reconciling request.md added — the stage analysis, execution
policies, buffer resources and semantic channels the current design
genuinely lacks.

### P1 — Shader text lives in files; generators fill holes

The lesson of ADR 0020 applied to the ABI: a *template* is a `.wxsl` file
under version control, readable and editable as shader, with a small
number of named holes the generator fills. Rust keeps the *logic* (which
models, which targets, which ids); files keep the *text* (loop, ambient,
unpack, bindings).

* Sketch: `shaders/wxsl/shading.wxsl` returns, with holes such as
  `${MODEL_IMPORTS}`, `${DISPATCH_BODY}`, `${EXTRA_DECL}`. The generator
  in `wxsl_core::lighting` stops owning 40 lines of `format!` and starts
  owning a table of hole → text, assembled from the registry. Same for
  the lighting pass.
* Cost: small. No compiler changes — this is string assembly with the
  text on disk (`include_str!`, like `wxsl-stdlib` already does).
* Payoff: M6-class features become "edit the template, add a registry
  row"; shader authors can read the generated output's source; the
  escaping class of bugs disappears.
* Alternative with a bigger payoff and a bigger cost: give WXSL an
  `@expect fn` (a required symbol the *mounting root* must provide — the
  inverse of an import). Then `shading.wxsl` stays a full file declaring
  `@expect fn lighting_dispatch(...)`, the generated material module
  provides it, and the generator shrinks to just the dispatch. This is
  the cleaner end state (generation only for the piece that varies) but
  touches `wxsl-lang`'s resolver; worth an ADR of its own. P1's template
  mechanism does not preclude it — the template holes can migrate to
  `@expect` later, hole by hole.

### P2 — One `PipelineConfig` instead of threaded parameters

Every knob a stock pipeline can vary by, in one data struct, threaded
once:

```rust
pub struct PipelineConfig {
    pub target: TargetConfig,          // exists today
    pub lighting: LightingSet,         // landed with M6
    pub effects: Vec<EffectId>,        // P4; screen effects, bloom, …
    // one field per future knob — not one signature per future knob
}
```

`StockPipeline::graph(&self, config)` interprets it; `Renderer` holds one
and `set_pipeline`/`set_lighting`/`enable_effect` become small mutations
of it. The variant-cache and bind-group derivations read the same struct.

* Cost: small — a refactor of signatures that already exist.
* Payoff: the *next* pipeline feature is a field, not a seven-site
  change; and P3 gets a single value to serialize.

### P3 — Pipelines as documents, presets as files

The actual "abstract pipelines" ask. A pipeline becomes **serializable
data**, like the scene document already is:

* `wxsl_core::pipeline` (new): nodes for *sources* (scene geometry,
  camera, lights), *passes* (material stage + tag expression + state +
  view), *resources* (target with format/scale/layers, persistence), and
  `present` — reusing the node/socket/type model the material graph
  already has, exactly as plan.md's section 4 sketched. Edges carry
  resources. The document validates and **compiles to a `RenderGraph`**
  by a pure function — testable with no device, schedulable by the
  existing engine.
* The two stock pipelines become **preset documents** shipped as files
  (`presets/forward.pipeline.json`), so they are readable, diffable and
  copyable. `StockPipeline` remains as a name for a preset; the preset
  loader is the only place that knows them.
* Chaining falls out: a chain *is* edges between pass nodes. Bloom after
  deferred lighting, a depth prepass before peeling — compositions the
  current code expresses as more hand-written Rust become document
  edits.
* Cost: the largest item here, but it is plan.md's own section 4 with
  P1/P2 done first, and the hard engine work (ordering, transient
  allocation, history) already exists and stays.

### P4 — Effects as first-class units

The pass-level twin of what a node is to a material. An effect is a
self-describing unit: what it reads and writes, its shader (a screen
graph or a template-filled module), its parameters, its pass state. Bloom
is one effect, not four passes of bespoke Rust; FXAA is one effect;
tonemap stops living inside `shade_surface` (plan.md M7 already wants it
out) and becomes one.

* `PassKind::Screen`'s hardcoded `ScreenShader::DeferredLighting` enum is
  replaced by a descriptor — the first effect is the existing lighting
  pass itself, migrated.
* Effects are what the pipeline-graph palette lists, and what an
  application can add without touching `wxsl-render` (ADR 0009's rule,
  extended from shaders to passes).
* Cost: medium; unblocks M7 almost entirely and M9 (a bake pass is an
  effect over a material subgraph).

### P5 — The pipeline canvas

The editor's second canvas, over the same model (plan.md's own guard
rail: "a pipeline graph is a second canvas over the same model, not a
second editor"). With P3's documents this is mostly editor work:

* a pipeline panel: load a preset, see its nodes, rewire, add an effect
  from the palette, see the compiled pass list and (once M7 lands) the
  frame it produces;
* the preview already runs the real `Renderer`, so a pipeline edit is
  live by construction;
* no new widget vocabulary: nodes, links and settings already exist.

Cost: medium; it is what makes the abstraction *friendly* rather than
merely present.

### P6 — Developer-experience floor (the gate M6 lacked)

* **A shader corpus gate**: one test that generates every generated
  shader (all telling lighting sets, all stages, both dispatch shapes)
  and compiles it to WGSL with no device, failing with the compiler's
  rendered diagnostic. M6 built ad-hoc versions of this in
  `wxsl-stdlib/tests/lighting_models.rs` and `wxsl-lang/tests/corpus.rs`;
  formalize it as the place every generator is checked, so template and
  generator changes fail in seconds. Cheap, and it would have caught
  every one of M6's generation bugs before any GPU test ran.
* **Error scopes in GPU tests**: wrap `create_shader_module`/pipeline
  creation in `wgpu` error scopes inside the test harness so a validation
  error becomes a catchable `Result` naming the pipeline, instead of a
  panic mid-frame with the message truncated.
* **Budget numbers from the spec table, tested against `wgpu`**: M6 got
  `gbuffer_bytes_per_sample` wrong once by guessing; a test asserting the
  cost table against `wgpu`'s own `target_pixel_byte_cost` (when
  available) or against golden numbers with a comment naming the spec
  section keeps the mirror honest.
* **A scene/graph test kit**: generalize `tests/probe/mod.rs` into the
  one way acceptance tests describe scenes (meshes, materials, lights,
  sample points), so each milestone's acceptance test is a scene
  description plus assertions, not a new harness.

### P8 — Identity and contracts (cross-author interop)

The addition the interop question surfaced: namespaced ids
(`package::name`) for nodes, effects and models; schema/ABI versions on
every serialized document; and published capability metadata (provided
stages, G-buffer layout, lighting set, required effects) with one
device-free `RenderSetup::check(&pipeline, &scene)` so "will A's
pipeline draw B's scene?" is a validator answer, not a runtime surprise.
Detailed in [plan2-architecture.md](plan2-architecture.md)'s "Will it
interop?" section. Small — days — and it is what turns "data-oriented"
into "interoperable".

### P9 — Stage analysis: the stage cut becomes computed, not wired

request.md's sharpest ask, and the core-data-model refactor worth doing
before the pipeline document freezes the node vocabulary. Today the
stage is decided by wiring; the proposal makes it a compiler analysis:

* A per-node stage setting — `Auto` by default, with explicit
  `Vertex`/`Fragment` constraints where the author wants them (compute
  kernels are their own graph domain, not a stage of a surface graph).
  `Auto` means *earliest stage that can produce me and satisfies every
  consumer*: shared nodes are evaluated once, in the earliest valid
  stage.
* The analysis partitions the value graph per stage and synthesizes the
  cut: interpolant declarations, the vertex-side write and the
  fragment-side read become generated instances of the interpolant
  mechanism M4/M5 built — the same output the user produces by hand
  today, minus the labour. The 16-location budget and the error that
  reports running out stay; they just fire against the analysis now.
* Moving a node between stages becomes a setting change, and the editor
  shows the computed stage per node — the graph is the stage
  visualization. The seeds already exist: `context_read` vs
  `vertex_context_read` is a per-node eligibility rule, `Vertex output`
  a forced-vertex consumer.
* Errors name ports: "`noise` runs in the vertex stage; `albedo`
  consumes it in the fragment stage" — request.md's validation bar.

Cost: medium — the analysis plus its tests, not a rewrite. The
varyings machinery, the location budget and the shadow-pass vertex
sharing all exist and stay. ~3–5 days. **Order matters: before P3**, so
the pipeline document is authored against the final node model instead
of acquiring special cases for the old one.

### P10 — Execution policies

request.md's Once / Per frame / On resize / On demand. `PassDesc` grows
a policy; the renderer's frame loop grows the dirty tracking to honour
it. The proof case is the BRDF LUT: a compute effect writing a
`Persistent` resource with policy `Once` — expressible today in every
part except the *once*. The interaction to get right is invalidation: a
pass that never re-runs but whose target was resized must re-run, so
`On resize` is really "re-run when my inputs' shapes change". The
document node exposes the policy as a setting, so P3's vocabulary gains
one row rather than a concept. ~2–3 days.

### P11 — Buffers and storage as graph resources

`ResourceDesc` is texture-shaped, and the buffers a frame uses (the
draw queue, the frame group's uniforms) are implicit ABI infrastructure
the scheduler cannot see. request.md's resource list — storage buffer,
storage texture, uniform data — wants them *declared*, so dependency
reasoning covers them the way it covers attachments. `PassDesc::writes`
already names non-attachment writes; what is missing is the descriptor
side (a buffer kind beside the texture fields, or a sibling
`BufferDesc`), allocation that is not an image, and pass-group binding
for storage buffers — the pass group's builder is the only part of bind
generation that has to learn anything. Scheduler-side a buffer is a
resource whose aliasing rule is "never" at first, refinable later.
~3–5 days, deliberately after P3 so the document vocabulary models it
once.

### P12 — Semantic channels

request.md's G-buffer section: channels with semantics and types,
derived from what materials and lighting actually require, rather than
anonymous slots. ADR 0028 already built the load-bearing half —
per-model targets carry names, precisions and pack functions, the
layout is computed, the budget check is real. What is missing is a
*second source* of channel requests: a material feature (subsurface)
asking for a channel without going through a lighting model. The
refactor is small — channel requests become a collected list with a
source tag instead of "the lighting set's extras" — but its first
consumer does not exist yet, so it lands with the first material
feature that needs it: the same rule P7 states for material
configuration, and for the same reason.

### P7 — Consolidate per-material configuration

`CodegenOptions.lighting`, `MaterialOptions.lighting` and the scene
document's `lighting` field are three spellings of one fact plus a
resolution step. The same will happen for every future per-material
feature. One `MaterialConfig` (macros, model, shadow flags, tags)
carried from document → options → codegen, with the resolution in one
place, keeps each new knob at one site. Small, and best done when the
second knob arrives rather than speculatively now.

## Suggested order

1. **P6** (a day-class change, pays immediately), then **P2**, then
   **P1** — all three are refactors with tests as the observable end
   state, and none block the others.
2. **P9** (stage analysis) — the core-data-model refactor request.md
   asks for by name ("refactor the core data model now"), done before
   the pipeline document freezes the node vocabulary.
3. **P3** (pipeline documents + presets), landing with forward/deferred
   as the first two presets and a pure compile-to-`RenderGraph` function
   under test.
4. **P4** (effects), landing with the lighting pass as the first
   migrated effect and bloom as the proof (this *is* M7's spine).
5. **P10** (execution policies) and **P11** (buffer resources) — small
   extensions once the document model exists to carry them; **P12**
   waits for its first material feature.
6. **P5** (the canvas), once P3/P4 give it something honest to show.

This order turns plan.md's M7 (screen domain, postprocess) and its
section 4 (the pipeline graph) into the same stretch of work, executed on
top of configuration data instead of signatures — and M8 (peeling)
becomes "another preset document plus two stages", which is the point.

## Guard rails

Carried over unchanged, because they are what kept M6 honest:

* No GUI toolkit; the editor still draws itself with the renderer.
* One graph model — pipeline nodes reuse `wxsl-core`'s, never fork it.
  A pipeline node needing a mechanism materials lack is a signal to
  generalize the mechanism.
* The scheduler stays pure and device-free; documents compile to
  `RenderGraph`, and everything checkable is still checked there.
* Generation stays *logic in Rust, text in files*: a generator that owns
  more than its holes has taken text that belongs in a template.
* Analyses run on the graph, never on generated text: the stage cut,
  resource requirements and channel layouts are computed before
  emission, and emission is the last step. The WXSL text is the emit
  format, not the analysis substrate — an instruction IR waits for a
  second backend that needs one.
* New document fields default to today's behaviour — `Auto` stages,
  per-frame policies, transient resources — so every graph that exists
  compiles unchanged.
* Every proposal here lands with an ADR and a runnable demo, like the
  milestones it accelerates.
