# Plan: from a material compiler to a pipeline editor

Written in English to sit alongside `AGENTS.md`, `docs/architecture.md` and
the ADRs, which future sessions read as one body of text. This file is a
*plan*, not a record: when a milestone lands, its decisions move into an ADR
and the corresponding section here shrinks to a link. Delete it when the last
milestone is done.

**Landed so far:** M0.

## How to read this

Everything below follows from this table. These are settled: revisit one by
superseding its row, not by quietly diverging inside a milestone.

| Decision | Chosen |
|---|---|
| What a document holds | **A scene — and the pipeline is not in it.** The unit is a *scene*: meshes and instances, each instance carrying a material. It says what exists, never how it is drawn. It is pure serializable data, so it lives in `wxsl_core::scene`, and the renderer keeps consuming a draw list — which is the decision its own doc already records, that batching, culling and sorting belong to the application. `wxsl_render::scene::Scene` is renamed `Environment` (camera, lights, ambient) to end the collision. |
| Who owns the pipeline | **The renderer.** A pipeline is swappable at any moment and swapping re-evaluates everything, because the pipeline is what decides how a material is split. Pipelines live in their own files, so `wxsl-stdlib` can ship a standard deferred one. |
| Swapping a pipeline | **Non-blocking: the previous pipeline keeps presenting.** Everything declarative — stage set, target descriptors, pass order — is re-derived at once because it is only data. Missing variants compile in the background behind a `compiling 3/7` indicator, and the swap lands in one frame when they are ready. No freeze, no black frame, no half-drawn scene. |
| Mesh assets | **glTF, behind a `gltf` feature.** Geometry only at first: positions, normals, tangents, uv, indices — no materials, no skins. A scene of primitives cannot demonstrate shadows or depth peeling convincingly, and a bespoke format's converter would have to parse glTF anyway. |
| Instance transforms | **A storage buffer indexed by `@builtin(instance_index)`.** One binding, one upload, no per-draw rebind — and the only shape that composes with M1's indirect draws, since a GPU culling pass emits instance *indices*. Amends ADR 0010's per-object dynamic offset. M4 extends this same buffer with whatever per-instance attributes a material declares. |
| Where pipeline configuration lives | **Two graph kinds.** A frame-level *pipeline graph* reuses `wxsl-core`'s node/socket/type model and is edited in its own canvas. A *material graph* stays per-surface, and codegen splits it across whatever stages the pipeline asks for. |
| Portability target | **WebGPU baseline, native fast paths behind a feature.** Every pass must have a path that runs on the WebGPU baseline; a native path may be faster or higher quality, and both are tested. |
| Culling | **Convention, as a per-pass knob.** The opaque pass culls back faces; an outline pass culls front faces. `cull_mode` is a field of a pass description, never a constant in the code. |
| How an object picks its queue | **Tags on the material, a tag expression on the pass.** The material declares what it *is* (`opaque`, `transparent`, `outlined`); a geometry pass declares what it *draws*. No introspection of graphs, and a draw may override its tags. |
| How a node is declared | **The `.wxsl` file is the definition.** *Done — [ADR 0020](docs/adr/0020-a-node-definition-is-derived-from-its-wxsl-source.md).* Its signature, its returned struct and its `@macro const`s derive into sockets, outputs and macro declarations; its leading comment block carries the label and doc, and `// @default` annotations carry socket defaults. The derivation is a public `wxsl-lang` function, called at build time for the stdlib and at runtime for files the user writes. |
| How a material declares a uniform | **A `param` node, with the layout computed rather than mirrored.** `const` inlines into the shader, so editing one compiles a new variant; `param` is a field of group 1's uniform buffer, so editing one only writes bytes. `wxsl-core` computes the WGSL layout — `vec3f`'s 16-byte alignment makes a hand-written Rust mirror impossible — and emits an offset table `wxsl-render` writes through. |
| How a material uses the application's slot | **It declares a block and hands out the layout; the application hands back a bind group.** Group 2 stays the application's and holds nothing of ours, but a material may now state what it expects to find there. `wgpu` validates the match, so there is no checking code of ours to keep in sync. |
| How a material requires a *vertex* attribute | **The base four are ABI and always present; declared extras are separate vertex buffers at locations 4 and up.** Read with one `input.attribute` node carrying the name, so `NodeRegistry` stays global and static. A mesh that lacks a declared attribute fails when the draw list is built, naming both sides. |
| How a material requires an *instance* attribute | **A field of the instance storage buffer, not an instance-step vertex buffer.** Same declaration in the editor, different backing, and for the reason already settled two rows up: one binding however many attributes, no vertex-slot pressure, and it survives a GPU culling pass that emits instance *indices*. A fragment stage reads them by passing `instance_index` down as one flat varying and re-indexing. |
| Lighting models | **A registry of WXSL functions, hand-written first.** A model is one function of fixed signature plus a small id; whether that function was written or generated is invisible to the dispatch, so graph-authored models land later against the same registry. |
| Compute passes | **In the engine from M1, first used in M7.** The repo already runs a compute pass — MSDF generation (ADR 0014) — so the pipeline, bind-group and variant machinery exists and the render graph generalizes it rather than inventing it. |
| Antialiasing | **Post-process only.** No MSAA anywhere: FXAA/SMAA in M7, TAA once persistent resources exist. One behaviour across forward, deferred and peeling, so the editor's preview always represents the final frame. |
| Shadows | **A texture array, one slice per light or cascade, PCF with a normal-offset bias.** Atlas packing is a later, contained change; normal-offset rather than depth bias because materials will displace and discard. |
| First pipeline milestone | **The render graph first.** Re-express forward and deferred on a declarative pass engine, adding no visible feature, so everything after it is additive rather than a refactor. M0 precedes it as independent groundwork. |

## Where we are

Honest baseline, because three of the gaps below are load-bearing for what
comes next:

* **Passes are hand-written Rust.** `ForwardPipeline` and `DeferredPipeline`
  each own their attachments, their depth texture and a pipeline cache, and
  `record()` writes out `begin_render_pass` by hand. Adding a fourth pass
  means a fourth struct that repeats all of it.
* **Render path is a two-valued enum.** `RenderPath::{Forward, Deferred}`
  binds one flag, `abi::FEATURE_DEFERRED`, which picks one of exactly two
  `@if`-gated fragment entry points that `codegen::write_entry_points`
  prints as a format string. There is no room in that shape for a third,
  fourth or fifth entry point.
* **One draw per frame.** `Renderer::render` takes one mesh and one material.
  `pipeline::FrameInput` already carries `draws: &[Draw]`, so the plumbing
  underneath is ready, but nothing above it builds a list.
* **The material bind group is not bound.** `MaterialPipelines::new` binds
  group 0 only, with a comment saying group 1 joins it "when a graph can
  declare parameters". There are no texture or sampler value types, so a
  graph cannot sample anything at all. Shadows, bakes and postprocess all
  need this before they need anything else.
* **No device feature negotiation.** `gpu.rs` calls `request_device` with
  `..Default::default()`, so no optional feature is ever requested and no
  code can ask whether one is available.
* **Four lights, no shadows, no transparency.** `MAX_LIGHTS = 4` in a fixed
  uniform array; `Light` has no shadow data; nothing anywhere blends.

What *is* solid and should not be disturbed: the graph model and its typing,
the WXSL compiler and its template monomorphization, the variant cache's
shape (`(source+macros hash, path)` generalizes cleanly), the ABI-as-tables
idea, and the editor's canvas — a pipeline graph is a second canvas over the
same model, not a second editor.

## The shape of the answer

Four concepts, in the order they need to exist.

### 1. A render graph, at runtime

A `RenderGraph` is a list of pass descriptions and the resources they read
and write; the engine orders them, allocates transient targets, aliases what
it can, and records them. A pass description is data:

```rust
pub struct PassDesc {
    pub label: String,
    pub kind: PassKind,
    pub color: Vec<Attachment>,      // load/store/clear, per target
    pub depth: Option<DepthAttachment>,
    pub state: PassState,            // cull_mode, depth_compare, depth_write, blend
    pub reads: Vec<ResourceId>,      // becomes the pass bind group
    pub writes: Vec<ResourceId>,
}

pub enum PassKind {
    /// Draw geometry with each material compiled for `stage`, taking its
    /// draws either from the scene filtered by a queue tag or from an
    /// indirect buffer a compute pass wrote.
    Geometry { source: DrawSource, stage: MaterialStage },
    /// One fullscreen triangle running a screen-domain graph.
    Screen { graph: ScreenGraphId },
    /// A compute dispatch (bloom chains, tile classification).
    Compute { entry: String, workgroups: Dispatch },
}
```

The engine owns exactly two hard jobs: **transient resource lifetime** (a
target is created on first write and may be reused after its last read) and
**pass state validation** (a `Geometry` pass writing three colour targets
needs a material stage that returns three). Everything else is bookkeeping.

### 2. Material stages replace `RenderPath`

`RenderPath` stops being the compile axis. A surface graph compiles once per
**stage**, where a stage says which entry points to emit and what the
fragment stage returns:

| Stage | Fragment returns | Which part of the graph it needs |
|---|---|---|
| `ForwardLit` | `vec4f` final colour | everything |
| `GBuffer` | the G-buffer struct | everything except lighting |
| `DepthOnly` | nothing | position, and alpha if the material discards |
| `Shadow` | nothing | same as `DepthOnly`, with a depth bias |
| `PeelFront` / `PeelBack` | colour + depth bounds | everything, plus the peel test |
| `Velocity` | `vec2f` screen motion | position, current and previous |

Stages are a table in `wxsl_core::abi`, like `GBUFFER_TARGETS` already is,
and each declares its entry-point shape. `codegen` emits the entry points
the requested stages need instead of the two it hardcodes, and the variant
cache key gains the stage. This supersedes the entry-point half of ADR 0005;
the *reason* ADR 0005 gives (one graph, many pipeline shapes, differences as
conditional translation) is exactly what generalizes.

### 3. Graph domains, and stage partitioning

Two things the material graph cannot express today, both requested:

**Domains.** A graph is authored against an ABI, and there will be more than
one. `Surface` is today's (`SurfaceContext` to `Surface`). `Screen` is a
fullscreen graph (uv plus bound textures, returning a colour) — that is what
makes postprocess node-authorable instead of a fixed list of effects. Later,
`Compute`. A `GraphDomain` field on the graph selects the ABI, and the node
registry filters by which domains a definition is valid in.

**Partitioning.** A surface graph gets more than one output node — a vertex
output (position offset, custom interpolants) next to today's surface
output, plus an explicit discard. Codegen must then answer "which nodes does
this stage need, and in which shader stage do they run":

```
        [UV] --> [Noise] --+--> [Surface.base_color]
                           |
        [Time] --> [Wave] -+--> [Vertex.position_offset]
                           +--> [Surface.alpha]

  vs_main    needs Time, Wave
  fs_gbuffer needs UV, Noise, Wave*, Surface
  fs_shadow  needs Time, Wave (alpha only) -- and nothing else
```

`Wave*` is the interesting case: a node feeding both a vertex output and a
fragment output either runs twice or crosses the stage boundary as a
varying. Running it twice is correct and simple; a varying is cheaper and
costs an inter-stage location. Start by duplicating, measure, and only then
add automatic varying allocation — the budget is 16 locations and 5 are
used, so there is room to be lazy for a while.

This is what makes "a discard the shadowing respects" fall out for free:
the shadow stage is the alpha subgraph and nothing else.

### 4. The pipeline graph, as nodes

Once passes are data, a graph that produces that data is a small step. Node
kinds: sources (`Scene geometry`, `Camera`, `Lights`), passes (`Shadow map`,
`G-buffer`, `Deferred lighting`, `Depth peel`, `Bake`, `Screen effect`),
resources (`Render target` with format and scale), and one `Present`. Edges
carry *resources* (a texture handle, a depth buffer, a draw queue), so the
type model needs a few new `ValueType`s — which the texture work in M3
introduces anyway.

The same node/socket/generic machinery applies unchanged, which is the
payoff of having built it: a `Blur` pass node generic over target format is
the same mechanism as `math.add` generic over `f32` and `vec3f`.
### 5. The scene is the unit; the pipeline belongs to the renderer

The authoring side and the runtime side are deliberately asymmetric:

* A **scene** is meshes and instances, each instance carrying a material,
  serialized as one document. It says what exists.
* A **pipeline** is held by the renderer, not by the scene. It says how a
  material is split into stages and in what order the passes run. Swapping
  it is a runtime operation that invalidates every material's stage set, so
  every material recompiles for the new stages.

That last point is what the variant cache is for, and it is why the cache
key gains the *stage* rather than the pipeline: two pipelines that both ask
for a `GBuffer` stage share the compiled result, so swapping between them
costs nothing the second time. A pipeline swap is therefore "re-derive the
required stage set, then compile what is missing" — not "recompile
everything".

**Where each half lives.** A scene is pure data — meshes by name, instances,
materials — so it is `wxsl_core::scene::Scene`, serializable, with no wgpu
anywhere near it. The renderer keeps taking a draw list, which is not a new
decision but an existing one: `RenderRequest`'s own doc says the renderer is
"scene-graph-free on purpose — batching, culling and sorting belong to the
application". Translating a scene into a draw list is the facade's job, not
the renderer's. `wxsl_render::scene::Scene` is renamed `Environment`, which
is what it has always been: camera, lights, ambient.

**What a swap costs, in wall-clock.** Re-deriving the stage set, the target
descriptors and the pass order is free — it is data. Compiling the variants
that are new is not, so it happens in the background while the previous
pipeline keeps presenting, behind a `compiling 3/7` indicator, and the swap
lands in a single frame once they are ready. One honest caveat: `wgpu`
exposes no asynchronous pipeline creation, so "in the background" means a
worker thread. On a single-threaded wasm build there is no background, and
the swap degrades to blocking — which is exactly what the indicator is for.


## The bind-group budget

ADR 0010 spent all four groups, and every feature below wants somewhere to
put a texture. Decide the allocation now, in one place, rather than
discovering the collision in M7:

| Group | Gains |
|---|---|
| 0 `frame` | the shadow atlas and its per-light matrices — produced once per frame, read by every lit pass, so it belongs with the lights it comes from, not in a pass group — and the instance storage buffer, whose *width* a material decides once it can declare per-instance attributes (M4) |
| 1 `material` | the material's **uniform parameters** — one buffer whose layout is computed from the graph — plus its own textures and samplers, and baked lookups (M9). This is the group that finally gets bound. |
| 2 `user` | still the application's, and still nothing of ours in it — but a material may now **declare the layout it expects** there and hand it out, so the application binds against the material rather than against a convention. **This supersedes this row's earlier "untouched"**: the slot's ownership is unchanged, what is new is that the material can describe it. |
| 3 `pass` | pass-local reads: the G-buffer, peel buffers, a screen effect's inputs. Already its job. |

The one that could go either way is the shadow atlas, and putting it in
group 0 is what keeps a shadowed *forward* pass from needing a pass group at
all. If group 0 gets tight, the escape hatch is that `SceneBindings` is one
struct in one file.

## Milestones

Each milestone ends with a runnable demo and green tests, and names the ADR
it owes. Nothing here is a refactor with no observable end state.

### M0 — A `.wxsl` file *is* a node definition — **done**

Landed as [ADR 0020](docs/adr/0020-a-node-definition-is-derived-from-its-wxsl-source.md),
which is now where the reasoning lives. What shipped, and the three things a
later milestone needs to know:

`wxsl_lang::node_from_source(source, module_path)` is the derivation, and
`wxsl-stdlib/build.rs` is its first caller: it runs over every file under
`shaders/<category>/` and writes the results out as the table `registry.rs`
includes. `wxsl-lang` is a **build**-dependency of `wxsl-stdlib`, so
`cargo tree -p wxsl-stdlib -e normal` still shows only `wxsl-core`.
`registry.rs` lost nine `pub fn`s and about 400 lines; the node count is
unchanged at 100, and the demo's two paths still agree to 0.0001.

* **The label and doc convention turned out to be narrower than the plan
  assumed.** "The first line of the leading comment block is the label" was
  written here as though the shipped files already opened that way. They did
  not — they opened with a *sentence*, and a label is not a sentence
  ("Tonemap (Reinhard)", "SDF box", "HSV to RGB" are derivable from neither
  the path nor the prose). So all 28 files gained a label line, and the rule
  is now: label line, blank line, **one** documentation paragraph, then
  whatever the source's own reader needs. Only that one paragraph reaches the
  editor.
* **`@default` on every parameter, one parameter per line.** Not in the plan
  and not optional: a comment after the second parameter on a shared line has
  nothing to say which of them it belongs to, so the derivation bounds each
  comment by the next parameter's position and a shared line yields nothing.
  Every node source is now one-parameter-per-line. Absent `@default` means
  the input is mandatory, which is why leaving it out is an error worth
  catching rather than a shrug.
* **M3 has a new place to edit.** A socket type is now language-visible:
  when textures and samplers become socket types, `node_from_source`'s type
  mapping is where `texture_2d<f32>` becomes one — one place, but it has to
  move with `ValueType`.

The deleted test, `every_called_function_lives_in_a_module_this_crate_ships`,
was replaced by two that check what is still checkable:
`every_function_node_is_derived_from_a_shader_this_crate_ships` and
`the_generated_table_is_what_the_derivation_answers_now`, the latter
re-deriving every node at test time and comparing it with the generated
table — the agreement test that makes the build script's serializer
trustworthy, in the same spirit as the two MSDF implementations (ADR 0014).

**Still owed to M11**, and unchanged: the runtime caller. Nothing about the
derivation assumes build time, so making a saved buffer appear in the palette
is calling the same function — which was the entire point of writing it as an
API rather than as a build script.

### M1 — The render graph, with today's two paths on it

Rewrite `wxsl-render::pipeline` as a declarative engine, and express forward
and deferred as pass lists built in Rust (not yet as graphs).

* `pass` module: `PassDesc`, `PassState`, `Attachment`, `ResourceId`,
  `ResourceDesc`. `PassState` carries `cull_mode`, `depth_compare`,
  `depth_write`, `blend` — **and the depth format**. `DEPTH_FORMAT` is a
  `const` today (`Depth32Float`, on the grounds that there is "no stencil
  work to do"), which quietly forecloses every stencil technique: portal
  masking, per-object outline masks, shadow volumes. A pass picks its own.
* `ResourceDesc` describes more than a screen-sized 2D target from the
  start, because retrofitting an allocator is worse than over-describing one:
  * **Dimension**: 2D, 2D array, cube, 3D. Cascaded shadows want an array,
    reflection probes want a cube, froxel volumetrics want a 3D texture.
  * **Persistence**: `Transient` (created at first write, reusable after its
    last read) or `Persistent { history: n }` (ping-ponged, survives the
    frame). Without the second, **every temporal technique is out** — TAA,
    temporally stabilised SSR, auto-exposure, trailing bloom. This is the one
    addition that genuinely changes the allocator's shape, so it cannot be
    bolted on later.
* `graph` module in `wxsl-render`: topological order, transient allocation
  with reuse, history rotation for persistent resources, the recorder.
* `MaterialPipelines` learns to key on `(variant, stage, PassState)` so cull
  mode, blend and depth format stop being baked into one hardcoded
  descriptor.
* Scene submission replaces the single mesh: meshes and instances, each
  instance carrying a material and a transform, plus the **tags** it was
  authored with. A geometry pass draws a *tag expression* (`opaque`,
  `transparent`, `opaque && outlined`), so the material says what it is and
  the pass says what it draws, with neither introspecting the other. A draw
  may override its tags for the odd case.
* `wxsl_core::scene::Scene` — meshes, instances, materials, tags — and the
  facade translates it into a `DrawList`. `wxsl_render::scene::Scene` becomes
  `Environment` in the same pass, while there are few callers.
* Instance transforms move into a **storage buffer** in the frame group,
  addressed by `@builtin(instance_index)`, replacing ADR 0010's per-object
  dynamic offset. One binding, one upload, and a shape a culling pass can
  write indices into. The only limit worth noting is `DownlevelFlags::
  VERTEX_STORAGE`, which the WebGPU baseline satisfies and WebGL does not —
  and WebGL is not a target.
* glTF geometry import behind a `gltf` feature, and **indices widen from
  `u16` to `u32`** — 65k vertices is a limit any real mesh crosses, and it is
  cheaper to change while `Mesh::new` has four callers.
* A geometry pass takes its draws from either the scene or an **indirect
  buffer**, so GPU-driven culling and GPU particles become a later pass
  rather than a later redesign of the draw path.
* `PassKind::Compute` exists from the start. This is nearly free: the repo
  already runs a compute pass — MSDF glyph generation (`ui::msdf_gpu`,
  `MSDF_MODULE`, ADR 0014) — so compute pipelines, their bind groups and
  their variant compilation are solved problems here. The render graph
  generalizes what works rather than inventing it. First *used* in M7.
* `gpu.rs` grows feature negotiation: ask for the optional features we want,
  record what was granted in a `DeviceCaps` the passes can query.

**Done when** `render_cube` still asserts the two paths match, the deferred
path is a three-entry pass list, and two tests hold: the transient allocator
reuses one texture across two non-overlapping lifetimes, and a resource
declared `Persistent { history: 2 }` hands a pass the previous frame's
contents.

**ADR** — "A declarative render graph, and material stages instead of a
render-path enum". Amends ADR 0005, ADR 0008 and ADR 0010 (instance transforms
leave the dynamic offset for a storage buffer).

### M2 — Stages in the ABI and codegen

* `abi::MATERIAL_STAGES` table; `RenderPath` retires into it.
* `codegen` emits entry points per requested stage; the variant key gains
  the stage; `write_entry_points`'s format string becomes a loop over the
  table.
* Add `DepthOnly` as the third stage — cheap, and it proves the mechanism
  with something the render graph can immediately use as a depth prepass.
* Pipeline swapping, non-blocking: re-derive the stage set, compile what is
  missing on a worker thread, and keep presenting the previous pipeline
  behind a progress indicator until the new one is complete. The variant
  cache already makes the second swap between two pipelines free, because
  its key holds the stage rather than the pipeline.

**Done when** `graph_to_wgsl` compiles every node in the library for every
stage, a depth prepass in the forward pass list renders identically, and
swapping pipeline mid-session never drops a frame or shows an incomplete one.

### M3 — What a material declares: uniforms, textures, samplers

The prerequisite for M5 through M9, and the first half of a larger idea: a
material graph does not only *compute*, it **declares what it needs from
outside itself**. Three kinds of resource land here. The fourth — what it
needs from the geometry — is M4, and it is the same idea again.

No pipeline work in either.

**Uniform parameters, in group 1.** A `param` node looks like `const` in the
editor and is nothing like it underneath. A `const` is inlined into the
generated WXSL, so editing one compiles a new variant; a `param` is a field
of one uniform buffer, so editing one is a buffer write. That difference *is*
the feature — a slider that does not recompile.

* One node kind, `param.value`, generic over every `ValueType` (ADR 0018's
  rule: one node per operation, not one per operation and type), carrying a
  name and a default.
* `wxsl-core` collects the reachable ones, orders them stably, and computes
  the **WGSL uniform layout**. That is where the trap is: `vec3f` aligns to
  16 bytes, so a naive field order either wastes a third of the buffer or,
  worse, leaves the host and the shader disagreeing about offsets — silently,
  every frame.
* So this **breaks ADR 0008's mirroring rule, deliberately**: there is no
  `#[repr(C)]` struct that can mirror a layout the graph decides. `wxsl-core`
  emits an offset/size table alongside the struct and `wxsl-render` writes
  parameters through it by name. First place in the repo where the Rust half
  of a layout is *computed* rather than written — and the machinery M4's
  instance attributes and M9's bake passes both reuse.
* The variant key gains the parameter **layout**, never the parameter
  **values**. Getting that wrong turns every slider back into a recompile,
  which is the whole thing this milestone buys.

**Textures and samplers, also group 1.** `ValueType::{Texture2d, Sampler,
…}` — naming to settle: these are not values in the same sense a `vec3f` is,
and may want a socket kind of their own rather than a widened `ValueType`.
Then `sample.texture_2d` and friends in `wxsl-stdlib`, which M0 makes cheap:
each one is a file and nothing else.

**The application's own block, in group 2.** Different in kind from the two
above — the material does not *define* this resource, it **requires** one.
The ABI has always reserved the slot for exactly this: group 2's doc says it
is the only group a node definition may bind in.

* The graph declares a named block with typed fields; codegen emits the
  struct and `@group(2) @binding(0) var<uniform> …`.
* The renderer cannot fill it, because it does not own the data. So: **the
  material hands out the `BindGroupLayout`, the application hands back a
  `BindGroup`.** No validation code of ours, and a mismatch is `wgpu`'s own
  error naming the binding rather than a wrong picture.
* The cost, accepted: the layout is per-material, so an application with two
  materials declaring different blocks binds per material rather than once
  per frame. That is the right price — the alternative is one global layout
  every material must agree with, which is the coupling group 2 exists to
  avoid.

**Done when** the demo cube samples a real texture through a graph, dragging
its roughness slider leaves `cache_stats()` reporting zero new misses, and an
application-supplied uniform reaches a node without `wxsl-render` knowing
what is in it.

**ADR** — "A material declares its resources: uniforms, textures, and the
application's slot". Amends ADR 0010's material-group row, and ADR 0008's
rule that a layout's two halves are written twice.

### M4 — The interface a material requires of its geometry

The other half of M3's idea, and the reason it is here rather than inside the
partitioning work: a declared attribute is a **requirement on the geometry**,
exactly as a group-2 block is a requirement on the application, and it wants
the same declare-then-validate shape.

Today both ends are fixed. `VertexIn` is hand-written in
`shaders/wxsl/vertex.wxsl` and mirrored by `mesh::Vertex::ATTRIBUTES` —
position, normal, tangent, uv, four locations. Per-instance data is a model
matrix and nothing else. A material that wants a second UV set, a vertex
colour, a per-vertex wind weight, or a per-instance tint, age, or animation
offset has nowhere to put any of it.

One declaration in the editor, **two backings**, and which one a declaration
gets is decided by its frequency rather than by the author:

#### Per-vertex: vertex buffers

* **The base four stay in the ABI and are always present.** Declared
  attributes are appended at locations 4 and up. Every existing mesh keeps
  working, `mesh::Vertex` is untouched, and a material declaring nothing
  extra compiles to exactly today's vertex stage.
* **One separate vertex buffer per attribute** — not a widened interleaved
  struct. Three reasons: glTF hands them over per accessor anyway (M1), a
  mesh can serve a material that wants colours and one that does not without
  re-uploading its positions, and the base stream stays a `#[repr(C)]`
  struct with a mirror to check. The cost is vertex-buffer slots: WebGPU
  guarantees 8, so **four declared vertex attributes is the budget**, written
  down here rather than discovered when somebody declares a fifth.

#### Per-instance: the storage buffer, widened

* **Not an instance-step vertex buffer.** The obvious route —
  `VertexStepMode::Instance`, one more buffer — loses to the decision already
  settled for instance *transforms*, and for the same three reasons: it is
  one binding however many attributes are declared, it puts no pressure on
  the eight vertex slots the per-vertex half is already spending, and it
  survives M1's indirect draws, where a GPU culling pass emits instance
  *indices* and an instance-step stream would have to be compacted to match.
* So declared per-instance attributes are **fields appended to the instance
  storage buffer**, whose base fields (model matrix, normal matrix) stay ABI
  and always present. Its layout is computed from the graph by the same code
  M3 wrote for uniform parameters — the second customer, which is what
  justifies that code being a table rather than a struct.
* **A fragment stage cannot read `@builtin(instance_index)`** — WGSL offers
  it in the vertex stage only. The fix is one `@interpolate(flat) u32`
  varying carrying the index down, and the fragment stage re-indexes the
  storage buffer itself. One inter-stage location covers *every* declared
  instance attribute, however many, which is strictly better than passing
  each value down; and it is the only reason this half touches varyings at
  all.

#### Both halves

* **Reading one is `input.attribute` with the name as a node parameter**,
  validated against the graph's declared set — not a generated node kind per
  attribute. The alternative makes `NodeRegistry` per-graph, and it is global
  and shared today; one node kind carrying a name keeps it that way. Whether
  a name is per-vertex or per-instance comes from the declaration, so the
  node does not have to say, and moving an attribute from one to the other
  rewires nothing.
* **The geometry says what it provides**, and a mismatch is caught when the
  draw list is built: an error naming the material, the attribute and the
  mesh or instance batch. Never a `wgpu` complaint about vertex buffer 4,
  never a storage buffer read past its stride, and never a frame that merely
  looks wrong.
* **The vertex layout and the instance stride join the variant key, alongside
  the stage (M2).** A shadow stage that reads none of the declared attributes
  should bind none of their buffers and pass no index down, which falls out
  of per-stage reachability — and is the first thing M5's partitioning gets
  to handle with a general mechanism instead of a special case.
* **The 16 inter-stage locations get an accountant.** Declared vertex
  attributes, the flat instance index, and whatever M5's partitioning wants
  to pass between stages all come out of the same budget. Whoever assigns
  `VertexOut` locations assigns all of them, and reports the total rather
  than overrunning it silently.

**Done when** a glTF mesh with a vertex-colour stream drives a material
through a declared attribute; a thousand instances of one mesh read a
per-instance tint from one storage buffer, in the fragment stage, with no
per-instance rebinding; the same material refuses to draw against the plain
cube with an error naming the missing attribute; and a material that declares
nothing at all produces byte-identical WGSL to today.

**ADR** — "A material declares the vertex and instance attributes it
requires". Amends ADR 0008 (`VertexIn` stops being fixed ABI text) and
ADR 0010 again (the instance buffer's width becomes a material's business).

### M5 — Multiple outputs, partitioning, and shadows

The first milestone that changes what a material *is*, and shadows are its
first consumer.

* Vertex output node (position offset, and interpolants the *graph*
  computes — M4 already brought in the ones the geometry supplies) and an
  explicit discard output.
* `codegen` partitions per stage: reachability from the outputs a stage
  needs, per shader stage, duplicating shared nodes.
* `Shadow` stage; a `Shadow map` pass node; automatic Z-buffer generation per
  shadowing light; shadow lookup in `shading.wxsl` so forward and deferred
  get it from the same code, exactly as they get lighting today.
* Storage is a **2D texture array**: one slice per light, one per cascade of
  a directional, fixed resolution per slice, no packing code — and M1's
  `ResourceDesc` already describes the dimension. An atlas with per-light
  resolution is the eventual answer and a contained change when it comes:
  one `ResourceDesc`, plus a rect per light in the frame uniform.
* Filtering is **PCF** over a comparison sampler with a small fixed kernel,
  biased by **normal offset** rather than depth bias. Not a detail: a
  constant depth bias behaves badly on exactly the two things this milestone
  introduces, displaced vertices and perforated alpha. PCSS is a later
  option, and it wants M1's history resources for its noise.
* Land the relative-to-eye ABI hook here (see M10) while the vertex stage is
  already open.
* The `Velocity` stage needs the vertex offset evaluated at `t-1`, not just
  the model matrix at `t-1`. So a time-driven input needs a "previous frame"
  variant, or motion vectors come out wrong on exactly the objects that have
  motion. Settle that shape here rather than in M7, which is only where it
  gets consumed.

**Done when** a material that discards by alpha casts a matching perforated
shadow, and a material that displaces vertices casts a shadow of its
displaced shape — the two things that are only true if the partitioning is
right.

**ADR** — "A material graph spans shader stages, and compiles per pass
stage".

### M6 — Lighting models, by ID in the G-buffer

* A lighting-model registry: each model is a WXSL function of one fixed
  signature, plus a small integer id. **The registry entry is the seam** —
  the generated dispatch cannot tell a hand-written function from one
  generated out of a graph, so hand-written models now cost nothing later:
  graph-authored models become a milestone that adds a producer, not one
  that redesigns the consumer.
* The G-buffer gains a `lighting_id` channel; `GBUFFER_TARGETS` stops being
  a fixed table and becomes base targets plus targets the pipeline requests
  (a clear-coat target only exists if some model needs it).
* `lighting_pass.wxsl` stops being hand-written and becomes *generated* from
  the enabled set: a `switch` over the id calling each model. A model nobody
  enabled costs nothing, because dead-code elimination already runs after
  monomorphization (ADR 0012).
* Ship `lambert`, `phong` and the existing PBR as three models, plus one
  extra-G-buffer model (clear coat) to prove the composition.

**Done when** one frame shows three objects shaded by three different models
through one deferred lighting pass, and the WGSL for a single-model pipeline
contains only that model.

**ADR** — "Lighting models dispatched by a G-buffer id, with a composable
G-buffer layout". Amends ADR 0008.

### M7 — The `Screen` domain and postprocess

* `GraphDomain`, the screen ABI, registry filtering by domain.
* A `Screen effect` pass node taking a screen graph.
* Built-in effects as screen graphs where possible, Rust passes where not:
  bloom (threshold, downsample chain, upsample, add), FXAA, tonemap moves
  here from the shading function, motion blur (needs a `Velocity` stage and
  previous-frame matrices in the frame uniforms).
* Antialiasing is **post-process only**: FXAA or SMAA as a screen graph now,
  TAA once persistent resources (M1) and the `Velocity` stage (M5) are both
  in. No `sample_count` above 1 anywhere, so forward, deferred and peeling
  antialias identically and the editor's preview always represents the final
  frame. TAA pays for itself twice by also stabilising shadow and SSR noise.
* The bloom chain is where `PassKind::Compute` gets its first use, and the
  first place the baseline/native rule applies to something other than
  blending: the baseline path stays a ping-pong of render passes.

**Done when** the editor authors a bloom chain as nodes, and emissive
materials glow.

**ADR** — "Screen-domain graphs: postprocess is a material over the frame".

### M8 — Transparency by dual depth peeling

The hardest pass, and the one the portability answer bites hardest.

* `PeelFront`/`PeelBack` stages; a `Depth peel` pass node with a layer
  count; front-to-back and back-to-front accumulation; the composite. Drawn
  from the `transparent` tag, so no material is classified by inspection.
* **The portability problem, concretely:** classic dual depth peeling keeps
  a near/far depth pair in one `rg32float` target updated by `MAX` blending.
  `rg32float` is **not blendable** on the WebGPU baseline — that needs
  `wgpu::Features::FLOAT32_BLENDABLE`, which nothing currently requests
  (hence the feature negotiation in M1). So: the native path uses the blend
  trick at one geometry pass per layer; the baseline path ping-pongs two
  `rg32float` targets and compares in the shader, at two passes per layer.
  Both tested, which is the cost the portability answer accepted.
* Order-independent alternatives (weighted blended OIT) are a *different*
  pipeline graph, not a fallback — which is the argument for the pipeline
  being a graph at all.

**Done when** four overlapping transparent surfaces composite correctly on
both paths, and the two produce the same image within tolerance.

**ADR** — "Dual depth peeling, and the baseline/native split for blendable
float targets".

### M9 — Bake passes

Precompute part of a material into a texture and sample it back.

* A `Bake` pass node: a material subgraph, a UV-space (or cube) target, and
  an invalidation rule (what changed forces a re-bake).
* Material side: a "baked" node that is transparently either the subgraph or
  a sample of its bake, so switching costs no edit.

**Done when** a material with an expensive noise-driven PBR term bakes it to
a texture, and toggling the bake changes cost but not image.

### M10 — Relative-to-eye rendering (optional)

Deliberately last in execution, but its **ABI hook lands in M5**, because
retrofitting it once many materials read `world_position` is a migration,
while doing it during M5's vertex work is a rename.

* The hook: `SurfaceContext.world_position` becomes camera-relative under a
  macro flag, with `camera.position` becoming the zero vector in that mode.
* The work: model matrices pre-translated by the camera on the CPU in `f64`,
  the camera's own translation removed from the view matrix, and a
  documented rule about which nodes may read absolute world position (none,
  in that mode, without an explicit "absolute origin" uniform).

**Done when** a scene at a 10^7-unit offset from the origin renders without
jitter, and the near-origin image is unchanged.

### M11 — A WXSL code editor in the editor

Unblocked at any time — it depends on the pipeline work not at all — but its
payoff depends on M0, so it is written here rather than early.

What exists already: the code panels, syntax-highlighted by `wxsl-lang`'s
own lexer (ADR 0016), MSDF monospace text, and a single-line `text_field`
used for a node's name. What is missing is multi-line text editing in the
immediate-mode layer: a cursor, a selection, keyboard navigation, scroll to
cursor, and per-buffer undo. That is the bulk of the work, and it is UI
work rather than shader work.

* Editable buffers in `ui`, on top of the existing highlighting.
* Compiler diagnostics in the gutter, from the spans the compiler already
  carries, so a mistake shows where it is rather than in the problem panel.
* Undo — the editor has none at all today, and a text buffer is where that
  gap stops being tolerable.

**Why it pairs with M0:** with derivation in place, saving a buffer makes a
node appear in the palette. Write a function, get a node, wire it into a
graph, read the WGSL it compiled to — in one window, with no restart. Without
M0 it is a viewer you can type into, which is worth much less.

**Done when** a function written in the editor becomes a usable node in the
same session, and a syntax error in it is reported on its own line.

**ADR** — probably none: it adds no new boundary. If editable buffers need a
different text model than the immediate-mode layer's, that is worth one.

## Hard parts, and how each is de-risked

| Risk | Mitigation |
|---|---|
| **Variant explosion**: stages x macros x lighting models x baseline/native. | Compile lazily, key precisely, and let the pipeline graph declare up front which combinations it needs, so they are warmed once instead of stuttering. Track it: `cache_stats()` already exists. |
| **Stage partitioning subtly wrong**: a node duplicated across stages that rounds differently, or a varying that should have been recomputed. | M5's two acceptance tests are exactly the cases that fail if it is wrong. Extend `graph_to_wgsl` to compile every node in every stage, as it already does for every path. |
| **A computed layout disagreeing with what the host writes.** Two of them now: M3's uniform parameters and M4's widened instance buffer. | They are the only layouts in the repo with no `#[repr(C)]` mirror to be checked against, so they get by test what the mirrors get by construction: read every parameter and every instance field back out of the buffer through a trivial graph, at every `ValueType`, including the `f32`-then-`vec3f` ordering that alignment breaks. One layout computer, one test, two customers. |
| **Bind groups**: four, all spoken for. | The allocation table above, decided now. `SceneBindings` is one struct in one file if group 0 needs to grow. |
| **Two implementations of every pass** (baseline and native). | Only where the baseline genuinely cannot express the technique — so far exactly one place, M8's float blending. Any second implementation owes a test asserting the two agree, as `msdf` already does for its CPU and compute generators. |
| **The pipeline graph becoming a second, worse editor.** | It reuses `wxsl-core`'s model, `wxsl-editor`'s canvas and the same registry. If a pipeline node needs a mechanism material nodes lack, that is a signal to generalize the mechanism, not to fork the editor. |
| **`MAX_LIGHTS = 4` and one draw per frame** meeting a pipeline with shadows and peeling. | M1 replaces the draw list. The light count is a separate, smaller job (clustered or tiled lighting) that the render graph makes additive: a compute pass and a bind group, not a redesign. |

## Costs accepted

Nothing in this plan is open any more; the questions this section used to
hold are answered and folded into the table and the milestones. What is left
is the bill those answers come with, recorded so nobody reads it later as a
surprise or a defect:

* **`u16` to `u32` indices.** glTF import makes the 65k-vertex ceiling real.
  Cheap now (`Mesh::new` has four callers), annoying once a mesh cache, an
  asset format and a peel pass all index geometry.
* **`DownlevelFlags::VERTEX_STORAGE`.** Instance transforms in a storage
  buffer read from the vertex stage are fine on the WebGPU baseline and
  unavailable on WebGL. WebGL is not a target, so this is a cost only if
  that ever changes — at which point the fallback is the instance-buffer
  shape ADR 0010's dynamic offset was already close to.
* **A blocking pipeline swap on single-threaded wasm.** `wgpu` exposes no
  asynchronous pipeline creation, so background compilation is a worker
  thread. Where there is no thread there is no background. The progress
  indicator is what keeps that honest rather than mysterious.
* **Four declared vertex attributes, not more.** WebGPU guarantees 8
  vertex-buffer slots and the base stream takes one, so a material may
  require four extra per-vertex attributes. Interleaving them into a second,
  material-specific stream is the escape hatch, and it costs the
  per-attribute upload independence that made separate buffers the choice.
  Per-*instance* attributes are not subject to this at all, which is half the
  reason they are storage-buffer fields.
* **One inter-stage location, whenever a fragment stage reads an instance
  attribute.** The flat `instance_index` varying. Cheap, fixed regardless of
  how many attributes are declared, and unavoidable: WGSL does not offer the
  builtin outside the vertex stage.
* **Group 2 is bound per material, not per frame.** The price of letting a
  material declare the layout it expects there instead of every material
  agreeing on one. An application that wants a single binding for everything
  can still have it — by declaring the same block in every material, which is
  then its choice rather than the engine's rule.
* **Two implementations, in exactly one place.** M8's float blending. Every
  other pass has one path. If a second one ever appears elsewhere, it owes a
  test asserting the two agree — the rule `msdf` already lives by.
