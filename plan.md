# Plan: from a material compiler to a pipeline editor

Written in English to sit alongside `AGENTS.md`, `docs/architecture.md` and
the ADRs, which future sessions read as one body of text. This file is a
*plan*, not a record: when a milestone lands, its decisions move into an ADR
and the corresponding section here shrinks to a link. Delete it when the last
milestone is done.

**Landed so far:** M0, M1, M2, M3, M4, M5, M6.

## How to read this

Everything below follows from this table. These are settled: revisit one by
superseding its row, not by quietly diverging inside a milestone.

| Decision | Chosen |
|---|---|
| What a document holds | **A scene — and the pipeline is not in it.** The unit is a *scene*: meshes and instances, each instance carrying a material. It says what exists, never how it is drawn. It is pure serializable data, so it lives in `wxsl_core::scene`, and the renderer keeps consuming a draw list — which is the decision its own doc already records, that batching, culling and sorting belong to the application. `wxsl_render::scene::Scene` is renamed `Environment` (camera, lights, ambient) to end the collision. |
| Who owns the pipeline | **The renderer.** A pipeline is swappable at any moment and swapping re-evaluates everything, because the pipeline is what decides how a material is split. Pipelines live in their own files, so `wxsl-stdlib` can ship a standard deferred one. |
| Swapping a pipeline | **Non-blocking: the previous pipeline keeps presenting.** Everything declarative — stage set, target descriptors, pass order — is re-derived at once because it is only data. Missing variants compile in the background behind a `compiling 3/7` indicator, and the swap lands in one frame when they are ready. No freeze, no black frame, no half-drawn scene. |
| Mesh assets | **glTF, behind a `gltf` feature.** Geometry only at first: positions, normals, tangents, uv, indices — no materials, no skins. A scene of primitives cannot demonstrate shadows or depth peeling convincingly, and a bespoke format's converter would have to parse glTF anyway. |
| Instance transforms | **A storage buffer indexed by `@builtin(instance_index)`.** One binding, one upload, no per-draw rebind — and the only shape that composes with M1's indirect draws, since a GPU culling pass emits instance *indices*. Amends ADR 0010's per-object dynamic offset. *M4 did not extend this buffer after all — a second array beside it, at binding 3, because this one is read at the ABI's stride by hand-written code that cannot know a material widened it.* |
| Where pipeline configuration lives | **Two graph kinds.** A frame-level *pipeline graph* reuses `wxsl-core`'s node/socket/type model and is edited in its own canvas. A *material graph* stays per-surface, and codegen splits it across whatever stages the pipeline asks for. |
| Portability target | **WebGPU baseline, native fast paths behind a feature.** Every pass must have a path that runs on the WebGPU baseline; a native path may be faster or higher quality, and both are tested. |
| Culling | **Convention, as a per-pass knob.** The opaque pass culls back faces; an outline pass culls front faces. `cull_mode` is a field of a pass description, never a constant in the code. |
| How an object picks its queue | **Tags on the material, a tag expression on the pass.** The material declares what it *is* (`opaque`, `transparent`, `outlined`); a geometry pass declares what it *draws*. No introspection of graphs, and a draw may override its tags. |
| How a node is declared | **The `.wxsl` file is the definition.** *Done — [ADR 0020](docs/adr/0020-a-node-definition-is-derived-from-its-wxsl-source.md).* Its signature, its returned struct and its `@macro const`s derive into sockets, outputs and macro declarations; its leading comment block carries the label and doc, and `// @default` annotations carry socket defaults. The derivation is a public `wxsl-lang` function, called at build time for the stdlib and at runtime for files the user writes. |
| How a material declares a uniform | *Done — [ADR 0023](docs/adr/0023-a-material-declares-its-resources.md).* **A `param` node, with the layout computed rather than mirrored.** `const` inlines into the shader, so editing one compiles a new variant; `param` is a field of group 1's uniform buffer, so editing one only writes bytes. `wxsl-core` computes the WGSL layout — `vec3f`'s 16-byte alignment makes a hand-written Rust mirror impossible — and emits an offset table `wxsl-render` writes through. |
| How a material uses the application's slot | *Done — [ADR 0023](docs/adr/0023-a-material-declares-its-resources.md).* **It declares a block and hands out the layout; the application hands back a bind group.** Group 2 stays the application's and holds nothing of ours, but a material may now state what it expects to find there. `wgpu` validates the match, so there is no checking code of ours to keep in sync. |
| How a material requires a *vertex* attribute | *Done — [ADR 0024](docs/adr/0024-a-material-declares-the-geometry-it-requires.md).* **The base four are ABI and always present; declared extras are separate vertex buffers at locations 4 and up.** Read with one `input.attribute` node carrying the name, so `NodeRegistry` stays global and static. A mesh that lacks a declared attribute is reported when the frame is compiled, naming both sides. |
| How a material requires an *instance* attribute | *Done — [ADR 0024](docs/adr/0024-a-material-declares-the-geometry-it-requires.md).* **A storage array in the frame group, not an instance-step vertex buffer.** Same declaration in the editor, different backing, and for the reason already settled two rows up: one binding however many attributes, no vertex-slot pressure, and it survives a GPU culling pass that emits instance *indices*. A fragment stage reads them by passing `instance_index` down as one flat varying and re-indexing. It is a *second* array beside the transforms rather than a widening of them — see the ADR. |
| Lighting models | **A registry of WXSL functions, hand-written first.** A model is one function of fixed signature plus a small id; whether that function was written or generated is invisible to the dispatch, so graph-authored models land later against the same registry. |
| Compute passes | **In the engine from M1, first used in M7.** The repo already runs a compute pass — MSDF generation (ADR 0014) — so the pipeline, bind-group and variant machinery exists and the render graph generalizes it rather than inventing it. |
| Antialiasing | **Post-process only.** No MSAA anywhere: FXAA/SMAA in M7, TAA once persistent resources exist. One behaviour across forward, deferred and peeling, so the editor's preview always represents the final frame. |
| Shadows | **A texture array, one slice per light or cascade, PCF with a normal-offset bias.** Atlas packing is a later, contained change; normal-offset rather than depth bias because materials will displace and discard. |
| First pipeline milestone | **The render graph first.** Re-express forward and deferred on a declarative pass engine, adding no visible feature, so everything after it is additive rather than a refactor. M0 precedes it as independent groundwork. |

## Where we are

Honest baseline. The first, third and fifth entries below are struck
through because M1 closed them; they stay listed so a reader can see what
the milestones were aimed at.

* ~~**Passes are hand-written Rust.**~~ Closed by M1: a pipeline is a list
  of `PassDesc`s, and `wxsl_render::graph` orders, allocates and records
  them.
* ~~**Render path is a two-valued enum.**~~ Closed by M2: stages are a
  table (`abi::MATERIAL_STAGES`), codegen emits one module per stage, and
  the flag is gone.
* ~~**One draw per frame.**~~ Closed by M1: `RenderRequest` takes a
  `DrawList`, and every draw's transform is a row of one storage buffer.
* ~~**The material bind group is not bound.**~~ Closed by M3: a graph
  declares its own uniform parameters, textures and samplers, and
  `wxsl_core::resources::MaterialInterface` is what codegen emits and the
  renderer binds. Group 2 is bound too, when a graph declares the block it
  expects the application to supply.
* ~~**No device feature negotiation.**~~ Closed by M1: `gpu.rs` asks for the
  optional features the later milestones want and records what was granted
  in `DeviceCaps`.
* **Four lights, no shadows, no transparency.** `MAX_LIGHTS = 4` in a fixed
  uniform array; `Light` has no shadow data; nothing anywhere blends.

What *is* solid and should not be disturbed: the graph model and its typing,
the WXSL compiler and its template monomorphization, the variant cache's
shape (`(source+macros hash, stage)`), the ABI-as-tables idea, the computed
layout with its two customers, and the editor's canvas — a pipeline graph
is a second canvas over the same model, not a second editor.

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

*Done — [ADR 0022](docs/adr/0022-material-stages-replace-the-render-path-enum.md).*

`RenderPath` stops being the compile axis. A surface graph compiles once per
**stage**, where a stage says which entry points to emit and what the
fragment stage returns:

| Stage | Fragment returns | Which part of the graph it needs |
|---|---|---|
| `ForwardLit` | `vec4f` final colour | everything |
| `GBuffer` | the G-buffer struct | everything except lighting |
| `DepthOnly` | nothing | position, and alpha if the material discards |
| `Shadow` | nothing | same as `DepthOnly`, from a light's view (the bias turned out to belong in the lookup, not the pass) |
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
| 0 `frame` | the shadow atlas and its per-light matrices — produced once per frame, read by every lit pass, so it belongs with the lights it comes from, not in a pass group. *M4 added binding 3, a second array holding whatever per-instance attributes a material declares; the transform array at binding 2 stayed fixed.* |
| 1 `material` | *Bound, as of M3.* the material's **uniform parameters** — one buffer whose layout is computed from the graph — plus its own textures and samplers, and baked lookups (M9). Its layout is per material, so the pipeline cache keys on the interface's *shape*. |
| 2 `user` | *Done in M3.* still the application's, and still nothing of ours in it — but a material may now **declare the layout it expects** there and hand it out, so the application binds against the material rather than against a convention. **This supersedes this row's earlier "untouched"**: the slot's ownership is unchanged, what is new is that the material can describe it. |
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

### M1 — The render graph, with today's two paths on it — **done**

Landed as [ADR 0021](docs/adr/0021-a-declarative-render-graph-and-a-scene-document.md),
which is now where the reasoning lives. What shipped, and the four things a
later milestone needs to know:

`wxsl_render::pass` describes a pass and `wxsl_render::graph` runs a list of
them. `ForwardPipeline`, `DeferredPipeline` and the `Pipeline` trait are
gone; `pipeline::forward_graph` and `pipeline::deferred_graph` build the two
stock lists, and `Renderer::set_graph` takes anyone else's.
`wxsl_core::scene::Scene` is the document, `wxsl_render::environment::Environment`
is the camera and lights, `wxsl::scene::SceneResources` is the translation
between them, and instance transforms are one storage buffer in the frame
group. `u32` indices, glTF import behind a `gltf` feature, and `DeviceCaps`
all landed as planned.

* **The deferred pass list is two passes, not the three this plan
  predicted.** G-buffer, then lighting. There is no honest third at M1: the
  depth prepass belongs to M2 (it needs the `DepthOnly` stage) and a
  present/tonemap pass would be pure waste, because the ABI's shading
  function already encodes sRGB. The pass *count* was never the point; the
  pass *list* was.
* **`Read` carries a history depth, and that is what makes the ordering
  work.** A read of `history: 0` is an edge from whoever wrote it; a read
  further back is no edge at all. Without that distinction a temporal pass
  is a cycle, and TAA would have needed the scheduler rewritten.
* **A pass may not sample what it attaches**, and the scheduler says so by
  name. Loading an attachment is the supported way to read what is already
  there. This was found by the test that expected a *cycle* and got a valid
  schedule: they are two different mistakes and only one of them is a cycle.
* **Buffers are not graph resources yet.** Only textures are allocated,
  aliased and rotated; an indirect buffer is handed to a pass directly. That
  is enough for M2 through M7 and is an extension rather than a rewrite when
  a compute pass wants a transient buffer.

**Still owed**: `MaterialPipelines` was to key on `(variant, stage,
PassState)`; it keys on `(variant, PassState, target formats)` because the
stage does not exist until M2, and `variant` already carries the path.

### M2 — Stages in the ABI and codegen — **done**

Landed as [ADR 0022](docs/adr/0022-material-stages-replace-the-render-path-enum.md),
which is now where the reasoning lives. What shipped, and the four things a
later milestone needs to know:

`abi::MATERIAL_STAGES` is the table and `MaterialStage` is an index into
it; `forward_lit`, `gbuffer` and `depth_only` are its rows. Codegen emits
one module *per stage* rather than one module with every stage's entry
point `@if`-gated inside it, so `abi::FEATURE_DEFERRED` and
`abi::FRAGMENT_ENTRY` are both gone. `RenderPath` retired into
`StockPipeline`, which names one of the two pass lists and has nothing to
do with variants. The forward pass list is now a depth prepass plus a
shading pass, and `Renderer::request_pipeline` swaps pipeline on a worker
thread while the previous one keeps presenting.

* **A stage's module is per stage, not per pipeline, and that is what M5
  needs.** Gating would have been the faithful extension of ADR 0005, but a
  stage will soon need a different *body* — the shadow stage is the alpha
  subgraph and nothing else — and a module that is already per stage has
  somewhere to put that. It also means the source hash separates stages
  before the cache key's `stage` field even looks.
* ~~**The depth-only module still carries the whole material function.**~~
  Closed by M5's partitioning: a stage compiles only the subgraphs
  reachable from the terminals it needs, so an ordinary material's
  prepass module is now the vertex entry and nothing else — no material
  function, and no fragment stage at all.
  `every_stage_compiles_from_the_same_graph` asserts both directions.
* **The prepass tests `LessEqual`, not `Equal`.** WGSL promises nothing
  about two pipelines running the same vertex code producing bit-identical
  clip positions unless the position builtin is marked `@invariant`, which
  this ABI does not emit. `Equal` turns a one-ulp difference into a hole in
  the surface; `LessEqual` costs nothing measurable.
* **Only the WXSL-to-WGSL half of a swap is off-thread.** It is a pure
  function over text with no device in it. Creating the `wgpu` modules
  happens on the render thread as results arrive, one poll at a time, so
  that cost is spread over the frames the swap was taking anyway. `wgpu`
  still has no asynchronous *pipeline* creation, so the first draw with a
  freshly swapped-in variant can still cost a backend compile — the swap
  removes the shader hitch, not every hitch.

~~**Still owed**: the `Shadow`, `PeelFront`/`PeelBack` and `Velocity`
rows.~~ `Shadow` landed in M5 and is `depth_only`'s shape rendered from a
light's view; `PeelFront`/`PeelBack` and `Velocity` are still owed, and
still not stubbed out, because each needs something that does not exist
yet — the peel test, a previous-frame transform. M5 settled half of the
second: `wxsl_previous_frame` re-evaluates a time-driven graph for the
previous frame, so what `Velocity` still needs is the *transform* at
`t-1`, not the graph.

### M3 — What a material declares: uniforms, textures, samplers — **done**

Landed as [ADR 0023](docs/adr/0023-a-material-declares-its-resources.md),
which is now where the reasoning lives. What shipped, and the five things
a later milestone needs to know:

`wxsl_core::resources` computes a `MaterialInterface` from the reachable
part of a graph — the parameter layout, the textures and samplers with
their bindings, and the block the application supplies — and it is
carried on `GeneratedShader`, so codegen emits the declarations from the
same table `wxsl_render::bindings` builds the bind groups from. Three
node bodies declare (`param.value`, `texture.*`, `input.user`), each
carrying a *setting*: a string a node instance holds that changes what it
compiles to. `ValueType` gained `Texture2d`, `TextureCube` and `Sampler`,
deliberately outside `ValueType::ALL`. Groups 1 and 2 arrive on the draw.

* **The layout computer is the thing M4 reused, and it is why it is a
  table.** `BufferLayout::uniform` orders fields by alignment, widest
  first, then by name, and both writes and reads go through it. M4's
  per-instance attribute row is the second customer, and it needed one
  branch: the storage address space does not round a struct's alignment
  up to 16.
* **A setting is a third kind of per-instance data**, distinct from
  ADR 0019's label and colour. M4's `input.attribute` was the next user
  of it and needed no new concept, as expected.
* **`bool` is a `u32` in the buffer**, because WGSL's uniform address
  space has no `bool` at all. `BufferLayout::read_expr` is the one place
  that knows, and it emits `(material.flag != 0u)`.
* **Binding 0 of group 1 is reserved for the parameter buffer even when
  there is none**, so a graph gaining its first parameter cannot renumber
  the textures above it.
* **The depth-only stage declares the textures too.** *Half closed by
  M5.* The prepass module no longer *calls* `textureSample` — it does not
  contain the material function at all — but the declarations are still
  emitted and group 1 is still in its pipeline layout, because the
  interface is deliberately per material rather than per stage: two bind
  groups for one object would cost more than an unused binding does. So
  a textured material still has to be bound before its depth pass runs,
  and that is now a decision rather than a gap.
* **The layout is only checked on a GPU.** Every other host-shared layout
  here has a `#[repr(C)]` mirror and a test on the sizes; this one has no
  second half, so `crates/wxsl/tests/material_resources.rs` checks it
  against hardware at every `ValueType`, comparing what the shader read
  with a literal the compiler inlined. That file is what notices if the
  alignment rules are ever got wrong.

**Deviations worth naming:**

* **`convert.to_float` was not in the plan.** `param.value` is generic
  over every value type, and nothing in the library consumed an `i32`,
  `u32` or `bool` — so an integer parameter was a value with nowhere to
  go, and the coverage test skipped it. One node, three lines, and the
  gap the test had been documenting since M0 closes with it.
* **The editor can name a texture but not supply one.** The preview binds
  a magenta checker to anything a graph declares, because there is no way
  to *author* an image yet: a scene document names meshes and materials
  and not pictures. Visible placeholder rather than a black surface, and
  it stays that way until M9 gives an image a name.
* **`SceneResources` grew two steps rather than one.**
  `create_bindings` then `upload`, with the gap in between being where an
  application supplies the textures the document could not. A single
  `bind` would have had to either fail on any textured material or
  silently draw it wrong.

### M4 — The interface a material requires of its geometry — **done**

Landed as [ADR 0024](docs/adr/0024-a-material-declares-the-geometry-it-requires.md), which is now where the reasoning lives. What
shipped, and the five things a later milestone needs to know:

`Graph::attributes` is a list of `AttributeDecl { name, ty, frequency }`,
graph-level like the application block and for the same reason.
`input.attribute` reads one by name and does *not* say which frequency,
so moving an attribute between backings rewires nothing. Per-vertex means
a vertex buffer of its own at slot 1 and up, matched to the mesh's stream
by name; per-instance means a row of a second storage array in the frame
group, indexed by the same `@builtin(instance_index)` and reached in the
fragment stage through one flat varying. Nothing in `wxsl-stdlib/shaders`
changed at all.

* **The instance transform buffer was not widened, and must not be.**
  The plan said to append declared fields to it. On hardware that put a
  thousand quads in a thousand wrong places, and the reason generalizes:
  `transform_vertex` reads that array through the ABI's `Instance` struct
  at the ABI's stride, so a row that grew is read at the wrong offsets by
  hand-written code with no way to know it grew. The declared attributes
  are their own array at `abi::BINDING_INSTANCE_ATTRIBUTES`. Two arrays,
  one instance index, neither knowing the other's width.
* **The extended IO structs sit beside the ABI's, never instead of
  them.** An entry point may take several IO parameters and return one,
  which decides everything: the vertex entry takes `VertexIn` *and*
  `MaterialVertexIn`, the fragment entry takes `VertexOut` *and*
  `MaterialVaryings`, and only the vertex entry's return is a generated
  struct repeating the base locations. That is why
  `abi::VERTEX_OUT_FIELDS` had to become a table — M5's partitioning,
  which will add interpolants of its own, writes into the same place.
* **`abi::MAX_VARYING_LOCATIONS` has one accountant**, in
  `Graph::check_attributes`. M5's graph-computed interpolants spend from
  it, counted there rather than in a second place, and they are numbered
  after everything the geometry brings so that declaring one never moves
  a vertex attribute's location.
* **Per-stage narrowing is still owed at the *binding* level.** M5
  narrowed the code — a depth stage compiles no surface — but not the
  interface: a depth pipeline still declares every vertex buffer and
  still passes the instance index down, because
  `MaterialInterface::geometry` is per material. That is deliberate
  twice over: the instance array stays one upload for the whole frame,
  and a vertex layout that changed per stage would be a second pipeline
  for the same mesh. Narrowing it would want the *declarations* to be
  reachability-driven too, which is a bigger change than it looks.
* **The layout computer now has both its customers**, which is what the
  table shape was for. `crates/wxsl/tests/probe/mod.rs` holds one probe
  graph and both GPU test files run it — once in the uniform address
  space, once in the storage one. Anything that touches
  `wxsl_core::resources` should be run against both.

**Deviations worth naming:**

* **A fourth binding in the frame group, which ADR 0010 allocated by
  update frequency.** The attribute array changes per frame like its
  neighbours, but *what is in it* is a material's decision, which is the
  first time anything in group 0 is. It is in the layout unconditionally
  so the group's shape stays fixed and no pipeline layout is invalidated;
  only the buffer behind it varies.
* **"Byte-identical WGSL" held, and is asserted structurally.** No
  `.wxsl` file changed and codegen's plain path is untouched, so a
  material declaring nothing generates exactly what it did before.
  `declaring_nothing_generates_what_it_always_did` asserts the plain
  vertex entry is there and that none of the invented names is —
  cheaper to keep true than a stored snapshot, and it fails for the same
  reasons.
* **The editor can read an attribute but not declare one.** Exactly where
  textures were left by M3, and for the same reason: the preview invents
  a positional gradient per per-vertex stream and a **one** per
  per-instance field, because the editor is not the application.
  Declaring one is a document edit. One rather than zero, unlike the
  magenta checker's "make the placeholder visible" rule: the editor draws
  a single object, so there is no per-instance variation to show, and
  zero multiplied into a base colour is a preview that went dark for a
  reason the author cannot see.
* **`MeshData::extend` drops a stream only one side carries.** An
  importer merging a file's primitives has no value it could honestly
  invent for the vertices of a primitive with no colours, and a stream
  half-filled with an invented one draws, wrongly.
* **glTF brings across `COLOR_0` and `TEXCOORD_1` and nothing else.**
  Joints and weights want a skinning stage that does not exist, and
  naming every semantic a file might carry is a vocabulary nobody agreed
  to.
* **The demo graph gained a per-instance tint, and `SceneResources` a
  hole to fill it through.** `--instances 6` is now six differently
  coloured cubes out of one draw list and one bind group, which is the
  shortest statement of what the milestone bought. A scene document
  cannot name a per-instance value — it holds one material and many
  instances — so `SceneResources::instance_attributes_mut` is where the
  application supplies it, the same gap `create_bindings` leaves for
  textures. One copy gets white, the identity for the multiply, so every
  reference image in `render_cube.rs` is unchanged.

### M5 — Multiple outputs, partitioning, and shadows — **done**

Recorded as [ADR 0025](docs/adr/0025-a-material-graph-spans-shader-stages.md) (a graph spans shader stages),
[ADR 0026](docs/adr/0026-shadows-a-view-per-light-and-two-flags-on-the-material.md) (shadows), and [ADR 0027](docs/adr/0027-a-graph-computes-its-own-interpolants.md) (interpolants the graph
computes).

Five things a later milestone needs to know, and one that is not obvious
from the ADRs:

* **The blocker was not the texture or the filter — it was that a pass
  could not change its point of view.** Everything else about shadows was
  ordinary. `PassDesc::view` plus a dynamic offset on the frame group's
  camera binding is the whole mechanism, and it is what reflection
  probes, portals and any second camera will use.
* **The shadow maps are in the *frame* group**, not the pass group, so
  `shading.wxsl` has one lookup and both render paths inherit it. That
  makes them the one resource read from outside the pass list, which is
  why `RenderGraph::declare_shadow_maps` exists and why `FrameBindings`
  keeps two bind groups per shape — a pass writing the maps must not also
  have them bound.
* **One shadow pass per light *slot*, always.** A pass list is built once
  and scheduled against every frame; which lights cast changes whenever
  the application says so. An unused slot is a clear, which reads as
  fully lit.
* **`cast_shadow` is a selection and `receive_shadow` is code.** Two
  flags an author sets together, landing in different places for a
  reason. In the deferred path `receive_shadow` is effectively per frame
  rather than per material — the lighting pass is one draw compiled
  against one macro set — which is the same limitation `wxsl_tonemap` has
  always had, and needs a G-buffer bit to fix.
* **A computed interpolant is a third attribute *frequency*, not a second
  vocabulary.** `input.attribute` reads all three, so moving a value from
  `computed` to `instance` when it turns out to be uniform per object is
  a one-line edit to the declaration.

**Done when** a material that discards by alpha casts a matching
perforated shadow, and a material that displaces vertices casts a shadow
of its displaced shape — the two things that are only true if the
partitioning is right. Both are in `tests/shadows.rs` and pass on
hardware, beside a test that forward and deferred agree pixel for pixel.

**Still owed, and deliberately:**

* **An atlas with per-light resolution**, instead of a fixed-resolution
  array slice. A shadow needs more texels the closer its light is to what
  it falls on. Contained when it comes: one differently-shaped binding
  plus a rect in the light.
* **Cascades**, and with them a shadow box that follows the camera rather
  than sitting on the origin. `Attachment::layer` and the array already
  describe the shape.
* **Point-light shadows**, which need six faces to one light's one slice.
  A point light asking for a shadow gets none today rather than a wrong
  one.
* **The demo does not show a shadow.** `pbr_cube` has no ground plane, so
  there is nothing for its cube to cast onto. A small change, and a
  visible-regression risk to the images the README ships.
* **The relative-to-eye hook is only half of M10.** The flag is correct
  and changes no pixel near the origin (there is a test); the precision
  it exists for needs model matrices pre-translated on the host in `f64`.
* **Nothing in `wxsl-stdlib` computes an interpolant yet.** The mechanism
  is there for the nodes a later milestone wants — a per-vertex wind
  phase, a triplanar blend weight.

### M6 — Lighting models, by ID in the G-buffer — **done**

Landed as [ADR 0028](docs/adr/0028-lighting-models-dispatched-by-a-g-buffer-id.md),
which is now where the reasoning lives. What shipped, and the four things a
later milestone needs to know:

`wxsl_core::lighting` is the registry: a `LightingSet` per pipeline (the
default is one model — the library's PBR — which generates no dispatch and
no id channel), four shipped models (`lambert`, `phong`, `pbr`,
`clearcoat`) under `shaders/lighting/models/`, and the generators that
produce the shading function, the G-buffer struct/pack and the whole
lighting pass from the set. `shading.wxsl`, `deferred.wxsl` and
`lighting_pass.wxsl` are gone. The demo grows `--models`/`--model`.

* **Every vec4 G-buffer target costs 8 bytes per sample against WebGPU's
  32-byte attachment floor, whatever its bit depth** — so the base three
  targets have nearly spent the budget before any model asks. The id
  channel is a *scalar* target (1 byte), clearcoat's request a *pair* (4);
  `GBufferPrecision` grew those two variants and the generated struct's
  field types follow them. `pipeline::gbuffer_bytes_per_sample` mirrors
  the spec's arithmetic and `Renderer::set_lighting` checks a set against
  the floor by name — the naive byte count was wrong here once already.
* **The forward path needs no set and no dispatch at all**: each
  material's module gets its own model pasted in directly, so a forward
  module contains only the model its material uses. The set is a deferred
  concept, and the strongest test in `tests/lighting_models.rs` is that
  forward and deferred still agree pixel for pixel with mixed models in
  the frame.
* **Materials name their model, never the id** — a scene document that
  stored ids would silently reshade when one was renumbered. `None`
  resolves to the library default (PBR) when enabled, else the lowest id,
  so widening a set does not quietly reshade materials that named nothing.
* **The registry entry is the seam, as planned.** The dispatch, the
  G-buffer layout and the lighting pass cannot tell a hand-written model
  from a generated one, so graph-authored models later are a new producer
  of entries — nothing downstream changes.

### M7 — The `Screen` domain and postprocess

* `GraphDomain`, the screen ABI, registry filtering by domain.
* A `Screen effect` pass node taking a screen graph.
* Built-in effects as screen graphs where possible, Rust passes where not:
  bloom (threshold, downsample chain, upsample, add), FXAA, tonemap moves
  here from the shading function, motion blur (needs a `Velocity` stage
  and previous-frame matrices in the instance rows — the graph half is
  already settled, see M5's `wxsl_previous_frame`).
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
both pipelines, and the two produce the same image within tolerance.

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

Deliberately last in execution. **The ABI hook landed in M5**, because
retrofitting it once many materials read `world_position` is a migration,
while doing it during M5's vertex work was a rename.

* ~~The hook~~ **Done.** `wxsl_relative_to_eye` makes
  `SurfaceContext.world_position` camera-relative, and `eye_position()`
  is exactly the zero vector in that mode. The documented rule turned out
  to want a *function* rather than a rule: `world_origin()` in
  `bindings.wxsl` is the explicit way back to absolute space, and the two
  things in the ABI that need it — a point light's falloff and the shadow
  lookup — take it, so the flag is already correct.
  `measuring_world_space_from_the_eye_changes_no_pixel_near_the_origin`
  is half the acceptance criterion below, passing today.
* The work: model matrices pre-translated by the camera on the CPU in `f64`,
  and the camera's own translation removed from the view matrix. Until
  that lands the flag buys nothing — the numbers are still absolute `f32`
  by the time they reach the shader — which is exactly why it is worth
  having the shape settled and tested first.

**Done when** a scene at a 10^7-unit offset from the origin renders without
jitter, and the near-origin image is unchanged (the second half already
holds).

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
| **Stage partitioning subtly wrong**: a node duplicated across stages that rounds differently, or a varying that should have been recomputed. | *Addressed.* `graph_to_wgsl` compiles every node in every stage, so a partition that fails to compile is caught; `tests/shadows.rs` catches a partition that compiles and is *wrong*, because a perforated shadow and a displaced one are only right if the alpha test and the displacement reached the shadow pass. |
| **A computed layout disagreeing with what the host writes.** Two of them: M3's uniform parameters and M4's per-instance attribute row. | *Done.* They are the only layouts in the repo with no `#[repr(C)]` mirror to be checked against, so they get by test what the mirrors get by construction — and by the *same* test: `tests/probe/mod.rs` holds one probe graph, `material_resources.rs` runs it over the uniform address space and `material_geometry.rs` over the storage one, at every `ValueType`, comparing what a shader read with a literal its compiler inlined. The instance *transform* row was deliberately left mirrored rather than computed, which is what closed the one bug this risk was really about. |
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
