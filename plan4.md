# Plan 4: the second backend, and a library with mass

Written when plan3's M8 (depth peeling) landed, ADR 0047 closing that
stretch. Two goals drive this plan, set by the project's owner:

1. **A stdlib with mass** — grow `wxsl-stdlib` from its current ~35
   function nodes to something comparable in coverage to the libraries
   people actually ship (Babylon.js's material library, Three.js's
   shader chunks, LYGIA), drawing on those libraries' techniques.
2. **A second backend** — the same WXSL, the same node graphs, the same
   pipeline and scene documents, running interchangeably over **wgpu
   (Rust)** or **Dawn (C++)**. Only the renderer implementation changes.
   This goal is the one that matters more, and the one that reshapes
   architecture.

## Decisions taken before this plan was written

Four questions had real alternatives and were answered up front; every
item below assumes them. Each still owes its ADR where it changes a
recorded decision.

* **Ports from permissive sources are allowed** (supersedes ADR 0007's
  blanket originality rule for the stdlib). Babylon.js is Apache-2.0,
  Three.js is MIT; porting or adapting from them with attribution is
  legally clean, and the reason ADR 0007 chose from-scratch — LYGIA's
  non-permissive license — does not apply. Attribution lands per file
  and in a `NOTICE`; ports are still *re-expressed* in WXSL idiom (one
  `fn` per file, socket defaults in comments), never pasted as foreign
  shader text. S1 is the ADR.
* **The core is shared, not re-implemented.** The device-free half —
  document compilation, the scheduler, uniform-layout computation, the
  WXSL→WGSL compiler — stays in Rust and is exposed to C++ through a
  C ABI. The C++ side implements only the device half: device,
  resources, pipelines, pass recording, presentation. There is one
  scheduler, one layout computer, one compiler; a second implementation
  of any of them is forbidden (guard rail below).
* **Dawn renders; it does not edit.** The backend contract is "same
  language, same graphs, same pipelines, same scenes" — the editor is
  not part of it. The editor and the 2D UI layer stay wgpu-only until
  someone writes the ADR that pays for porting them.
* **WGSL arrives in two stages on the C++ side**: offline
  precompilation first (a Rust tool turns documents + graphs + macro
  bindings into WGSL and layout data the C++ renderer consumes), then
  runtime compilation through the same C ABI once that surface has
  stabilized.

## Feasibility

Both goals are feasible. The measured basis:

**The stdlib is flat work.** The mechanism a bigger library needs
already exists and scales by itself: the file is the node (ADR 0020 —
`build.rs` derives every definition from the `.wxsl` source), the corpus
gate (ADR 0029) and the `graph_to_wgsl` test compile *every* node on
*every* material stage, so coverage grows with the library at zero
marginal test cost. Nothing about doubling or tripling the node count
touches architecture. The cost is authoring time, and the ports-allowed
decision multiplies throughput several-fold. The one thing that needs
watching is the editor's palette at 3–4× its current population (see
S2's risk note).

**The backend split is cheaper than it looks, and the residue is
knowable.** The architecture has been walking toward a second backend
for a whole plan: pipelines are documents compiled by a pure function
(ADR 0033), the render graph's scheduler is pure and tested with no
device (ADR 0021), the WXSL→WGSL half of compilation is a pure function
over text (ADR 0022's worker exists because of it), uniform layouts are
*computed* rather than mirrored (ADR 0023, probe-tested), effects are
data with zero `wgpu` references in `effect.rs` today, and the
capability contract publishes what a setup needs and checks it
device-free (ADR 0044). Dawn consumes WGSL natively — it is the WebGPU
reference implementation — so the compiler's output needs no translation
for the second backend. `wgpu` and Dawn implement the *same*
specification; the differences are API shape and caps, not semantics.

The real cost, measured where it lives:

| What | Measured shape |
|---|---|
| The `wgpu` vocabulary leaking into the device-free half | `pass.rs` 73 refs, `pipeline_doc.rs` 31, `graph.rs`'s descriptor/schedule paths — mostly `TextureFormat`/`TextureUsages`/blend enums in descriptors. B1 replaces them with neutral enums whose serde spellings are the documents' current ones, so no document changes. |
| Extracting the device-free half of `wxsl-render` | `effect.rs`, `swap.rs`, `material.rs`, `library.rs`, `gltf.rs` carry **zero** `wgpu` refs already; `setup.rs` has 3. The extraction is mostly mechanical module moves once the enums are neutral. |
| The C++ device half | A re-implementation, in C++, of what `graph.rs`'s record/`ResourcePool`, `bindings.rs`, `pipeline.rs`'s cache, `renderer.rs`, `mesh.rs` and `gpu.rs` do over `wgpu` — order of 8–10k lines of the 23k in `wxsl-render`, against a stable spec rather than invented behaviour. |
| Keeping the two backends honest | Solved by construction for the core (shared, decision 2) and by test for the pixels: the parity harness (B5), the msdf rule generalized. |

One guard rail of plan2 answers itself here: "a real IR waits for a
second backend that needs one." The second backend has arrived, and it
does **not** need one — because the core is shared rather than
duplicated, the contract between the halves stays what it already is:
versioned JSON documents (ADR 0044), WGSL text, and computed layouts.
No new IR is introduced; the guard rail is reworded rather than
violated.

## Where the plans stand

Carried from plan3 (all still open there):

| Item | What it is | Interplay with this plan |
|---|---|---|
| N6 | the subsurface lighting model | stdlib-adjacent; S2's BRDF batch feeds it the separation approximations it will want |
| N7 | lighting at scale (atlas, cascades, point shadows, light lists) | reshapes frame-group bindings — the C ABI must carry *computed* layouts, never struct mirrors, so N7 can land after B2 without an ABI break (design rule under B2) |
| M10 | relative-to-eye, host half | must land in both backends eventually; it is pure host math, so it lives in the shared half once the split exists |
| M11 | the code editor | wgpu-only, unaffected |
| N9 | the editor catches up | wgpu-only, unaffected; its palette work absorbs the stdlib's growth |
| N10 | WebGL2 is a target | **shares B1's neutral vocabulary** — the ES 3.0 column is a third mapping over the same enums, and naga's wgsl-in/glsl-out hangs off the shared compiler; B1 lands with N10's needs in view even if the port waits |
| N8 | release readiness | the document-format decision now has a second customer: the C ABI ships the same JSON, so "what a shipped application embeds" (Rust *or* C++) is decided once, in B2's window |

## Proposals

Ordered by (payoff ÷ cost), with the B-series and S-series interleaved
by dependency rather than importance — the owner's stated priority is
the backend, so the B-series owns the critical path and the S-series,
being flat, fills every pause (the way plan3 slotted N9 and M11).

### B1 — The device-free frame crate

The precondition for everything else in the B-series: `wxsl-render`
splits in two, and the seam is exactly where the `wgpu` references stop.

* A new crate (working name `wxsl-frame`; the ADR settles it) takes the
  half of `wxsl-render` that needs no device: the pass and resource
  descriptors (`pass.rs`), the scheduler (`RenderGraph::schedule`),
  the pipeline document compiler (`pipeline_doc.rs`), the effect
  descriptors (`effect.rs`), the capability contract (`setup.rs`), and
  the environment's *data* (the values the uniforms carry, not their
  buffers).
* Its enums are **neutral**: texture formats, usages, blend components,
  depth comparisons, limits — with the documents' current serde
  spellings preserved, so every preset, scene and material document
  already written parses unchanged (a test asserts it against the
  preset-parity corpus).
* `wxsl-render` keeps everything that touches a device and maps
  neutral → `wgpu` at its edge. `cargo tree -p wxsl-frame` shows
  `wxsl-core` and nothing else — the same boundary test `wxsl-core`
  and the facade's `--no-default-features` get.
* N10's third column is designed for here: the enum set must be able to
  name what ES 3.0 needs (a `glsl-out` mapping is a later item, but the
  vocabulary must not assume WebGPU-only concerns).

* Cost: medium-large — the largest single refactor in this plan, but
  mechanical after the enums; the scheduler and compilers move, they do
  not change.
* Done when: the full `cargo test --workspace` suite passes with the
  crates split; `graph.rs`'s schedule tests and every preset-parity
  test run against `wxsl-frame` with no `wgpu` in its tree; the
  boundary checks in AGENTS.md gain one line.
* ADR: "The frame's plan is device-free" — amends ADR 0002's crate
  table.

### B2 — The C ABI

**Completed on 2026-10-08** — [ADR 0049](docs/adr/0049-share-the-core-through-a-data-only-c-abi.md).
`wxsl-ffi` exports pipeline compilation/scheduling, raw/effect WXSL→WGSL,
material-stage compilation with computed layouts, and scene/setup checks.
The header is generated from Rust declarations; results own JSON, WGSL,
parameter bytes and field-table views until explicitly freed. Transport and
document ABI mismatches are refused by name. Normal dependencies include
core/frame/lang/stdlib, never a renderer or GUI. Custom effect/model
registrations remain outside this first cut; module overlays and derived
function nodes are supported. See `crates/wxsl-ffi/README.md` for the contract.

Verification: 13 Rust ABI tests, plus dynamically loaded C and C++ harnesses
in `dawn/tests`. Both presets export the shared graph/schedule; all nine
material stages match native Rust WGSL byte-for-byte. Every host-shared
value type is checked in parameter/user/instance layouts, including packed
matrix/vector/boolean offsets, plus effect parameters, ownership, concurrent
calls, malformed inputs, panic containment and named capability/version
refusals. Workspace all-feature tests, clippy, feature builds and dependency
boundaries pass. The Apple M5 headless cube still differs by 0.0002 between
forward and deferred. `bash dawn/tests/run_abi_smoke.sh` runs without a GPU
or Dawn SDK. The Dawn renderer and fixed-layout generation remain B3/B6.

N8's format decision is recorded in ADR 0049: retain versioned JSON and
WXSL for authoring/runtime compilation; offline device-only applications
consume plan JSON, WGSL and layout artifacts without embedding the compiler.

One cdylib exposing the shared core to C++, and the header C++ includes.
The seam carries **data, never behaviour**: versioned documents in,
schedules / WGSL text / layout tables out.

* The surface, first cut: parse and compile a pipeline document to a
  frame plan (`wxsl-frame`'s output serialized); schedule it; compile
  WXSL→WGSL for a stage under a macro binding (the same pure function
  the wgpu side's worker thread runs); compute a material's interface
  and uniform layout (`wxsl_core::resources`, as a table of offsets —
  **computed layouts, not struct shapes**, so N7 and every later
  frame-group change ships without an ABI break); the capability check
  (`RenderSetup::check`) for load-time refusal on the C++ side too.
* JSON at the seam where a document is involved — it is already the
  wire format, already versioned (ADR 0044) — and a compact binary
  (WGSL text + layout tables) where JSON would be silly. N8's "what a
  shipped application embeds" decision lands in this item's window,
  because the C ABI is the second customer that forces it.
* The header is generated (cbindgen or a small generator over the same
  tables), not hand-written — `environment.rs`'s `#[repr(C)]` structs
  are the precedent for what hand-mirrors cost; B6 finishes that job.
* The ABI itself carries a version, refused by name on mismatch, the
  same rule the documents already follow.

* Cost: medium — the surface is small because the core already speaks
  in documents and tables; the work is boundary discipline and tests.
* Done when: a C test harness (in the B3 tree) loads a preset, gets its
  schedule and its WGSL, and refuses a newer `abi` version by name; the
  same WXSL text comes out for the same inputs as the Rust side's
  `variants` path — asserted by a test on both sides of the boundary.
* ADR: "The core is shared across backends through a C ABI" — records
  decision 2 and the data-not-behaviour rule.

### B3 — The Dawn renderer

**Implemented (offline/headless, 2026-10-09).** `dawn/` builds with Conan 2
and a SHA-256-pinned official Dawn SDK. `bake_docs` exports both shared
stock plans, layouts, WGSL and upload bytes. PBR and scene_check match Rust
in both paths on Apple M5; buffer/history/indirect probes agree too. The
binary links no Rust. See ADR 0051 and `dawn/README.md` for commands and
the host's explicit limits. Runtime compilation and the wider parity/CI GPU
harness are provided by B4 and B5 below.

The C++ half. Offline-first, per decision 4.

* A top-level `dawn/` directory (CMake, pinned Dawn version), not a
  crate: `wxsl_render_cpp` is a library, `pbr_cube_cpp` the demo that
  renders the *same* `pbr_cube.wxsl.json` and `scene_check.scene.json`
  documents the Rust demo uses.
* It implements: device and surface (or headless) setup; a resource
  pool honouring `wxsl-frame`'s schedule — transient reuse, history
  rings, never-aliased buffers — reading the schedule the C ABI hands
  it rather than re-deriving anything; pipeline layouts and pipelines
  from the neutral descriptors; pass recording with the computed pass
  bind-group layout; the draw list and mesh upload (vertex format
  tables mirrored from `wxsl-render`'s, generated or test-pinned);
  uniform uploads through B6's generated headers.
* WGSL arrives **offline** first: a small Rust tool (`wxsl-bake-docs`
  or a flag on an existing example) turns a material + stage + macro
  binding into WGSL files plus a layout manifest the demo loads. The
  variant cache's keying logic is trivial at this stage (one config);
  it is not a re-implementation of the cache, it is the absence of one.
* Capability honesty: float blending (M8's native peel path), timestamp
  queries, limits — read from the adapter and reported through the same
  capability vocabulary, so a preset that cannot run says so, exactly
  as the wgpu side does.

* Cost: large — the biggest single item in this plan, but against a
  fixed specification with the schedule and layouts handed over
  finished.
* Done when: `pbr_cube_cpp --headless` writes a PNG within the parity
  tolerance of the Rust `--headless` run (B5 sets the number), from the
  same documents, with no Rust linked into the binary beyond the
  precompile tool's outputs.
* ADR: "The second backend implements the device half only" — records
  decision 3's boundary and what the C++ side is forbidden from
  re-deriving.

### B5 — The parity harness

**Implemented (2026-10-09).** `dawn/tests/run_parity.sh` consumes one curated
scene list: PBR, scene_check, the gallery's exported single-pass document, and
rendered samples for all ten stdlib categories, including graph-authored FXAA.
It uses ADR 0051's unchanged comparator and negative recording/upload guards.
CI has Linux/macOS compile jobs and an opt-in trusted `wxsl-gpu` runner job;
the remote GPU runner must be provisioned separately.

The msdf rule, generalized to backends: **every backend owes the
harness a green run per shipped preset** — the rule plan3 wrote for
second implementations, now with its widest customer.

* One scene list (the gallery's, or a curated subset), rendered by both
  backends headless, images compared with a stated tolerance. The
  same-GPU/different-API comparison will not be bit-exact; the
  tolerance is chosen once, honestly, from the forward/deferred
  comparison's precedent and the actual distribution of differences —
  and recorded in the ADR with its rationale, not tuned until green.
* The corpus half: the stdlib corpus gate (every node, every stage)
  already compiles for WGSL; the harness adds a *rendered* sample per
  category through both backends, so a node whose math diverges under
  one backend is a red image, not a shipped surprise.
* Runs only where an adapter exists, like every GPU test here; a CI job
  compiles both backends everywhere and runs the harness on GPU
  machines.

* Cost: small-medium; mostly plumbing, and it pays for every later
  item's honesty.
* Done when: CI renders `pbr_cube` (and one gallery demo) through
  wgpu and Dawn and diffs them; a deliberately-injected C++ recording
  bug turns it red (the harness's own acceptance test).
* ADR: none new — it *is* the ADR 0047-era rule, applied; recorded in
  B3's ADR as the acceptance instrument.

### B4 — Runtime compilation through the shared C ABI

**Implemented (headless, synchronous, 2026-10-09).** The optional C++
`Compiler` loads `wxsl-ffi`, checks versions, copies owned results and caches
identical requests. Runtime mode checks the bundle's scene/setup, compiles its
plans and shaders without offline WGSL, and consumes Rust's layout signatures
and variant keys. `Renderer::compile_material` replaces compatible materials
transactionally; parameter-byte uploads compile nothing. Named compile/layout
failures retain the last valid image. Layout, host geometry, draw selection or
pipeline-configuration changes require re-exporting host data. Offline binaries
still need no Rust deployment. Worker queues/window/editor integration remain
outside this cut. See ADR 0052 and `dawn/README.md`.

### B6 — Generated host-shared headers

**Implemented (2026-10-09).** `wxsl-core::host` generates fixed frame,
vertex/UI and MSDF Rust/C/WXSL definitions (ADR 0050). Generated sizes and
offsets are asserted; the checked-in header is test-pinned and compiled in
C/C++. A stale header is refused before Dawn setup. Rust GPU layout probes
and the Dawn/Rust scenes pass; an injected bad upload turns parity red.

`environment.rs`'s `#[repr(C)]` structs are edited together with
`shaders/wxsl/bindings.wxsl` today — a three-way mirror about to become
four-way (C++). The fix is the one the parameter buffer already got:
**the mirror becomes a table**.

* The struct layouts are declared once (a table beside `abi`, or
  derived from the same declarations the WXSL is generated from), and
  everything else is generated: the Rust struct, the C header, and the
  WGSL's uniform blocks. The ABI's "two halves edited together" rule
  becomes "one table, three generated halves".
* The instance-row and parameter layouts are already computed
  (`wxsl_core::resources`); this item extends that treatment to the
  fixed host-shared structs, so every host-shared buffer in the repo is
  either computed or generated — none hand-mirrored.

* Cost: small; the probe-test infrastructure (`tests/probe`) already
  validates computed layouts on device, and it validates the generated
  headers the same way.
* Done when: editing a field in the table changes the Rust struct, the
  C header and the WGSL in one commit, and the probe tests pass on both
  backends; a deliberate mismatch is caught by test, not by a garbled
  uniform.
* ADR: "Host-shared layouts are computed or generated, never mirrored"
  — amends the ABI's edit-together rule (ADR 0008/0021's convention).

### S1 — The licensing ADR *(unblocks S2–S4)*

**Implemented (2026-10-09).** ADR 0053 replaces only ADR 0007's blanket
originality rule. Audited MIT/Apache-2.0/BSD ports retain pinned provenance,
license text and notices. The first worked port is three.js `IBLSheenBRDF`,
exposed as `lighting.sheen_ibl_response`; root/packaged notices and a public
notice string support redistribution. `refs/` is ignored research material.
The detailed source inventory, proposed tickets and validated WXSL snippets
for S2/S3/S4 live in `docs/s2-s3-s4-implementation-guide.md`; those lots are
not implemented by writing the guide.

Decision 1, written down: supersede ADR 0007's blanket originality rule
for `wxsl-stdlib`.

* Allowed sources: OSI-permissive licenses (MIT, Apache-2.0, BSD) —
  the license file is checked before porting starts, not after.
  Anything else (copyleft, custom, unclear) stays under the original
  rule: technique only, re-derived.
* Attribution: a source line in the file's leading comment block (what
  came from where, which revision) plus an entry in a top-level
  `NOTICE`. Apache-2.0's terms are met by carrying the notice; state
  changes plainly.
* The craft rule survives the licensing change: a port is
  *re-expressed* in WXSL idiom — one `fn` per file, socket defaults as
  `// … @default` comments, `@macro const` for compile-time knobs —
  because the derivation pipeline (ADR 0020) and the corpus gate do not
  know what a foreign shader chunk is. If a technique cannot survive
  re-expression, it is a sign the *graph vocabulary* is missing
  something, which is itself a finding worth an item.

* Cost: days — an ADR, a NOTICE, and the first ported function as the
  worked example.
* Done when: one Babylon- or Three-derived function ships with
  attribution, passes the corpus gate and appears in the editor's
  palette; `shaders/README.md` and AGENTS.md's licensing section say
  the new rule.
* ADR: "Permissive sources may be ported, with attribution" —
  supersedes the originality half of ADR 0007 (its "why not LYGIA"
  reasoning stands untouched).

### S2 — The surface batch

The stdlib's first growth spurt, aimed at what a *material* wants.
Everything here is surface-domain nodes, corpus-gated the day it lands.

* **BRDF variants** beyond the shipped PBR: anisotropy, sheen, cloth,
  iridescence — Three's and Babylon's material libraries are the
  technique sources; each is a lighting-side function where it can be
  (feeding N6's model) and a node where it is a surface term.
* **Geometry anti-aliasing**: the `fwidth`-based coverage and normal
  feathering family; SDF nodes get their AA companions here too.
* **Colour**: OKLab/OKLCh conversions, the common tonemap operators
  (ACES, AgX, Reinhard-Jodie) as *nodes* so a screen graph can chain
  them before the shipped tonemap effect or replace it.
* **SDF operations**: the missing operator half of `sdf/` — union,
  subtract, intersect with smooth/round/onion/chamfer variants, 2D
  shape primitives — the category exists with three lonely files.

* Cost: flat and parallelizable — the ideal pause-filler between B1
  and B3's long stretches.
* Done when: a gallery demo shows a sheen or anisotropic material; an
  SDF blob anti-aliases; the palette's node count crosses 100 with the
  corpus gate green.
* ADR: none per function (the mechanism is ADR 0020's); the palette
  consequences, if any, are N9's.

**Mostly landed on 2026-10-09**, following the guide's ticket order.
The palette is at 171 nodes; every one compiles through the real
compiler on every material stage (`graph_to_wgsl`), and the corpus gate
is green. What shipped:

* **SDF** (sdf/): the operator half — `union`, `intersection`,
  `subtract`, `onion`, `round`, `smooth_intersection`,
  `chamfer_union` — the 2D shapes (`circle`, `rounded_box`, `capsule`,
  `hexagon`, `equilateral_triangle`, the last two from Inigo Quilez's
  articles as technique), 3D `torus` and `cylinder`, and `coverage`,
  the anti-aliased alpha. Coverage takes its `width` from the caller,
  which keeps the node domain-agnostic — the fragment-only derivative
  contract (`roughness_aa`'s blocker) is *not* shipped and stays queued
  below.
* **Colour** (color/): OKLab↔linear and OKLab↔OKLCh (hue in turns, the
  library's phase convention; Ottosson's definition as technique,
  original dot-product translation), `tonemap_reinhard_jodie`, and
  `tonemap_aces` — the full three.js fit, ported under ADR 0053's rule
  (MIT, NOTICE updated). `bloom_threshold` (the soft-knee extract)
  lives here too rather than in filter/: it is a pure colour
  operation, no image.
* **BRDFs** (lighting/): `charlie_distribution` and
  `visibility_neubelt` (original implementations of the Estevez–Kulla
  forms), `distribution_ggx_anisotropic` (the guide's half-vector-in-
  tangent-frame form), `visibility_ggx_anisotropic` (three.js port),
  `anisotropic_widths` (the roughness+anisotropy → alpha-pair
  convention; signed anisotropy deliberately left to an authoring
  layer), `ior_to_f0`/`f0_to_ior`, `thin_film_phase` — the phase term
  only, with the doc saying honestly what a complete iridescence still
  owes.
* **The cloth model** (`wxsl.cloth`, id 4) — S2-D's first increment,
  in the clearcoat shape: a roughness-wrapped diffuse plus a
  Charlie-sheen layer that takes its energy out of the diffuse rather
  than adding on top. No G-buffer request (`extra: None`), so a
  single-model set is unchanged in shape and the forward/deferred
  parity tests cover it by iterating `DEFAULT_MODELS`. The gallery
  gains the `cloth` demo — the mechanism cost: `Demo` grew a `model`
  field, entering the demo widens the renderer's lighting set to
  `default_set()` and leaving restores the stock one, since a material
  may only name a model its set carries.

Not yet, deliberately: full spectral iridescence (the Khronos port is
queued and costed — helpers must inline into one function), AgX (it is
gamut matrices, a log domain, contrast and looks — a curve named AgX
would be the dishonest thing the guide warns about), roughness AA
(needs the fragment-only stage contract — queued as plan5's D1, with
the forward-shaded escape hatch as D2), and the surface-schema half
of S2-D (per-fragment sheen tint/thickness as real channels).

### S3 — The world batch

What surrounds the surface:

* **Sky and atmosphere**: Rayleigh/Mie single-scattering (the technique
  behind Three's `Sky`), aerial perspective, height and fog variants —
  as screen or surface nodes plus the environment hookup the IBL work
  (ADR 0039) left open: an equirect → cube prefilter and irradiance
  pipeline, which the BRDF-LUT's `once`-bake machinery already knows
  how to run.
* **Noise, completed**: simplex, Worley/cellular, tiled and hash-based
  variants — `generative/` has three files and PBR materials want ten.
* **Procedural textures**: brick, checker, grid, truchet — the staples
  every library carries and every demo wants.

* Cost: flat; the atmosphere and prefilter halves are the real work
  (medium), the rest is authoring.
* Done when: the `ibl` gallery demo renders against a procedural sky
  rather than an approximation; a Worley-driven material compiles on
  every stage through the corpus gate.
* ADR: the prefilter pipeline touches frame-group bindings — it
  follows the computed-layouts rule (B2) and lands as its own small
  decision.

**Partly landed on 2026-10-09.** What shipped:

* **Noise** (generative/): `simplex2` and `simplex3` — the Ashima/
  Gustavson noise, ported under the permissive-ports rule (MIT, NOTICE
  updated, helpers inlined because a node source is one function) — and
  `worley3` (F1/F2, integer-hashed feature points jittered at most a
  quarter cell, the 27-cell neighbourhood documented as the F2
  approximation it is).
* **Procedural textures** (generative/): `checker`, `grid_mask`,
  `brick_mask`, `truchet_arcs` (the tile choice hashed from the cell
  index, so it is stable under camera movement and identical across
  backends).
* **Fog and phases** (lighting/): `fog_transmittance`,
  `fog_composite`, `height_fog_depth` (the analytic exponential-height
  integral), `rayleigh_phase`, `hg_phase` — the media vocabulary, all
  linear-radiance-honest.
* **Space and math**: `direction_to_equirect` (space/) and
  `ray_sphere` (math/) — the primitives the sky and IBL work stands on.

Not yet: the single-scattering sky itself, and the IBL prefilter
pipeline — both named in the guide as ADR-bearing (bindings, host HDR
imports, per-roughness mips), so they stay queued rather than half-
landed. The phases and ray-sphere primitives above are their
groundwork, exactly as the guide ordered.

### S4 — The screen batch

Post effects as screen-domain graphs, per the rule that a new post
effect is a graph first (ADR 0040):

* The **blur family** — separable Gaussian, box, Kawase — as screen
  graphs, which finally gives bloom the multi-pass pyramid plan3
  deferred twice (the sub-document synthesis question); a pyramid is
  the consumer that forces it.
* DOF, chromatic aberration, vignette, film grain — the cheap,
  high-visibility tail.
* SSR waits for N7's lighting scale and TAA's stability, and says so
  here rather than in a console.

* Cost: medium — the effects are small; the pyramid's scoped
  sub-documents are the real design work.
* Done when: bloom's chain is a real pyramid authored as documents, and
  one S4 effect is in a shipped preset.
* ADR: the sub-document synthesis, if it lands here, gets its own —
  plan3 queued it twice already.

**Nodes and effects landed on 2026-10-09; the pyramid did not.** What
shipped:

* **Filter nodes** (filter/), sampler-free over `textureLoad` with
  clamped coordinates — the pass group binds no sampler, which is what
  keeps every one of these usable inside a screen graph exactly as
  `fxaa` is: `blur_box` (radius a macro — a loop bound), `blur_gaussian`
  (one axis per pass, `axis` in texels so a chain separates it),
  `blur_kawase` (one pass of the growing-offset sequence),
  `downsample2`, `vignette`, `film_grain` (deterministic integer hash
  over pixel + frame time — identical across backends, which is the
  whole reason not to reach for a `sin` hash), `chromatic_aberration`.
* **Three shipped screen effects** in `wxsl::effects`:
  `wxsl.vignette`, `wxsl.film_grain`, `wxsl.chromatic_aberration` —
  graphs, in the tonemap/fxaa shape, so the pipeline canvas's palette
  grows them for free. They carry no parameters (a screen graph cannot
  declare any — the restriction codegen documents), so their knobs live
  on the *nodes*, which an authored chain tunes per instance; the
  shipped graphs wire the defaults.
* The gallery's `fxaa` demo already exercises the mechanism all three
  ride; they compile in the facade's tests the same way.

Not yet: the pyramid (its sizes want the `Extent` vocabulary extended
before the document can say per-level targets, and the synthesis is
the ADR plan3 queued twice), and DOF's near/far passes. SSR stays
queued behind N7/TAA exactly as written.

## Suggested order

**B1 completed on 2026-10-07** (ADR 0048). `wxsl-frame` owns the neutral
pass/resource vocabulary, scheduler, pipeline compiler/config, presets,
effects, capability contract and environment data/host layouts. Its only
workspace dependency is `wxsl-core`; CI checks the no-GPU/no-WXSL-compiler
boundary. wgpu allocation, recording and exhaustive type mappings stay in
`wxsl-render`, which re-exports or adapts shared APIs.

Verification: workspace all-feature tests (including GPU tests on Apple M5),
clippy, formatting, headless/editor feature builds and dependency checks.
The frame crate has 94 passing tests (one preset-writing helper ignored),
including the scheduler and preset-parity corpus. Compute-written indirect
arguments render identically to a direct draw. `plan_frame` compiles and
schedules both presets without a device; the headless cube's mean
forward/deferred difference remains 0.0002.

1. **B1** — the split. Everything else in the B-series depends on it,
   and it is the only item whose cost is uncertain enough to want
   started first. **S1** lands in the same window (days, independent).
2. **B2 — completed** — the C ABI, minimal (documents, schedule, WGSL,
   layouts, check), with N8's format decision recorded in ADR 0049.
3. **B3 + B6** — the Dawn renderer and the generated headers, offline
   first. The long stretch; **S2** fills its pauses.
4. **B5** — the parity harness green on `pbr_cube`, then widened. From
   here on, no backend change lands without its parity run.
5. **B4** — runtime compilation on the C++ side, once the ABI has had a
   customer for a stretch and its shape is trusted. **S3** fills this
   window.
6. **S4**, then the plan3 leftovers in plan3's own order: **N6**, **N7**
   (its binding reshapes ship safely through the computed-layouts
   rule), **M10** (now one fix, in the shared half), **M11**, **N9**,
   **N10** (cheaper now — B1's vocabulary is its first half), **N8**'s
   remainder.

**S2/S3/S4 status, 2026-10-09**: S1 landed earlier (ADR 0053); the
node-and-effect bulk of S2 (SDF, colour, BRDFs, the cloth model —
S2-D's first increment), S3 (noise, procedural textures, fog, phases,
the ray/equirect primitives) and S4 (the blur family, vignette, grain,
chromatic aberration, as nodes and as shipped screen graphs) landed as
described in the proposal sections above. Still open from the guide's
twelve tickets: the fragment-only derivative contract (ticket 2),
full iridescence (6), the single-scattering sky and the IBL ADR
(8–9), the pyramid synthesis ADR and DOF (11–12). The palette is at
171 nodes; N9's palette reorganization trigger has fired.
The derivative contract and its consequences moved to their own plan —
**plan5** (forward-shaded materials, ordered transparency,
`max_layers`) owns ticket 2 from here.

The B-series owns the critical path because the owner's priority is the
backend; the S-series, being flat and unblocked, is what keeps every
pause productive — the same shape plan3 gave N9 and M11.

## Risks, and how each is de-risked

| Risk | Mitigation |
|---|---|
| **The neutral vocabulary drifts from `wgpu`'s** — a mapping gap found mid-render. | The mapping table is total and tested: every neutral enum round-trips through `wgpu` (and the preset-parity corpus pins the serde spellings). A gap is a compile error in the mapping, not a wrong pixel. |
| **The C ABI ossifies wrong** — an early shape that N7 or N10 later breaks. | The seam carries data, never behaviour, and *computed* layouts rather than struct mirrors; documents at the seam are already versioned. The ABI version rule mirrors ADR 0044's. |
| **The two device halves drift** — same documents, different pictures. | The core cannot drift (shared, decision 2). The device halves are pinned by B5's parity harness per shipped preset, with a tolerance chosen and justified once, up front. |
| **Dawn churn / platform surfaces** — a moving dependency and per-platform windowing. | Pin the Dawn version; consume it through its `webgpu.h` surface; the demo starts headless (the readback path both GPU tests and the harness need anyway) and gains windows after parity. |
| **Capability divergence** — float blending, timestamp queries, limits differ between backends. | The capability contract already exists device-free (ADR 0044); both backends publish into the same vocabulary and the same named-refusal path. M8's native/baseline split is the precedent per feature. |
| **Ports rot the library's shape** — pasted chunks that fit no palette and no graph. | S1's craft rule: ports are re-expressed in WXSL idiom or not taken; the corpus gate and the every-node compilation test are the mechanical backstop; the NOTICE and per-file source lines keep provenance auditable. |
| **The palette drowns** — 300 nodes in one search list. | N9's catch-up owns the palette's organization; the trigger is S2 crossing ~100 nodes. Category-aware search is a small, known editor item — queued there, not solved here. |
| **C++ build debt** — CMake, a second toolchain, CI matrix growth. | One directory, one pinned dependency, one demo binary until B5 demands more; the CI job compiles everywhere but runs the harness only on GPU machines, the same split every GPU test here already makes. |
| **Scope creep toward porting the editor** — the tempting next step after B3 works. | Decision 3 is a guard rail: the editor is wgpu-only until an ADR pays for the port. plan4 does not contain that ADR. |

## Guard rails

Carried unchanged from plan3 — they are what kept it honest:

* No GUI toolkit; the editor still draws itself with the renderer.
* One graph model — a new domain or canvas reuses `wxsl-core`'s, never
  forks it.
* The scheduler stays pure and device-free; documents compile to the
  frame plan, and everything checkable is still checked there.
* Generation stays *logic in Rust, text in files*.
* Analyses run on the graph, never on generated text.
* New document fields default to today's behaviour.
* Every proposal lands with an ADR and a runnable demo.
* An ADR's "waits for its first consumer" is a queue entry, not a
  graveyard.
* Two implementations owe an agreement test before they land.

Rewritten by the second backend's arrival, and three added:

* **"A real IR waits for a second backend that needs one"** — answered:
  it does not need one. The shared core plus versioned documents plus
  WGSL text *is* the contract; no IR is introduced to connect Rust to
  C++, and the burden of proof now sits on anyone proposing one.
* **The C++ side never re-derives what the core computes** — schedule,
  layouts, dispatch, capability checks come through the C ABI. A second
  implementation of a core rule is a bug, not a port.
* **Host-shared layouts are computed or generated, never mirrored** —
  B6's rule, applying retroactively to the last hand-mirrored structs
  in the repo.
