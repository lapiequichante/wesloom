# Architecture map

This is the map; the ADRs in `docs/adr/` are the territory's legal record.
When the two disagree, trust the ADRs and fix this file.

## Vision, one paragraph

`wxsl` lets you build a material/shader as a node graph (visually, via
`wxsl-editor`, or programmatically against `wxsl-core` directly),
compiles that graph to [WXSL](https://wesl-lang.dev), and runs it through a
`wgpu` renderer (`wxsl-render`) that can switch between forward and
deferred rendering without you maintaining two graphs. An original,
from-scratch library of granular base nodes (`wxsl-stdlib`) — math,
color, lighting, SDFs, noise, and so on, in the spirit of libraries like
[LYGIA](https://lygia.xyz) but not derived from one — ships as part of the
default build (ADR 0007).

## Crate graph

```mermaid
graph LR
    core["wxsl-core<br/>(graph model + WXSL codegen)<br/>no wgpu, no GUI"]
    lang["wxsl-lang<br/>(WXSL compiler:<br/>parser + WGSL backend)"]
    render["wxsl-render<br/>(wgpu pipelines,<br/>forward/deferred switching)"]
    editor["wxsl-editor<br/>(visual node editor,<br/>draws itself with wxsl-render)"]
    stdlib["wxsl-stdlib<br/>(original base nodes)<br/>MIT/Apache-2.0"]
    facade["wxsl<br/>(facade crate, feature-gated re-exports)"]

    render --> core
    render --> lang
    editor --> core
    editor --> render
    editor --> lang
    lang --> core
    stdlib --> core
    stdlib -. "build only" .-> lang
    facade -. "render feature (default)" .-> render
    facade -. "editor feature" .-> editor
    facade -. "stdlib feature (default)" .-> stdlib
    facade --> core
```

Arrows point from dependent to dependency. The only crate every build
includes is `wxsl-core`. See
[ADR 0002](adr/0002-cargo-workspace-crate-boundaries.md) for why the split
exists and which edges must never appear, and
[ADR 0016](adr/0016-syntax-highlighting-reuses-wxsl-langs-lexer.md) for why
`editor --> lang` exists (the code panels' syntax highlighting reuses the
compiler's own lexer).

Two of those edges are about node definitions being read out of shader
sources ([ADR 0020](adr/0020-a-node-definition-is-derived-from-its-wxsl-source.md)).
`lang --> core` is there because the derivation answers in the graph model's
terms — it hands back a `NodeDefinition`. `stdlib -. build only .-> lang` is
dashed for a reason worth knowing: the compiler is a **build**-dependency of
the node library, so the library derives its nodes at build time and the
shipped crate carries no compiler. `cargo tree -p wxsl-stdlib -e normal`
shows `wxsl-core` and nothing else.

## Module map

What lives where, now that the crates have contents. Each module's own doc
comment is the detailed version.

**`wxsl-core`** — `node` (value types, sockets, `WeslFunction`
descriptors, `NodeDefinition`, the registry), `graph` (nodes, edges,
validation, traversal, the serialized node format), `codegen` (graph → WXSL,
plus the generated macro module), `macros` (macro variables), `abi` (the
shader ABI's names and field tables), `lighting` (the lighting-model
registry, and the shading function, G-buffer pack and lighting pass
generated from a set of models), `stages` (the stage analysis, plan2 P9),
`pipeline` (the pipeline *document* vocabulary, plan2 P3 — a different
registry over the same graph model), `template` (strict hole-filling for
the generated shader text), `scene` (the scene document), `wesl`
(identifier/float/hash helpers), `error`.

**`wxsl-stdlib`** — `shaders` (the embedded `.wxsl` sources, keyed by
module path), `registry` (the operators as node definitions, plus the
function nodes `build.rs` derived from the sources).

**`wxsl-render`** — `pipeline` (`StockPipeline`, the preset loader, the
`wgpu` pipeline cache), `pipeline_doc` (the pipeline compiler: document →
`RenderGraph`, plan2 P3), `library` (`ShaderLibrary`),
`material` (a graph compiled to WXSL), `variants` (WXSL → WGSL and the
variant cache), `renderer` (the front end that hides the path switch), `scene` (camera,
lights, uniform layouts), `mesh` (vertex format, cube, sphere, plane, torus),
`gpu` (device setup, offscreen rendering and readback), `error`, and `ui` —
the 2D layer with nothing to do with materials (ADR 0013): `draw` (the
instanced primitive and the draw list), `atlas` (the shared glyph and image
texture), `msdf` (distance fields from outlines, on the CPU), `msdf_gpu` (the
same as a compute pass, and the backend switch), `font` (outlines and metrics
from bytes the application supplies), `text` (the glyph cache and shaping),
`input` (windowing-agnostic events), `renderer` (the UI pass).

**`wxsl-editor`** — `app` (the `Editor`: panels, frame, shortcuts), `canvas`
(the pan/zoom node canvas: layout, links, hit-testing, dragging), `highlight`
(colouring the WXSL/WGSL code panels, from `wxsl-lang`'s own lexer), `palette`
(searching the node library), `preview` (the offscreen material preview and
the compiled WXSL and WGSL), `ui` (the immediate-mode layer: identity,
interaction, widgets), `widgets` (editors per socket type, and the colour
picker), `theme`.

**`wxsl`** — feature-gated re-exports, plus `stdlib_library()`, the one
line that hands the node library's WXSL to the renderer.

## Feature flags (`wxsl` facade crate)

| Feature | Default | Adds | Implies |
|---|---|---|---|
| `render` | **on** | `wxsl-render` (wgpu pipelines) | — |
| `stdlib` | **on** | `wxsl-stdlib` (original base nodes) | — |
| `editor` | off | `wxsl-editor` (visual node editor) | `render` |
| `gltf` | off | glTF/GLB geometry import (`wxsl_render::gltf`) | `render` |

A headless runtime that just loads and runs a pre-authored graph can use
`default-features = false, features = ["render"]` — no GUI toolkit anywhere
in its dependency tree. A build with nothing but `wxsl-core` (e.g. an
offline graph validator/exporter) uses `default-features = false` with no
features at all.

## Data flow: authoring to pixels

```mermaid
graph TD
    author["Graph authored<br/>(editor UI, or built programmatically,<br/>or loaded from the node format)"] --> model
    model["wxsl_core::graph::Graph<br/>(typed, acyclic, validated)"] --> codegen["wxsl_core::codegen<br/>(graph -> WXSL source)"]
    codegen --> weslsrc["one WXSL module:<br/>imports + wxsl_material()<br/>+ @if-gated entry points"]
    codegen --> macromod["generated macro module<br/>(numeric macro variables<br/>as const declarations)"]
    library["wxsl_render::ShaderLibrary<br/>(the ABI + node functions,<br/>supplied by the application)"] --> compiler
    weslsrc --> compiler["wesl<br/>(WXSL -> WGSL: resolves imports,<br/>evaluates @if/@elif/@else)"]
    macromod --> compiler
    compiler --> variants["wxsl_render::variants<br/>cache: (source+macros hash, stage)<br/>-> wgpu shader module"]
    path["MaterialStage of the pass<br/>(forward_lit | gbuffer | depth_only)"] --> variants
    variants --> record
    passes["wxsl_render::pipeline<br/>(forward: prepass + shading;<br/>deferred: G-buffer + lighting)"] --> schedule["wxsl_render::graph::Schedule<br/>(order, transient reuse,<br/>history rotation)"]
    schedule --> record["record: attachments, pass<br/>bind groups, draws"]
    record --> gpu["wgpu render passes"]
```

A **material stage** is a property of the *pass*, never of the *graph* — a
material graph is written once and works under every stage, because a stage
changes which entry point codegen emits, not what the graph says. The
stages are a table in `wxsl_core::abi`, so adding one is a row rather than
an arm in every match. See
[ADR 0005](adr/0005-render-pipeline-abstraction-and-shader-switching.md)
and [ADR 0022](adr/0022-material-stages-replace-the-render-path-enum.md).

| Stage | Fragment returns | Used by |
|---|---|---|
| `forward_lit` | one `vec4f` colour | the forward pipeline's shading pass |
| `gbuffer` | the `GBuffer` struct | the deferred pipeline's material pass (the struct is generated per lighting-model set) |
| `depth_only` | nothing — no fragment stage at all | the forward pipeline's depth prepass |

## How a frame is drawn

A pipeline is not a Rust struct: it is a list of `PassDesc`s over a set of
`ResourceDesc`s — a `wxsl_render::graph::RenderGraph`. Where the list comes
from has two spellings, tested to agree
([ADR 0033](adr/0033-pipelines-are-documents.md)):

* **As a document.** A pipeline is a `wxsl_core::graph::Graph` over the
  *pipeline node registry* (`wxsl_core::pipeline`): sources
  (`source.scene`, `source.lights`), resources (`resource.gbuffer`,
  `resource.color`, `resource.depth`), passes (`pass.geometry`,
  `pass.shadow`, `pass.screen`) and one `present` terminal. Edges carry
  render-graph resources (`DrawQueue`, `ColorTarget`, `DepthTarget`,
  `ShadowMaps`, `GBuffer` — handle types outside `ValueType::ALL`, like
  the texture sockets). `wxsl_render::pipeline_doc::compile` is a pure
  function turning the document into a `RenderGraph`; every error names
  the document node. The two stock pipelines are preset files
  (`crates/wxsl-render/assets/presets/*.pipeline.json`) that
  `StockPipeline::graph` loads and compiles.
* **As hand-built Rust.** `forward_graph` and `deferred_graph` stay as the
  reference pass lists the presets' parity tests compile against; an
  application building its own graph hands it to `Renderer::set_graph`.

The two shipped pass lists are `StockPipeline::{Forward, Deferred}`.
Forward is a depth prepass (`depth_only`) followed by a shading pass
(`forward_lit`) that tests `LessEqual` without writing depth; deferred is a
G-buffer pass (`gbuffer`) followed by a fullscreen lighting pass.

`RenderGraph::schedule` is a pure function and is tested without a device.
It orders the passes by what they read and write (never by declaration
order), validates them, and decides which physical texture serves each
resource. `ResourcePool` then owns the textures, and `RenderGraph::record`
opens each pass, resolves its attachments, builds its pass bind group from
its declared reads and hands it to the renderer to draw into.

Two properties of a resource are worth knowing before you need them:

* **Persistence.** `Transient` is created at first write and its texture is
  reusable after its last read; `Persistent { history: n }` is a ring of
  `n + 1` textures, so a pass can read what a previous frame wrote. Reading
  history creates no ordering edge — that is what keeps a temporal pass from
  being a cycle.
* **Dimension.** 2D, 2D array, cube or 3D, because cascaded shadows,
  reflection probes and volumetrics each want a different one.

The frame's own target is resource 0, `RenderGraph::TARGET`, and is
*imported*: the caller supplies a view for it each frame.

## Swapping pipeline

`Renderer::set_pipeline` switches now and compiles whatever is missing
during the next frame, which is a hitch. `Renderer::request_pipeline`
switches when it is ready: the missing stages compile on a worker thread
while the current pipeline keeps presenting, and the swap lands in one
frame. `Renderer::swap_progress` is what a `compiling 3/7` indicator reads,
and the editor's `F`/`D` keys are the second kind.

Only the WXSL-to-WGSL half runs on the worker — a pure function over text,
no device in it. On a single-threaded target there is no worker and the
swap blocks, which is what the indicator is for. See
[ADR 0022](adr/0022-material-stages-replace-the-render-path-enum.md).

## What a frame draws

A **scene** — `wxsl_core::scene::Scene` — is meshes, materials and
instances, and it is pure serializable data with no `wgpu` in it. It says
what exists; it says nothing about how it is drawn, because the pipeline
belongs to the renderer.

The renderer takes a `DrawList`: geometry, a material, a transform and the
`Tags` the material was authored with. A geometry pass draws a **tag
expression** over it (`opaque`, `opaque && !outlined`, `*`), so the material
says what it *is* and the pass says what it *draws*. Translating a scene
document into a draw list needs the node registry and a device at once, so
it is the facade's job: `wxsl::scene::SceneResources`.

`wxsl_render::environment::Environment` is the other half of a frame:
camera, lights, ambient, exposure, time. Every draw's transform goes into
one storage buffer in the frame group, indexed by
`@builtin(instance_index)`, so a draw's position in the list is its row.
See [ADR 0021](adr/0021-a-declarative-render-graph-and-a-scene-document.md).

## What a graph is responsible for

A material graph describes a *surface*, not a whole shader: it compiles to
one function taking the per-fragment `SurfaceContext` and returning a
`Surface` (base colour, metallic, roughness, normal, emissive, occlusion,
alpha). Codegen wraps that in the entry points; the vertex stage is
hand-written WXSL in `wxsl-stdlib`; and the light loop, the lighting-model
dispatch and the G-buffer packing are *generated* — see "Lighting models"
below.

`wxsl_core::abi` is where the two halves agree on names and field
layouts, and where the render-path flag and the macro variables the ABI
honours are declared. See
[ADR 0008](adr/0008-surface-graphs-and-a-named-shader-abi.md).

Both paths shade through the *same generator* — `wxsl_core::lighting`
writes `shade_surface` for the forward fragment entry (calling that
material's model directly) and for the deferred lighting pass (switching
over the model id the G-buffer carries) — so the two cannot drift apart in
what lighting means. `crates/wxsl/tests/` asserts they render the same
image, with mixed lighting models in the frame.

## Lighting models

A *lighting model* is one WXSL function of a fixed signature plus a small
integer id; a registry entry in `wxsl_core::lighting` names it. A material
says which model shades it (by name, in `MaterialOptions::lighting` or the
scene document's `lighting` field); a deferred pipeline enables a
**`LightingSet`**, which decides three things at once:

* the **G-buffer layout** — base targets, plus a scalar id channel when
  the set dispatches, plus one target per model that requests one;
* the **generated dispatch** — a direct call in a forward module, a
  `switch` over the id in the lighting pass;
* the **cost** — the set's layout is checked against WebGPU's
  bytes-per-sample attachment budget by name, in `Renderer::set_lighting`.

The default set is one model, the library's PBR: no dispatch, no id
channel, a G-buffer shaped exactly as it always was. The shipped models —
`lambert`, `phong`, `pbr`, `clearcoat` — live under
`shaders/lighting/models/`, deliberately outside the node derivation:
they are shaded through the registry's contract, not placed on a canvas.
See [ADR 0028](adr/0028-lighting-models-dispatched-by-a-g-buffer-id.md).

## Bind groups

WebGPU guarantees only four bind groups, so all four are allocated up front,
ordered by how often their contents change — a backend may disturb
higher-numbered groups when a lower one is rebound.

| # | Slot | Rebound | Holds |
|---|---|---|---|
| 0 | `frame` | per frame | Camera, scene lighting, the instance transform buffer |
| 1 | `material` | per material | A graph's uniform parameters, textures and samplers, all of them declared by the graph |
| 2 | `user` | per material | Nothing wxsl binds. A material may *declare* the block it expects here; the application fills it |
| 3 | `pass` | per pass | Whatever a pass declares it reads: the G-buffer; the UI pass's viewport and atlas; the MSDF compute pass's buffers |

Instance transforms share the frame group despite changing per draw, as one
read-only storage buffer: one binding and one upload serve the whole frame,
where a group of their own would cost a quarter of the budget. (ADR 0010
chose a per-object uniform at a dynamic offset for this; ADR 0021 took the
storage buffer it had already named as the later option, once a frame drew
more than one thing.) That leaves the application one free slot, not two —
the honest cost of the deferred path needing somewhere to put the G-buffer
while a graph still has to compile for either path.
`wxsl_core::abi::BIND_GROUPS` is the single declaration, and a test in
`wxsl-stdlib` asserts the shipped `.wxsl` binds the groups it names. See
[ADR 0010](adr/0010-four-bind-groups-allocated-by-update-frequency.md) and
[ADR 0021](adr/0021-a-declarative-render-graph-and-a-scene-document.md).

The pass group is no longer hand-written per pipeline: it is built from a
pass's `reads`, in declaration order, at bindings 0..n. The deferred
lighting pass's G-buffer bindings are what that produces for the deferred
pass list, and a screen effect's inputs will be what it produces later.

Groups 1 and 2 have no fixed layout at all, because a *material* decides
them. `wxsl_core::resources::MaterialInterface` is that decision, computed
from the reachable part of the graph: the uniform parameters and their
offsets, the textures and samplers and their bindings, and the block the
application is expected to supply. Codegen emits the declarations from it
and `wxsl_render::bindings` builds the bind groups from it, so the two
halves cannot disagree — see the next section. Both groups arrive on the
*draw* (`DrawItem::bindings` and `DrawItem::user`), because the resources
behind them belong to the application, which is the same rule that keeps
the renderer scene-graph-free.
[ADR 0023](adr/0023-a-material-declares-its-resources.md).

## What a material graph is

Several terminals, not one.

* **`output.surface`** — the material, evaluated per fragment. Required,
  and unique.
* **`output.vertex`** — an object-space offset added to the vertex before
  it is transformed. Optional.
* **`output.discard`** — throw this fragment away. Optional, and not the
  same thing as `alpha`: alpha is a blend weight the deferred path cannot
  honour and does not stop depth being written, while a discarded
  fragment leaves a hole that a shadow can shine through.
* **`output.varying`** — one per declared interpolant: a value the vertex
  stage computes and the fragment stage reads back
  ([ADR 0027](adr/0027-a-graph-computes-its-own-interpolants.md)).

Codegen partitions the graph by reachability **from each terminal
separately**, and emits one function per partition: `wxsl_vertex`,
`wxsl_discard`, `wxsl_material`. Where each node *runs* is decided first,
by stage analysis ([ADR 0032](adr/0032-stage-analysis-computes-the-cut-between-stages.md)):
every node defaults to `Auto` — the earliest stage that can produce it and
satisfy every consumer — so a node both stages read is computed once, per
vertex, and the fragment side reads a *synthesized* interpolant the
analysis declared (named `autoN`, from the same location budget as a
hand-wired one). A node can pin `Vertex` or `Fragment` to override that;
what cannot ride an interpolant is computed in both stages, as below.

A node feeding two terminals used to be compiled into both unconditionally
— two `let` bindings in two functions, on the grounds that a shader
compiler's common-subexpression pass is cheaper than an inter-stage
location ([ADR 0025](adr/0025-a-material-graph-spans-shader-stages.md)).
The analysis keeps that as the fallback for un-interpolable types and
spent budgets, and as the explicit `Fragment` choice, but the default is
now to share through the stage boundary rather than always pay twice.

**A stage compiles only the partitions it needs.** A pass that writes no
colour — a depth prepass, a shadow pass — wants the vertex offset and the
alpha test and nothing else, so the material function is not in its
module at all. A material that does not discard gets no fragment stage
there either. That is why a displaced object casts a displaced shadow and
a perforated one casts a perforated shadow: the parts that decide those
things are compiled into the pass that draws them, and the rest is not.
[ADR 0025](adr/0025-a-material-graph-spans-shader-stages.md).

The vertex side reads a `VertexContext`, which is a *superset* of the
`SurfaceContext` the fragment side reads: same field names, same types,
computed before any displacement. So `input.uv` is one node that works in
either stage, and the only vertex-only inputs are the two object-space
ones. Those are what stage analysis is for: an object-space read the
fragment stage wants is computed per vertex and interpolated down. The
named errors remain for the decisions that cannot be honoured — an
object-space node *pinned* to the fragment stage, a computed interpolant
read in the stage that computes it
([ADR 0032](adr/0032-stage-analysis-computes-the-cut-between-stages.md)).

## What a material declares

A graph does not only compute. Three things it can ask for from outside
itself, and the difference between the first and a `const` node is the
point of all of it:

* **Uniform parameters** (`param.value`). A field of one buffer in group
  1, so changing one is a buffer write — no new shader variant, no new
  pipeline. A `const` is inlined into the generated WXSL, so changing one
  *is* a recompile. Both look like a value on the canvas; only one is a
  slider.
* **Textures and samplers** (`texture.texture_2d`, `texture.sampler`),
  bound by name in group 1 and read by `sample.texture_2d`. These are the
  first socket types that carry a *handle* rather than a value, so they
  live in `ValueType` but not in `ValueType::ALL`.
* **The application's block** (`Graph::user_block`, read by `input.user`).
  Different in kind: the material does not own it. It states the shape,
  `Renderer::user_layout` hands out the `BindGroupLayout`, and the
  application hands back a `BindGroup`. `wgpu` checks that they agree, so
  there is no validation of ours to keep in sync.

A fourth thing it can ask for does not come in through a bind group at
all:

* **Attributes** (`Graph::attributes`, read by `input.attribute`). Per
  vertex, and the mesh must carry a stream of that name; per instance,
  and the draw must supply a value; or **computed**, and the graph's own
  vertex stage writes it through `output.varying`. Which of the three is
  part of the *declaration* and not of the reading node, so moving an
  attribute between them rewires nothing. A mesh or a draw that cannot
  supply one is an error naming the material, the attribute and the mesh
  — reported while the frame is compiled, before a pass is opened.

And two flags, which are facts about the material in the way its tags
are: **`cast_shadow`**, whether the shadow passes draw it, and
**`receive_shadow`**, whether its shading is attenuated by the maps. The
first is a selection and changes no generated code; the second is a macro
and so a variant of its own ([ADR 0026](adr/0026-shadows-a-view-per-light-and-two-flags-on-the-material.md)).

## How a shadow gets there

One depth 2D texture array, one slice per light, in the **frame group**
beside the lights themselves — so the generated shading function has one
`shadow_factor` and the forward stage and the deferred lighting pass both
inherit it.

A pass renders from a **view**: `PassDesc::view` names the camera or a
light, and the frame group's camera binding is addressed by a dynamic
offset, so `camera.view_proj` in a shadow pass is the light's and
`transform_vertex` never learns that shadows exist. Both stock pipelines
declare one shadow pass per light slot up front; a slot whose light is
not casting is cleared and drawn into by nothing, which reads as fully
lit.

The lookup is PCF over a comparison sampler, biased by **normal offset**
rather than depth bias — the sample point moves along the surface normal
instead of the comparison moving, which is where the error actually is
and the only version that behaves on a displaced vertex or a perforated
surface.
  [ADR 0024](adr/0024-a-material-declares-the-geometry-it-requires.md).

Each declaring node carries a **setting** — a string the node instance
holds that names the thing it declares. A setting is not a label: renaming
a `param.value` renames the uniform.

**The layout is computed, not mirrored.** This is the one host-shared
buffer in the repo with no `#[repr(C)]` struct beside it, because its
fields are whatever the graph declared.
`wxsl_core::resources::BufferLayout` computes the offsets under WGSL's
uniform rules — a `vec3f` aligns to 16 and occupies 12, a `mat3x3f` is
three columns each padded to 16 — and it is the *only* thing that knows
them: the shader's struct is generated from it, and the host writes
through it. It has two customers: the parameter buffer, and the row of
declared per-instance attributes, which differs only in that the storage
address space does not round a struct's alignment up to 16.
`crates/wxsl/tests/material_resources.rs` and `material_geometry.rs` run
one probe graph over both, on a GPU, at every type — which is what
replaces the mirror test everything else here gets.

**Where a declared attribute lands.** A per-vertex one is a vertex buffer
of its own at slot 1 and up, matched to the mesh's stream *by name*; four
of them is the budget, because WebGPU guarantees eight slots and the base
vertex takes one. A per-instance one is a row of a second storage array in
the frame group, beside the transform array rather than inside it: the
transform array is ABI and the hand-written vertex stage reads it at that
stride, so widening it would move every field out from under code that
cannot know it moved. Both arrays are indexed by the same
`@builtin(instance_index)` — which WGSL offers in the vertex stage only,
so one `@interpolate(flat) u32` varying carries it down and the fragment
stage re-indexes. One location, however many attributes.

## Macro variables

Not everything a node needs can be a socket value: a loop bound has to be a
compile-time constant, and a lighting-model switch should remove code rather
than pick between two results. Those are *macro variables*
(`wxsl_core::macros`), declared by node definitions and pinned per graph
in the node format:

| Kind | Becomes | Example |
|---|---|---|
| `Flag(bool)` | a WXSL conditional-translation feature (`@if(name)`) | `wxsl_fbm_ridged`, `wxsl_tonemap` |
| `Int` / `Float` | a `const` the declaring module carries | `WXSL_FBM_OCTAVES` |

A macro is declared, with a default, by the WXSL module that uses it —
`@macro const OCTAVES: i32 = 5;` — and is an ordinary module-scope `const`
after binding, which is why it can be a loop bound or an array size.

Precedence, weakest first: the declaring file's default, each node's declared
default, what the graph pins, then what the application overrides. The whole
set is part of the variant cache key, because the bindings also reach
*imported* modules, whose own defaults are nowhere in the root source.

## The renderer takes its shaders from the application

`wxsl-render` compiles WXSL but ships none — it must not depend on
`wxsl-stdlib` (ADR 0002), so the application hands it a `ShaderLibrary`.
With both facade features on that is `wxsl::stdlib_library()`. This is
also the seam for substituting an ABI module or adding hand-written WXSL. See
[ADR 0009](adr/0009-the-application-supplies-the-shader-library.md).

## The editor draws itself with the renderer

There is no GUI toolkit in this workspace. The editor's chrome is a WXSL
module (`package::wxsl::ui`) compiled by `wxsl-lang` and submitted through
`variants::compile` like any material, and its preview is the real
`Renderer` on the real pipelines — so every frame of editing exercises the
stack the editor exists to author for. [ADR
0013](adr/0013-the-editor-draws-itself-with-wxsl-render.md) records why, and
what it cost (`wxsl-editor` now depends on `wxsl-render`, amending ADR
0004).

Two things follow that are worth knowing before touching either half:

* **Everything on screen is one instance.** A rounded box, a capsule, an
  image and a glyph are the same primitive with a different `kind`; the
  quad's corners come from the vertex index, so there is no vertex or index
  buffer in the UI path at all. The layout is host-shared three ways —
  `abi::UI_ATTRIBUTES`, `wxsl_render::ui::draw::UiInstance`, and
  `shaders/wxsl/ui.wxsl` — and they are edited together.
* **Text is MSDF, generated in-tree, twice.** `ui::msdf` is the reference
  implementation on the CPU and `shaders/wxsl/msdf.wxsl` is the same
  algorithm as a compute pass; `MsdfBackend` picks one at runtime and a test
  asserts they agree. The renderer embeds no font: the application supplies
  the bytes, exactly as it supplies the shader library. [ADR
  0014](adr/0014-msdf-text-with-an-own-generator-and-app-supplied-fonts.md).

## Why WXSL and not raw WGSL

WGSL alone has no imports and no conditional compilation, both of which
this project needs structurally: composing node functions, and branching
shader behaviour on a feature flag. It also has no generics, which
the node system needs so that one authored function can serve `f32`,
`vec2f`, `vec3f` and `vec4f`.

WXSL is a superset of WGSL adding four things — templates, imports,
conditional translation and macro constants — compiled by `wxsl-lang` and
lowered back to WGSL, because WGSL is what `wgpu` consumes.

This started as a dependency on [WESL](https://wesl-lang.dev), which
provides imports and conditional translation. That was
[ADR 0003](adr/0003-wesl-as-the-shading-language.md), now superseded: WESL's
generics do not work (tested, with the evidence recorded in that ADR), and
templates were the feature the node system could not do without.
[ADR 0011](adr/0011-own-the-shading-language.md) records the replacement and
the four cheaper alternatives it rejected first.

## The compiler

`wxsl-lang` is a source-to-WGSL compiler, in pipeline order:

| Stage | Job |
|---|---|
| `lexer` | tokens, plus WGSL's template disambiguation (`a < b` vs `vec3<f32>`) |
| `grammar` | LALRPOP, over the token stream rather than raw text |
| `cond` | evaluate `@if`, bind `@macro const` values — per module, before renaming |
| `resolve` | inline imports, mangle by origin, rewrite references respecting shadowing |
| `mono` | instantiate templates, one copy per set of type arguments |
| — | dead-code elimination from the root's declarations |
| `emit` | WGSL, refusing anything still WXSL-only |

The pass order is an invariant, not a preference:

* `cond` must run before renaming, because an `@if` names macros in its own
  module's vocabulary and a dropped branch should never have its references
  resolved.
* `mono` must run after flattening, because a template and its call sites
  can be in different files, so no per-module pass sees all of them.
* dead-code elimination must run after `mono`, so an instantiation whose
  only caller was itself dropped goes with it.

`resolve` takes the bindings and sequences all of this itself rather than
trusting callers to get it right.

Templates carry one builtin, `components(T)` — the number of scalar
components in a type, folded to an integer literal, so it works as an array
size or a loop bound. Type arguments are written explicitly (which is what
the node graph emits, since a graph knows every socket's type) or inferred
from the arguments by a deliberately shallow rule that errors rather than
guesses. [ADR 0012](adr/0012-monomorphize-templates-on-the-flat-module.md)
records why, and what a full type checker would have cost.

The backend's refusal is deliberate. If an unresolved import, an
uninstantiated template or a surviving `@if` reaches it, the error names the
construct and the pass that should have removed it — instead of `wgpu`
rejecting syntax it has never heard of.

One module sits off that pipeline: `node`, which parses a single file and
answers with the `NodeDefinition` it describes rather than with WGSL
([ADR 0020](adr/0020-a-node-definition-is-derived-from-its-wxsl-source.md)).
It is the only place in the compiler that reads comments, because the node
metadata a signature cannot carry — a label, a socket default — is stated in
them rather than in attributes the compiler would have to thread through
every pass and then ignore.

## The base node library

`wxsl-stdlib` mirrors the category layout common to granular shader
libraries under `crates/wxsl-stdlib/shaders/` (`math/`, `color/`,
`space/`, `lighting/`, `generative/`, `sdf/`, `sample/`, `animation/`,
`filter/`, `distort/`), one `.wxsl` file per function — but every function
is original code, not a port. A `wxsl/` directory alongside them holds the
shader ABI (ADR 0008), which is plumbing rather than granular functions.

Two kinds of node come out of it. Arithmetic is an inline WXSL expression
(`math.add` is `{a} + {b}`), because wrapping an addition in a function call
buys nothing. It is *one* node per operation, not one per operation and
type: the definition declares the types it works over as a type parameter
and its sockets carry it, resolved per graph node from whatever is connected,
and the operators whose two operands WGSL lets differ (`f32 * vec3f`,
`mat3x3f * vec3f`) declare two parameters and derive the result from both
([ADR 0018](adr/0018-one-generic-node-per-operation.md)). Everything with a body — PBR
shading, noise, tonemapping, colour spaces — is a real WXSL function, and
**the file is the node**: its signature gives the sockets, its type
parameter's bound list gives the allowed set, its `@macro const`s give the
macro declarations, and its comments give the label, the documentation and
the socket defaults
([ADR 0020](adr/0020-a-node-definition-is-derived-from-its-wxsl-source.md)).
`wxsl-stdlib/build.rs` runs `wxsl_lang::node_from_source` over every file
under a category directory and writes the results out as the table
`registry.rs` includes, so a function and its node cannot disagree — there
is nowhere for them to disagree — and hand-written WXSL can call the same
function a graph does. An earlier plan to rewrite
[LYGIA](https://lygia.xyz) into WXSL was scrapped once its non-permissive
license ([ADR 0006](adr/0006-lygia-port-licensing-and-isolation.md)) turned
out to be a real adoption cost even fully isolated behind an opt-in
feature; ADR 0007 replaced it with this from-scratch library, which is why
`stdlib` needs no special licensing treatment and defaults on. See
[ADR 0007](adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md) and
`crates/wxsl-stdlib/shaders/README.md`'s authoring rule before adding a
function.

## Current status

Implemented and tested end to end:

| Area | State |
|---|---|
| `wxsl-core`: node/socket model, typed acyclic graph, validation, WXSL codegen, macro variables, node format (serde) | done |
| `wxsl-stdlib`: shader ABI, 100 node definitions over arithmetic, vectors, conversions, logic, colour, space, noise, SDFs, animation, PBR lighting | done |
| `wxsl-render`: WXSL→WGSL compilation, variant cache, forward and deferred pipelines, the render graph with per-light shadow passes, lighting-model sets, cube mesh, scene uniforms, offscreen rendering | done |
| `wxsl-render`: the `ui` layer — texture atlas, MSDF text (CPU and compute pass), instanced draw list, input, the UI pass | done |
| `wxsl-editor`: node canvas (pan/zoom, link, unlink, move, delete), searchable palette, live preview, WXSL/WGSL/problem panels, macro and parameter editing, per-node name and colour, light/dark themes | done |
| `wxsl`: facade, `stdlib_library()`, the `pbr_cube` demo, the `editor` demo | done |

The demo is the thing to run first:

```sh
cargo run -p wxsl --example pbr_cube              # windowed; F/D switch path
cargo run -p wxsl --example pbr_cube -- --headless  # both pipelines to PNG
cargo run -p wxsl --example pbr_cube -- --dump-wgsl # what the graph became
cargo run -p wxsl --example pbr_cube -- --headless --models lambert,phong,pbr,clearcoat
                                     # three models and an extra G-buffer target,
                                     # through one deferred lighting pass
```

And the editor, which is the same graph with somewhere to edit it:

```sh
cargo run -p wxsl --features editor --example editor
cargo run -p wxsl --features editor --example editor -- --screenshot out.png
```

Test coverage worth knowing about, since it is what keeps the two halves of
the shader ABI honest:

- `crates/wxsl/tests/graph_to_wgsl.rs` compiles *every node in the
  library* to WGSL for every material stage, plus the demo graph, macro
  switching, and the node format's round trip. No GPU needed.
- `crates/wxsl/tests/render_cube.rs` renders the cube through both pipelines
  on a real device and asserts the images match. Skips when no adapter is
  available.
- `crates/wxsl/tests/editor_frame.rs` drives the editor for several frames
  on a real device: that it draws an interface at all, that editing
  recompiles, that a path switch produces different WGSL, that a frame of
  every input event leaves it drawing — and that both MSDF backends produce
  the same interface, which is what keeps the CPU generator and the compute
  shader honest. Also skips with no adapter.

## See also

- `docs/adr/` — the decision log this map summarizes.
- `docs/glossary.md` — terminology used above without re-explanation.
- `AGENTS.md` (repo root) — process and conventions for making changes here.
