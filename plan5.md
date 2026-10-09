# Plan 5: forward-shaded materials, ordered transparency, and the derivative contract

Written when plan4's S2/S3/S4 node batches had landed and the guide's
ticket 2 — the fragment-only derivative contract — was still blocking
`roughness_aa` and every fwidth-based node after it. The prompt is the
owner's proposal, which is worth stating in their words before it is
restated precisely:

> Marquer ce qui n'a pas pu être fait à cause des dérivatives comme
> *forward only* : le shading est computé en forward, y compris quand
> deferred est activé (dans ce cas l'albedo est l'output). Le rendu
> devrait être front to back pour les objets opaques, back to front
> pour les transparents, sauf les objets qui s'entrepénètrent, rendus
> deferred — chaque objet transparent ayant une propriété *max layers*.

Three ideas, each real, each landing on a different piece of recorded
architecture:

1. **A material can demand forward shading even under a deferred
   pipeline**, its shaded colour stored in the G-buffer and passed
   through by the lighting pass. This is the escape hatch for everything
   derivatives and screen-space context blocked — and it converts the
   derivative question from "blocked" to "routed".
2. **Draw order becomes data**: opaque front-to-back, transparent
   back-to-front — per object, the painter's algorithm, which is sound
   exactly as long as transparents do not interpenetrate.
3. **Interpenetrating transparents keep the per-pixel mechanism**
   (ADR 0047's peel), with the layer budget moving from a pipeline-wide
   macro to a per-object `max_layers` — so a lone transparent sphere
   costs no peeling at all, and a knot of glass asks for exactly as many
   layers as it needs.
4. **Per-object render order, the Babylon.js rendering-group control**:
   an object can be pinned to draw before or after everything else,
   without authoring tags or extra passes — the coarse version of this
   already exists (passes are data, ordered by declaration), the
   per-object version is what is missing.

## What the proposal means, stated precisely

The proposal mixes three granularities that must be separated before
any of it is designed:

* **Fragment-only is a property of a *node*** — a body calling `dpdx`
  cannot run in a vertex stage, and the stage analysis (ADR 0032) has to
  refuse that placement by name. This is guide ticket 2 verbatim, it is
  the *prerequisite* item, and it does not by itself force anything
  forward: derivatives are legal in the G-buffer pass's fragment too.
* **Forward-shaded is a property of a *material*** — `MaterialConfig`
  grows a flag saying "shade this surface in the geometry pass, whichever
  stage the pass names". Under a forward pipeline it changes nothing.
  Under a deferred one, the shading function runs in the G-buffer pass,
  its radiance lands in the albedo target, and the lighting pass returns
  it as-is. That is the owner's "l'albedo est l'output", and the plan
  keeps that spelling.
* **`max_layers` is a property of a *draw*** — how many peel layers this
  transparent object may consume. Zero is the common case: the object
  does not self-overlap, so the painter's algorithm (item 2) sorts it and
  no peel pass ever touches it.
* **Render order is a property of a *draw*, inside one *pass*** — the
  fine half of the ordering story. The coarse half already exists and
  needs nothing: a pipeline is a document, `pass.geometry` draws a tag
  expression, and two passes writing the same target are ordered by
  declaration — which is how an overlay pass has always been forced in
  front of a scene. What Babylon.js's `setRenderingGroup` buys over that
  is per-*object* control without authoring a pass per level; that is a
  draw-level integer, and it must never grow into a second way to order
  *passes*.

## Where this plan touches recorded decisions

| Decision | What changes | How |
|---|---|---|
| ADR 0005 / 0021 — "sorting belongs to the application" | The renderer gains *optional* per-pass sort keys the application opts into; submission order stays the default and every existing document compiles unchanged. | Amended by D3's ADR, not superseded — the application's ownership survives as the opt-out. |
| ADR 0032 — stage analysis computes the cut | A node-level `fragment_only` constraint is a new input to that computation, refused in vertex/compute with a named error. | Extended by D1's ADR. |
| ADR 0022 — material stages | A forward-shaded material under a `gbuffer` pass is the third exception shaped like `velocity`'s (which reads no surface) and `peel`'s (which draws a tag): its material function returns *shaded radiance* where the stage's contract says "a `GBuffer` struct". The codegen variant is the ADR's business. | Extended by D2's ADR. |
| ADR 0028 — lighting models | The passthrough half of D2 may be spelled as a model (`wxsl.preshaded`, id dispatched like any other, lighting function = "return what the G-buffer stored") — which would reuse the model registry, the id channel and the dispatch wholesale — or as a stage variant. The ADR picks; the plan's money is on the model, because the machinery exists and the capability check already names models. | Decided in D2's ADR. |
| ADR 0047 — dual depth peeling | The layer budget moves from the pipeline-global `wxsl_peel_layers` macro to a per-object `max_layers`, clamped by that macro; the peel pass reads it per draw. The two-path native/baseline rule and its agreement test are untouched. | Extended by D5's ADR. |

## Proposals

Ordered by dependency, not importance: D1 is small and unblocks the
*reason* the owner asked for the rest; D3 is the sorting contract D4's
groups and D5's tiers stand on; D2 and D5 are the two large items and
are independent of each other.

### D1 — The fragment-only contract *(guide ticket 2)*

A node whose body needs derivatives — `dpdx`, `dpdy`, `fwidth`, or any
future non-uniform flow — cannot ride an interpolant and cannot run in a
vertex or compute stage. Today nothing says so at the node level: the
derivation sees a plain signature, and the stage analysis would happily
place the node in a vertex partition, where the WGSL compiler rejects it
three layers downstream.

* The constraint lives on the `NodeDefinition` — declared through
  `NodeDefinitionBuilder` (the same channel as a domain binding, so a
  derivative node cannot land in every palette by omission), and set
  automatically by the derivation when the body names a derivative
  builtin, overridable by the author.
* Stage analysis treats it exactly like its existing constraints: `Auto`
  never places the node outside the fragment stage; an explicit pin that
  conflicts is a named error; a node reaching `output.vertex` or a
  compute terminal through it is a named error naming the edge.
* The corpus gate gains the negative cases, as `interpolants` did for
  locations: a derivative node pinned to vertex fails by name, in about
  a second, with no device.
* The first consumers land with it: `roughness_aa` (guide S2.06), and
  the fwidth-driven `sdf_coverage` width pattern as a documented
  caller-side recipe or a companion node — the ADR says which.

* Cost: small-medium — the derivation half is a scan, the analysis half
  is one more constraint in a machine built for them.
* Done when: `roughness_aa` ships; a graph feeding it to
  `output.vertex` fails by name in the corpus gate; every existing
  graph compiles unchanged.
* ADR: "Fragment-only is a node constraint the stage analysis checks."

### D2 — Forward-shaded materials

The escape hatch, in the owner's spelling: a material marked
forward-shaded runs its shading in the geometry pass under *either*
pipeline. Under forward, nothing changes. Under deferred, the G-buffer
pass stores the shaded radiance in the albedo target, and the lighting
pass returns it — no lights, no shadow attenuation, no re-shading. The
picture is identical by construction, which is what makes this honest
where "forward pass bolted onto deferred" was not.

* **The passthrough is a lighting model.** `wxsl.preshaded` in
  `DEFAULT_MODELS`: its lighting function returns the colour the G-buffer
  stored (from `extra` or the base target — the ADR settles which), so
  the model-id dispatch, the capability check and the named-mismatch
  errors all already exist. A material marks itself with
  `MaterialConfig.forward_shaded`; resolution requires the preshaded
  model in the pipeline's set, by name, at load — the feature-handshake
  pattern, not a first-frame surprise.
* **The G-buffer half is codegen's.** Under a `gbuffer` pass, a
  forward-shaded material's fragment writes `shade_surface(...)`'s
  output where albedo goes, and the stage's remaining targets get their
  defaults. What the partition drops (the lighting pass never reads
  normals for this material) is ADR 0025's question again, with the same
  answer: compile what the pass reads, nothing more.
* **Honest costs, written down**: the material does not receive shadow
  maps or any per-light term — `receive_shadow` is meaningless next to
  this flag, and resolution rejects the combination rather than honouring
  one silently; the deferred G-buffer's byte budget pays for targets the
  material does not use; exposure still applies at the chain's end, so
  the stored value is radiance and ADR 0039's linear-until-tonemap rule
  survives untouched.
* **Forward/deferred parity becomes the acceptance test**: for a
  forward-shaded material the two pipelines must agree exactly — closer
  than tolerance, because the same fragment output travels both paths.
  The `shadows` and `lighting_models` GPU tests gain that assertion.
* Unblocks, with D1: roughness-AA'd shading under deferred,
  derivative-dependent models, screen-space-context shading — and it is
  the honest route for effects plan4 queued (full spectral iridescence
  with derivative-driven film thickness, if it wants one).

* Cost: medium — the model row is small; the codegen variant, the
  resolution checks and the parity assertions are the work.
* Done when: a gallery demo shades one roughness-AA'd material under the
  *deferred* preset, and the forward and deferred captures of that scene
  are pixel-identical for the preshaded object.
* ADR: "A material may shade forward under either pipeline" — amends
  ADR 0022's stage contract and records the model-spelling decision.

### D3 — Draw order as data

Opaque front-to-back (early-z), transparent back-to-front (the painter's
algorithm), per pass, computed by the renderer from the draw list and
camera — a pure function over data, which is what makes it testable in
`wxsl-frame` with no device.

* `PassDesc` grows `sort: None | FrontToBack | BackToFront`, default
  `None` — every existing document and hand-built pass list compiles and
  behaves identically, per the series' oldest guard rail. The shipped
  presets and the peel documents opt in where it matters.
* The key is the camera-space depth of the draw's transform origin. Good
  enough for the tiers this serves (D4's groups, D5's transparency):
  *object*-level sorting, which is
  exactly and only sound for non-interpenetrating draws — the plan says
  so rather than reaching for per-triangle topologies.
* **Sorting happens before instance rows are built** — a draw's index in
  the list is its row in the transform buffer (ADR 0024), so the sort
  reorders draws, not rows. Two consequences the ADR must carry:
  * The previous-frame rows must be sorted by the *same* order, or
    ADR 0046's velocity chain reads row *k* against row *j*. The sort
    key is computed from the current transforms and applied to both
    arrays; the motion test suite is the guard.
  * Per-instance attributes ride their draw; the reorder permutes them
    with it.
* The scheduler stays the last line: a sorted pass's draws are the same
  set, and the scheduler's checks never depended on order — but the
  `sort` field travels the document compiler like any `PassDesc` field,
  documents can pin it, and the editor's pipeline canvas exposes it as
  one more settings row.

* Cost: small-medium — the key function is a day; the instance-row
  interaction and the velocity guarantee are the care.
* Done when: a gallery demo submits N mutually-overlapping opaque cubes
  in adversarial order, renders identical images with `sort: None` and
  `FrontToBack`, and the motion tests stay green with sorting on.
* ADR: "Sorting is opt-in per pass, and it sorts rows, not draws" —
  amends ADR 0005's application-owns-order note.

### D4 — Render order: the per-object group control

Babylon.js answers "put this in front of everything" with
`setRenderingGroup(mesh, n)`: groups render in ascending order, and each
group sorts its own contents. The vocabulary here has the coarse half of
that already — passes are data, a document can put an overlay
`pass.geometry` after the main one, and the scheduler's declaration-order
tie-break (the same rule the bake's sample dependency leans on) is what
orders them. What is missing is the per-object half: pinning one draw
ahead of or behind the rest *without* authoring a pass per level.

* A **`render_order` integer on the draw**, travelling the
  `MaterialConfig` → draw path ADR 0038 built (the same road `max_layers`
  takes in D5) — default 0, so every existing scene draws exactly as it
  does today.
* **D3's sort key becomes lexicographic**: `(render_order, camera
  depth)`. All draws of order 0 sort and draw first, then order 1, and
  so on — within the pass. Front-to-back and back-to-front apply
  *inside* each order, which is exactly Babylon's group semantics.
* **It orders draws, never passes.** The scheduler's resource-based
  order is untouched and supreme; `render_order` has no meaning across
  passes, and a document that wants pass-level ordering keeps saying so
  with pass nodes, where it has always been sayable. This line is the
  ADR's spine: one ordering system per level, and the levels do not
  blur.
* The tiers compose inside an order: an order-5 transparent with
  `max_layers = 0` draws after every order-≤4 draw — including their
  peeled composites — which is the precise, sound spelling of "force it
  in front of the glass".
* Negative orders are legal (an underlay — a skybox, a reflection card —
  drawn behind everything without depending on depth alone); the ADR
  fixes the range and the clamp, as `wxsl_peel_layers` did.
* Opaque and transparent share the namespace deliberately: an order-1
  opaque draws after an order-0 transparent. That is a feature (force a
  cutout decal over glass) and also the edge the ADR must state —
  blending an order-1 opaque *over* an order-0 transparent is the
  author's decision, and the renderer does not second-guess it.
* The editor's inspector shows a draw's resolved `render_order` beside
  its tags — data a canvas can draw is data a canvas will draw.

* Cost: small — it is one more field on a road ADR 0038 built and one
  more term in D3's key; the care is all in stating what it does *not*
  order.
* Done when: a gallery demo pins one object at order 1 — an in-world
  hologram in front of a transparent sheet at order 0 — and the image
  is correct with the *submission* order of the two draws reversed
  between two runs; sorting off (`sort: None`) leaves submission order
  untouched, unchanged from today.
* ADR: "Render order sorts draws inside a pass; passes order passes." —
  recorded alongside D3's amendment of ADR 0005.

### D5 — `max_layers`: tiered transparency

The peel pipeline (ADR 0047) is the right mechanism for *interpenetrating*
transparents and an expensive default for everyone else — a lone glass
sphere pays two passes per layer for a sort that one comparison would
have settled. The proposal splits the `transparent` tag into two tiers:

* **`max_layers = 0` (the default, the new tier)**: the object is plain
  alpha-blended and sorted — D3's `BackToFront` — no peel pass reads it.
  A document's transparent pass draws the tier in submission-or-sorted
  order, straight onto the frame.
* **`max_layers ≥ 1`**: the object goes through the peel pass, consuming
  at most that many layers, clamped by the pipeline's `wxsl_peel_layers`
  macro (which stays the *pipeline's* cap — the per-object value is the
  draw's request under it).
* `max_layers` lives on the material configuration, travels the
  `MaterialConfig` → draw path ADR 0038 built, and the peel pass reads
  it per draw — the one genuinely new piece of plumbing, and the peel
  pass's first per-draw property since it landed as a tag.
* **The mixing rule, stated rather than discovered**: in a frame, the
  peel pass handles every transparent whose `max_layers ≥ 1`; the
  `max_layers = 0` objects composite *after* the peel composite, sorted
  back-to-front. That is sound when a sorted-only transparent does not
  interpenetrate a peeled one — which is the same assumption the sorted
  tier already makes about itself, and the demo shows the working case
  and names the broken one in its blurb. A draw whose `max_layers`
  exceeds the pipeline cap is not an error; it is clamped, and the
  editor's pass inspector shows the clamp — the same honesty
  `wxsl_peel_layers`'s own clamp has.
* The dual-peeling baseline/native split and the msdf-rule agreement
  test are untouched: the tiers change *who enters the peel pass*, not
  how peeling works.
* What this buys beyond cost: `wxsl_peel_layers` stops being a global
  tax, scenes with one overlapping pair stop peeling their forty other
  transparents, and the peeling demo's geometry-pass budget loosens for
  the objects that never needed it.

* Cost: medium — the per-draw property and the tier split in the
  document vocabulary are the work; the peeling itself is bought, not
  built.
* Done when: the gallery's `peel` demo gains a third object — a
  non-interpenetrating transparent plate at `max_layers = 0` crossing
  *behind* the peeled pair — and composites correctly on both paths;
  and a scene of ten `max_layers = 0` transparents plus one peeled pair
  runs the peel passes for the pair only (visible in the pass run
  counts).
* ADR: "Transparency is tiered: sorted by default, peeled on request."

### D6 — The derivative nodes this unblocks

The consumer ticket, last because everything above feeds it:

* `roughness_aa` (D1's done-when), the fwidth-width `sdf_coverage`
  recipe, AA'd `grid_mask`/`checker` variants — the guide's S2 fragment
  family, finally landable.
* Iridescence's honest version (plan4's queue) may want derivatives for
  the film's grazing terms — now it can, under either pipeline, via D2
  if it wants screen-space context.
* SSR stays behind N7 and TAA stability exactly as plan3/plan4 wrote —
  sorted transparency (D3–D5) removes one of SSR's edge headaches, which
  is a reason to do this plan first, not a promise SSR lands here.

* Cost: flat — authoring, gated by D1's corpus.
* Done when: the guide's ticket 2 closes and its derivative family is
  in the palette, compiled on every legal stage.
* ADR: none beyond D1's.

## Suggested order

1. **D1** — the fragment-only contract. Smallest, unblocks the family,
   and its corpus-gate patterns are established.
2. **D3** — draw order as data. Independent of D1; lands the
   instance-row and velocity guarantees *before* D4 builds transparency
   tiers on top of the sort.
3. **D2** — forward-shaded materials. The large shader-side item; its
   parity test needs nothing from D3–D5.
4. **D4** — render order, then **D5** — `max_layers` and the tier
   split. D4 is small and lands the group semantics D5's tiers read;
   both need D3's sort and borrow D2's
   *MaterialConfig-travels-a-new-property* pattern.
5. **D6** — the derivative family. Fills every pause.

D2 and D5 are independent enough to interleave with plan4's leftovers
(the sky, the IBL ADR, the pyramid) wherever a pause wants filling —
the same slot discipline plan3 gave N9.

## Risks, and how each is de-risked

| Risk | Mitigation |
|---|---|
| **Velocity breaks under sorting** — previous-frame rows sorted by a different order than this frame's. | The sort key is computed once per pass from the current transforms and applied to both arrays; the `motion` tests run with sorting on from D3's first landing, so the violation is a red test the day it is written. |
| **Preshaded materials silently lose shadows** — a user marks the flag without understanding. | `MaterialConfig::resolve` refuses `forward_shaded` with `receive_shadow` by name; the capability check reports a preshaded material under a set without the preshaded model the same way feature channels are reported today. The flag is a decision, never a fallback. |
| **Sorted and peeled transparents composite wrong** — the tiers' boundary case. | D5 states the rule (sorted composites after the peel composite) and its assumption (no interpenetration across tiers) in the ADR and the demo blurb; the demo shows the working case. Making the renderer *detect* interpenetration is not attempted — that problem is what peeling exists for. |
| **The parity test's meaning drifts** — "forward and deferred agree" has meant one thing since ADR 0028. | D2 sharpens it rather than weakening it: preshaded objects must agree *exactly*; the existing tolerance-covered assertions are untouched and stay green. |
| **ADR 0005's amendment opens the door to renderer-side scene graphs** — sorting today, culling tomorrow, a scene graph the day after. | The ADR draws the line explicitly: the renderer sorts *the draw list the application handed it*, derives nothing, owns nothing between frames. Draw-list-in, pixels-out is unchanged; the opt-out default is the proof. |
| **`render_order` grows into a second way to order passes** — a document that encodes pass structure in draw integers, and the scheduler's resource order and the declaration tie-break drift from what the author meant. | The ADR's spine: `render_order` is undefined across passes, and the document compiler refuses a pipeline whose geometry passes could interleave orders ambiguously only if that ever proves confusing — the first answer is documentation, the second is a named error, never silent reordering. |
| **`max_layers` on the draw leaks into the ABI's stride rule** — ADR 0024 forbids widening the instance row. | It rides the material's per-instance attribute path or the extra channel of the peel stage's own targets — the ADR picks; widening `Instance` is the one shape forbidden up front. |
| **Exposure double-applies to preshaded output** — once when shaded, once at the chain's tonemap. | The stored value is pre-exposure radiance and ADR 0039's single display transform is untouched; the parity test (exact agreement) catches any drift mechanically. |

## Guard rails

Carried unchanged from plan4 — they are what kept the series honest:

* No GUI toolkit; the editor still draws itself with the renderer.
* One graph model; the scheduler stays pure and device-free.
* Generation stays *logic in Rust, text in files*.
* Analyses run on the graph, never on generated text.
* New document fields default to today's behaviour.
* Every proposal lands with an ADR and a runnable demo.
* Two implementations owe an agreement test before they land.

Three added by this plan's subject:

* **A forward-shaded material is an explicit, checked decision** —
  refused at resolution when its demands cannot be honoured, never a
  silent fallback the renderer picks to make a node compile. The
  fragment-only contract (D1) is the *node* half and says exactly where
  a node may run; forward shading is the *material* half and says the
  author accepted losing per-light terms to get screen-space context.
* **Sorting rearranges the application's draw list; it never becomes a
  scene.** No culling, no visibility tracking, no between-frame state —
  the opt-in field is per pass, per frame, stateless.
* **The painter's-algorithm tier says its assumption out loud** — object
  order is sound for non-interpenetrating draws, and `max_layers` is the
  honest answer for everything else. A tier that quietly grows
  per-triangle sorting is a fourth pipeline, and owes its own ADR before
  it exists.
* **One ordering system per level, and the levels do not blur** —
  passes order by being data (documents, scheduler), draws order by
  `render_order` inside one pass, fragments order by depth or peeling.
  A feature that wants to reorder across levels names itself at that
  level's ADR, or it does not exist.
