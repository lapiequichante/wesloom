# 0045. A bake is an effect over a material subgraph

Date: 2026-09-30

Status: Accepted

Implements plan3's N5 (plan.md's M9). The mechanism pre-existed twice —
"an effect over a material subgraph" is the screen graph of
[ADR 0040](0040-screen-domain-graphs-postprocess-is-a-material-over-the-frame.md)
pointed at a material's own cone, and the invalidation rule is
[ADR 0035](0035-execution-policies.md)'s policies whole. What was new is
the generation and the stand-in, and where the texture lives.

## Context

plan.md's bar for a bake: *an expensive noise-driven PBR term bakes to a
texture, and toggling the bake changes cost but not image*. Nothing in the
workspace could say what a bake even was. The two halves it needs live on
opposite sides of a boundary nothing had ever crossed:

* A material's value is computed **per fragment, in the material's own
  bind group** (group 1). A pass list's textures live in the pool, keyed
  to slots and generations. There was no way for a material to sample
  anything a pass had written — the shadow maps and the environment LUT
  cross in the *other* direction (graph → frame group), and both are
  global tables, not per-material ones.
* A pass list's shaders come from effects — files, or graphs in the
  screen domain. Nothing generated a shader from *part of a material*.

The M9 sketch — "a `Bake` pass node, a UV-space target, an invalidation
rule" — predates effects being data and policies existing. With them in
hand, the only honest questions left were: where does the declaration
live, who owns the texture, and how does the toggle switch arms without
an edit.

## Decision

### The declaration is graph-level, about a node

`Graph::bakes: Vec<BakeDecl>` — "bake *this node's* output through this
table". The declaration carries the node id, which output socket (default:
the first), the table's **texture name** (its identity everywhere: the
`@group(1)` variable, the `resource.color` label, the renderer import, the
host's binding), a fixed pixel size, and a precision (`standard` or `hdr`
— the two that can be written through storage). It is a claim *about a
node*, not a node of its own: the graph reads exactly as it would
unbaked, validation reports a broken declaration beside every other
broken declaration, and the serialized format gains one field that is
absent in every file written so far.

The node keeps its own semantics. What the declaration changes is which
of two ways its value reaches the surface.

### The bake effect is generated from the cone, at registration

`Effect::from_bake(id, …, &graph, texture_name, macros, registry)` runs
`codegen::generate_bake`: the subgraph feeding the declared node — the
node itself included — compiles into one `fn wxsl_bake_value(uv: vec2f)
-> vec4f` and a compute entry that fills the table, one dispatch at
`@workgroup_size(8, 8)`, guarded by the table's own extent. The effect is
an ordinary compute effect from there: one declared output (wired from a
`resource.color` the document labels with the texture name), a
`pass.compute.<id>` node, a `policy` setting. Documents need no new
vocabulary for the bake itself — only for the target (below).

Purity is checked at generation, so an impossible bake is a failure at
registration rather than at the first frame: the cone may read
`input.uv` — that *is* the domain, and becomes the function's parameter —
and nothing else. No world position, no time, no attributes, no material
parameters, no textures. A bake is a pure function of where it is
sampled; that is the whole of what makes sampling it back equal computing
it. (A bake whose *content* would change on re-bake — time-driven noise —
waits for the frame group in compute, ADR 0035's standing note, and is
not spent here.)

### The texture is the material's; the graph borrows it

The table is **host-owned**. A scene (or demo, or test) creates it —
size and precision come from the declaration, the one place they live —
binds it into the material's group 1 like any ADR 0023 texture, and
hands the view to the renderer with
`Renderer::import_resource(label, view)`. The pipeline's `resource.color`
gains `imported: true`: the target is declared, not allocated, and the
compiler refuses to also size it — whoever created the texture sized it.
A policy'd pass writing an imported target is exempt from the
stable-storage rule (the frame's own target stays subject to it): the
rule exists so a skipped pass leaves its last write on a texture that
cannot have moved, and a host-owned texture cannot move — the purpose
holds by ownership rather than by the pool's ring.

This is the join the two documents make by name: material declaration
`roughness_bake`, resource label `roughness_bake`, import label
`roughness_bake`. A view nothing declares is `RenderError::UnknownResource`;
a declared import nobody supplied is the existing `MissingImport` — the
mismatch says which name disagrees.

### The toggle is a material-configuration field, and one arm leaves the module

`MaterialConfig.bakes: bool` (default on — a declaration expresses the
intent), carried through `resolve` like every other knob (ADR 0038). When
it is on, `generate` resolves the declarations into stand-ins: the
fragment partitions stop at the declared nodes, binding their output to
one `textureSample(texture, texture_sampler, ctx.uv)` (swizzled to the
value's width), and the cone is **not emitted at all** — not
`@if`-gated, emitted. The baked arm's module is smaller by the whole
subgraph, which is what "changes cost" has to mean. When it is off, the
graph generates exactly as it always did. Both arms compile from the one
document; toggling costs one material recompile and no edit.

Two soundness rules fall out of the stand-in, both checked at generation
with named errors:

* **The cone belongs to the bake alone.** A node inside the cone that
  also feeds something outside it would go dark when the bake stands in —
  refused, naming both nodes. (Sharing *between two bakes* is fine; each
  table recomputes what it needs.)
* **The vertex stage may not reach the baked node.** `textureSample`
  does not run there, so there is no stand-in to emit.

The sampler is the declaration's too, named `{texture}_sampler` — one
per table, clamp-to-edge semantics, because the table covers exactly the
0..1 domain.

### Authoring order is part of the contract

The material samples the table through its own bind group — a dependency
the scheduler cannot see. Declaration order is its tie-break, so a bake
pass must be declared **before** the pass that samples it, and both the
shipped demo and the test write their documents that way. The alternative
— teaching the scheduler about material bind groups — would put the
graph model inside the scheduler for one ordering convenience.

## Alternatives considered

**A dedicated `bake.sample` node body** in the material graph, carrying
uv/value sockets and the table's settings. It reads well on a canvas and
it is what plan.md's sketch said — but the node it *replaces* is the
author's own expensive term, and making "bake the fbm node" take a new
kind of node means re-parenting the cone under it, doubling the authoring
for the inline arm. A declaration about an existing node keeps the graph
identical under both arms — which is what "toggling costs no edit"
actually demands. The palette can still show declarations later
(N9-class work); nothing about the data model changes to earn it.

**The table lives in the pool as a persistent resource, the material
borrows a view.** The ownership goes the other way: every pool
reallocation (a window resize anywhere in the pass list) would recreate
the table and silently invalidate every material bind group holding a
view of it — per-frame bind-group patching for all materials, exactly the
churn the stable-storage rule exists to avoid. The shadow maps can do it
because they are re-bound every frame from the frame group; a material's
group 1 is not.

**The table in the frame group, like the environment LUT.** The LUT is
one global table; a bake is per material. Growing the frame group's ABI
per bake is a per-material fact in a shared group, and two baked
materials would need two ABI slots. Group 1 is where per-material
textures already live.

**Emit both arms under `@if` and let the macro switch.** The toggle would
not need a codegen branch — but the cone's `let`s would still be *in the
material source* and their removal would be the backend's dead-code pass
to notice. "Changes cost" is the generator's claim to make, not the
optimizer's to grant.

**SceneResources creates the table automatically** when a loaded
material declares one. Still true and desirable — but it is a convenience
on top of the primitive, and the primitive is what this item lands. The
demo shows the six lines an application (or the loader, later) writes.

## Consequences

* The done-when holds on device: `bake_passes.rs` renders the toggle's
  two arms under the same lights and compares — mean difference 0.0016,
  the cost of sampling a continuous function at texel centres and
  nothing more — and a `once` bake's run count stays at 1 across frames
  while an `on demand` one sleeps, runs on `mark_pass`, and stops. The
  corpus of device-free tests covers the declaration's checks, the
  stand-in's emission, the cone's purity and sharing rules, the dispatch
  shape, and the imported target in the document vocabulary.
* The gallery gains `bake`: the deferred chain with the bake pass first,
  a twelve-octave noise term on a metal cube, and
  `assets/bake_term.wxsl.json` as the copyable material document — the
  same move `pbr_cube.wxsl.json` made for the node format.
* The variant cache needed nothing: the two arms are two sources, and the
  source hash is already the key's spine. A group holding an import is
  built only when its pass is due, so the once-bake builds one bind group
  in its life.
* A bake pass written *after* the pass that samples it feeds it an empty
  table for one frame — the scheduler cannot see the material's sample.
  Named here as authoring order; the editor's pipeline canvas could badge
  it later, and the capability check (`RenderSetup::check`, ADR 0044) is
  the natural home for a declaration/resource join check once it can see
  material graphs.
* `EffectShader::Graph` gained the module path it mounts at: the screen
  mount is `codegen::SCREEN_MODULE` (nominal — the generated module
  *imports* the ABI's `abi::SCREEN_MODULE`, and mounting the root there
  made it import itself), the bake's is `abi::BAKE_MODULE`.
* A compute effect that wants its content to *change* — time in the
  cone, a parameter — is still waiting for the frame group in compute
  (ADR 0035's note, unchanged). Until then, `on demand` bakes re-run a
  pure function, which is idempotent and therefore cheap to prove.
* If this changes, also update `AGENTS.md` (conventions and the gallery
  list), `docs/architecture.md`, and the glossary's *Bake* entry.
