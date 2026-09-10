# 21. A declarative render graph, and a scene document the pipeline is not in

Date: 2026-09-10

Status: Accepted, amended by [0022](0022-material-stages-replace-the-render-path-enum.md)

Amends [0005](0005-render-pipeline-abstraction-and-shader-switching.md),
[0008](0008-surface-graphs-and-a-named-shader-abi.md),
[0010](0010-four-bind-groups-allocated-by-update-frequency.md)

## Context

The renderer worked and was stuck. Three facts, all of them load-bearing for
everything the project wants next — shadows, transparency, postprocess,
bakes:

* **A pipeline was a Rust struct.** `ForwardPipeline` and `DeferredPipeline`
  each owned their attachments, their depth texture and a pipeline cache,
  and each wrote `begin_render_pass` out by hand. A fourth pass meant a
  fourth struct repeating all of it, and a *user-defined* pass meant nothing
  at all, because there was no way to describe one.
* **A frame drew one object.** `Renderer::render` took one mesh and one
  material. `FrameInput` already carried `draws: &[Draw]`, so the plumbing
  underneath was ready and nothing above it built a list.
* **There was no scene.** `wxsl_render::Scene` was camera and lights — the
  *environment* — and the name was already taken by the thing that did not
  exist. Nothing in the workspace could say "these meshes, with these
  materials, at these transforms".

The forcing question was where the pipeline lives. A scene that contains its
own pipeline cannot be re-rendered by a different one, and a pipeline that
lives in the scene document has to be serialized alongside geometry that has
nothing to do with it. The answer decides what a document *is*.

## Decision

**A pipeline is data, and the renderer owns it. A scene is data, and the
application owns it. Neither contains the other.**

### A pass list, not a pipeline struct

`wxsl_render::pass` describes a pass — `PassDesc`, `PassState`,
`Attachment`, `ResourceDesc` — and `wxsl_render::graph` runs a list of them.
Nothing in either module records anything by hand:

* `RenderGraph::schedule` is a **pure function**: it validates the list,
  orders it by what each pass reads and writes, and decides which physical
  texture serves each resource. No device, so it is tested without one —
  which matters, because CI has no GPU.
* Ordering comes from the resources, never from declaration order. A pass
  list written back to front schedules the same as one written front to
  back.
* `ResourcePool` owns the textures and is reconfigured only when the target
  size or the schedule changes.
* `Renderer::set_graph` hands the whole thing over: an application with its
  own pass list gets one, and the stock forward and deferred lists are just
  two functions in `pipeline.rs` that build one.

Two things in `ResourceDesc` are there before anything uses them, because
retrofitting either would mean rewriting the allocator rather than extending
it:

* **`Dimension`** — 2D, 2D array, cube, 3D. Cascaded shadows want an array,
  reflection probes a cube, froxel volumetrics a volume.
* **`Persistence::Persistent { history: n }`** — a ring of `n + 1` textures,
  rotated per frame, so a pass can read what the previous frame wrote.
  Without it *every* temporal technique is out: TAA, stabilised SSR,
  auto-exposure, trailing bloom. Reading history deliberately creates **no
  ordering edge**, which is what keeps a temporal pass from being a cycle.

`PassState` carries the depth format per pass. `DEPTH_FORMAT` remains as a
default, not a law: a crate-wide `Depth32Float` quietly forecloses every
stencil technique — portal masking, outline masks, shadow volumes — and
whether a pass wants a stencil is a property of that pass.

`PassKind::Compute` exists from the start. It is nearly free: the repo
already runs a compute pass (MSDF glyph generation, ADR 0014), so compute
pipelines and their bind groups are solved problems here. First *used* in
M7.

### A scene is meshes, instances and materials — and it is pure data

`wxsl_core::scene::Scene` is the document: meshes by primitive or by file,
materials as graphs, instances with a transform and `Tags`. It has no `wgpu`
in it and never will, which is why it is in `wxsl-core`.

The renderer keeps taking a **draw list**, which is not a new decision but
ADR 0005's: batching, culling and sorting belong to the application.
Translating a scene into one needs the node registry *and* the device, so it
is the `wxsl` facade's job (`wxsl::scene::SceneResources`) and neither
crate's alone.

A geometry pass draws a **tag expression** (`opaque`, `opaque && !outlined`,
`*`) over that list. The material says what it *is*; the pass says what it
*draws*; neither introspects the other. Tags are open strings rather than a
bitset, because the set is open — an application invents `underwater`
without asking anyone — and a document that round-trips through a file
cannot carry a bit whose meaning lived in someone else's enum.

`wxsl_render::scene::Scene` is renamed `wxsl_render::environment::Environment`,
which is what it always was.

### Instance transforms move to a storage buffer

One `array<Instance>` in the frame group, indexed by
`@builtin(instance_index)`: one binding, one upload for the whole frame, and
the shape a culling pass can write indices into later. A draw's position in
the draw list *is* its row.

### Device features are negotiated

`gpu.rs` asks for the optional features the coming milestones want
(`DEPTH32FLOAT_STENCIL8`, `INDIRECT_FIRST_INSTANCE`, `TIMESTAMP_QUERY`,
`FLOAT32_FILTERABLE`), intersected with what the adapter has, and records
what was granted in `DeviceCaps`. Before this, `request_device` took
`..Default::default()` and no code could ask whether a feature existed.

### Geometry can come from a file

`u32` indices throughout, and glTF/GLB import behind a `gltf` feature on
`wxsl-render`. Geometry only: a glTF material is a PBR parameter set and
this project's materials are graphs, so importing one as the other would be
either a lie or a translator nobody asked for.

## Alternatives considered

* **Keep the `Pipeline` trait, add a struct per pass.** What we had. It
  scales linearly in hand-written code and can never express a pass list an
  application builds at runtime — which is the whole point of the pipeline
  editor this is heading towards.
* **Put the pipeline in the scene document.** Tempting, because then one
  file is the whole picture. Rejected: a scene must be re-renderable by a
  different pipeline, and swapping the pipeline must not rewrite the scene.
  It also drags target formats and pass order into a file that is otherwise
  about where a cube is.
* **A tag *bitset* rather than strings.** Faster and smaller, and it makes
  the set closed. Rejected for now on the grounds above; the sets are a
  handful of entries and a linear scan wins at this size. If profiling ever
  says otherwise, interning them is a change inside `Tags`.
* **Transient aliasing by memory pooling rather than whole textures.** The
  general answer, and what a mature engine does. Rejected as premature: a
  slot is reused only when the descriptors match exactly, which is
  conservative, correct, and enough for a pass list of single digits.
* **Keep the per-object dynamic offset** (ADR 0010's choice). Rejected: it
  costs a bind-group rebind per draw and cannot be written by a compute
  pass. The capability it gives up — read-only storage in the vertex stage —
  only matters on WebGL, which is not a target.
* **Deriving the pass list from a *node graph* now.** That is M4 of the
  plan, and it needs the texture value types M3 introduces. Passes are data
  first; a graph that produces that data is then a small step.

## Consequences

* **`RenderRequest` changed shape**: `scene`/`model`/`mesh`/`material`
  become `environment` and `draws`. `single_draw` covers the one-object
  case the editor's preview and the demo use.
* **The ABI's frame group changed**: `abi::BINDING_OBJECT` becomes
  `BINDING_INSTANCES`, `bindings.wxsl`'s `object: Object` uniform becomes
  `instances: array<Instance>`, and `VertexIn` gains
  `@builtin(instance_index)`. The host mirror is
  `environment::InstanceTransform`; the two are still edited together
  (ADR 0008).
* **This amends ADR 0010**, whose "storage buffer indexed by
  `instance_index`… stays available as an opt-in later" is now the default.
  The slot allocation is unchanged, exactly as that ADR predicted.
* **This amends ADR 0005** in one place only: `RenderPath` is still the
  compile axis, but it is now a *field of a pass* rather than a property of
  the renderer. **Superseded as predicted by
  [ADR 0022](0022-material-stages-replace-the-render-path-enum.md)**:
  `PassKind::Geometry { path }` is now `{ stage }` and nothing else moved.
* A pass list is validated up front, so several classes of `wgpu` error
  become a named error naming the pass: a colour-attachment count its
  shader cannot write, a depth attachment disagreeing with the pipeline
  state, a resource nothing writes, a pass sampling what it is drawing
  into, a history read deeper than the ring.
* Bind groups for the pass group are built generically from a pass's
  `reads`, in order. The deferred lighting pass's hand-written G-buffer bind
  group is gone; the same code now serves any pass with inputs, which is
  what M7's screen effects need.
* `PipelineCache` keys on (variant, `PassState`, target formats), so one
  material can be drawn opaque in one pass and blended or front-face-culled
  in another with no second graph.
* **What is not done here**: buffers are not graph resources (only textures
  are), so an indirect buffer is handed to a pass directly; `multi_draw` is
  not used; and the transient allocator aliases whole textures rather than
  memory. Each is an extension, not a rewrite.
* If the pass or resource vocabulary changes, `docs/architecture.md`'s
  "How a frame is drawn" section and `AGENTS.md`'s test-command list are the
  two places outside the code that describe it.
