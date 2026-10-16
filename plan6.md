# Plan 6: real assets, a scene editor, an agent interface, a stable surface

Written on 2026-10-14, after plan3's N8 (ADR 0077) closed the release gate.
The owner's goals, in their words:

> Supporter le chargement des fichiers glb, supporter l'affichage et le
> debug de scènes entières dans l'editor, modifier l'editor pour y ajouter
> un serveur MCP qui autorisera les LLMs à faire des shaders / inspecter /
> debugger la scène. […] Cleaner un peu le projet : que la documentation
> soit plus claire et concise, documenter l'API et la stabiliser
> progressivement.

Four tracks: **C**lean up and stabilize, **G**LB import, **E**ditor scenes,
**M**CP. They are not independent: everything the editor gains should be
reachable by an agent, and every new API should land already sorted into
"stable" or "unstable". So the plan starts with two small foundations:
C1/C2 (what is public, and in what state) and M0 (editor commands). The
features come after.

## Where things stand

* **glTF is geometry only** (`wxsl_render::gltf`, behind the `gltf`
  feature). It reads primitives, bakes node transforms into world space,
  and maps `COLOR_0`/`TEXCOORD_1` to named streams. It ignores materials,
  textures, lights, cameras and the node hierarchy. Its module doc says
  why: a glTF material is a parameter set, and ours are graphs.
* **The scene document** (`wxsl_core::scene`) holds meshes (primitives or
  `MeshSource::File`), materials (graph + `MaterialConfig`) and flat
  instances. It cannot name textures or per-instance attribute values;
  `wxsl::scene::SceneResources` leaves both to the application.
* **The editor** has two canvas modes, `Material` and `Pipeline`. The
  preview renders one material on a primitive mesh. It already has pass
  stepping and resource inspection (`stepping.rs`), shader hot reload,
  GPU timing in the renderer, and headless screenshots. It has **no undo**,
  and its state changes only through UI events and a handful of `pub fn`s.
* **The documentation** is spread over:
  * 6 plans, `plan2-architecture.md`, `request.md` and `todo.md` at the root;
  * a 1085-line `docs/architecture.md`, half of which restates ADRs;
  * a French S2–S4 implementation guide;
  * 77 ADRs;
  * an `AGENTS.md` whose test-routing section has grown into a catalogue.
* **The API**: every crate is at `0.0.0`. There are about 1,400 `pub`
  items, with no record of which ones are meant to stay. `missing_docs` is
  `warn` in six crates. The C ABI and the document formats are already
  versioned (ADRs 0044, 0049); the Rust API is not.

## Decisions to take before starting

Each item below assumes the recommended answer. The owner should confirm
or overturn it before the first ADR is written.

1. **A glTF material becomes a generated graph, not a parameter set.** The
   importer emits one stock graph, `gltf_pbr`, built from stdlib nodes:
   texture samples, factors, alpha, emissive. The values come from the
   file. The graph is visible and editable in the editor, so it is not a
   lie but a starting point. This amends ADR 0021's "geometry only" stance.
2. **The importer produces documents, not GPU objects.** Its output is a
   `Scene` (plus mesh data and image bytes), not `Mesh`/`Material`. The
   editor, `SceneResources`, Dawn bundles and the MCP server all consume
   the same result, and it can be tested without a device.
3. **MCP attaches to a running editor.** It is served over local HTTP
   (`127.0.0.1`, opt-in flag, token), so the user watches the agent work
   in the window they already have open. A headless mode
   (`--mcp-stdio --headless`) serves the same tools over stdio for CI and
   for agents with no screen. The two transports share one implementation.
4. **Every MCP tool is an editor command.** The editor gains a
   serializable `Command`/`Query` vocabulary with undo. The UI, the tests
   and MCP all go through it. MCP is a transport over that vocabulary; it
   has no logic of its own.
5. **Skinning, morph targets and animation are out of scope** for this
   plan (G4 is optional). They need a new stage and a clock; static
   scenes are the goal.
6. **"Stable" means three surfaces, under three different rules:**
   * the document formats and the C ABI, which are already versioned;
   * a small stable Rust surface on the `wxsl` facade;
   * everything else, which stays explicitly `unstable`.
   The first release is `0.1.0`, not `1.0`.

## Proposals

### C1 — Documentation triage *(first, small)*

The goal is that a newcomer can find what they need in three documents.
This is moving and cutting, never rewriting.

* **History:** *done.* `plan.md`–`plan5.md`, `plan2-architecture.md` and
  `request.md` moved to `docs/history/`, with their links fixed. The S2–S4
  guide follows once its last items (S4 preset, AgX) close. `todo.md` is
  gone: its WebGL2 item was done (ADR 0075), its GLB demo is G2, and the
  bloom banding question is under C3.
* **`README.md`:** *done.* 193 → 80 lines: what it is, one picture, the
  commands, the crates, links.
* **`docs/architecture.md`:** *done.* 1085 → 460 lines.
  * One section per concept: a summary, a link to its ADR, a link to its
    module.
  * Removed: the demo walkthroughs, the status table, the test list.
  * New in it: transparency, forward shading, light kinds and the C ABI
    backend, which it barely mentioned.
* **`CONTRIBUTING.md` and the glossary:** *done.* They had drifted from
  ADRs 0053 and 0011 (no ports allowed, WXSL described as WESL).
* **`AGENTS.md`:** *done (2026-10-14).* Down from 575 to 210 lines. The
  test routing is a table (change → command), the per-test detail is left
  to the test files' headers, and each convention is one line plus its ADR.
* **`docs/adr/README.md`:** *done.* The index already had a status column.
  It gains a "read these first" list of ten ADRs.
* **Two guides, short and runnable:**
  * `docs/guide/` "write a node", "write an effect", "load a scene", with
    their snippets checked the way `check_shader_guide` checks the S2–S4
    guide;
  * an LLM-oriented reference generated from the node registry (M2).

### C2 — API inventory and tiers

**Landed (2026-10-14, ADR 0078):**

* **Snapshots.** `api/<crate>.txt` holds every public item
  (`cargo public-api -s`). `scripts/public-api.sh` checks them, `--bless`
  rewrites them, and a CI job runs the check. The nightly and the tool are
  pinned.
* **Tiers.**
  * Stable: what a crate exports at its root, plus the document formats and
    the C ABI.
  * Unstable: module paths, and all of `wxsl-frame`.
  * Internal: private.
  Each crate's root doc has a "Stability" section.
* **First internal cut.** `wxsl-lang`'s pass modules are now private: its
  public API went from 1836 to 1533 lines.
* **Rust docs.**
  * The 21 rustdoc warnings are fixed, and `cargo doc -D warnings` runs in
    CI.
  * `missing_docs` is on in every library crate, so the ffi structs are
    documented.
  * The facade's front page now shows a complete frame, as a doctest.
  * `docs/rust.md` is a tour of the main types.
* **C/C++ docs.** `docs/cpp.md` is now the single home of the C ABI
  contract, moved there from the `wxsl-ffi` README. It covers:
  * the three ways to integrate;
  * ownership and status codes;
  * every request and response;
  * the `wxsl::Renderer` and `wxsl::Compiler` classes.
  The generated header now carries `/** */` comments on every field and
  status code, and the C++ headers document every method, all readable by
  Doxygen.

* **Root curation.**
  * `wxsl-render`'s root lost the renderer's machinery: the resource pool,
    the pipeline and variant caches, `gbuffer_format`, and the shipped
    effects' ids. These are still reachable through their modules.
  * `wxsl-lang` lost `mangle`, `Origins` and `Tok`.
  * `wxsl-editor`'s modules are private. Its surface is `Editor`,
    `EditorConfig` and what their methods return, which went from 1360 to
    372 lines. `Editor::function_text` and `function_problems`
    replace the `Functions` buffer handle.
  * Crates with private modules enable `unnameable_types`.

**Still open:**

* **More internal leaks.** The remaining candidates are in unstable modules:
  `wxsl-core::abi` (mostly shader ABI constants, public by design),
  `wxsl-render::ui` and `wxsl-lang::ast`. Revisit them when a module is
  promoted to the root.
* **Doctests** for the stable items that have none.

### C3 — Targeted cleanup

This work is driven by C2, never by style.

**Landed (2026-10-15):** four splits, one commit each, with no behaviour
change. The code moved verbatim, and the API snapshots list the same items.
Public types stay defined in the parent module, since a type defined in a
private child prints under that child's path in other crates' snapshots.

* `wxsl-frame::pipeline_doc` → `mod.rs` (the error, `compile` and the
  compiler's core), `resources`, `passes`, `peel` and `tests`. The guess
  "registry vs. compiler" did not hold: the registry is ten lines.
* `wxsl-core::graph` → `mod.rs` (the model and editing), `generics`,
  `validate`, `wire` (the file format) and `tests`. Serialization was only
  the 120-line `wire` module.
* `wxsl-core::codegen` → `mod.rs` (the generators), `emit` (the expression
  emitter), `write` (the module writer) and `tests`.
* `wxsl-editor::app` → `mod.rs` (the `Editor`), `host`, `panels`,
  `inspector` and `code` (the bottom tabs).
* **The bloom bands are answered.** The one-pass `bloom` effect samples a
  13-tap disc at a four-texel stride, so a highlight stamps a ring of dots,
  and a low threshold turns the rings into bands. The gallery's documents
  use the separable Gaussian (`bloom_x`, `bloom_y`) and the pyramid instead.
  The shaders' headers say so, and the 2026-10-15 screenshots show smooth
  halos.
* **Orphans:** no dead-code allowances outside shared test helpers. The one
  example nothing links, `wxsl-frame`'s `effect_interfaces`, stays: it is
  the device-free effect API's demo beside `plan_frame`.

* **One name per type (2026-10-16, ADR 0087).** The `wgpu` adapters keep
  the stable names; `wxsl-frame`'s originals became `TargetDesc`,
  `FrameGraph` and `ScheduleError` (the scheduler's error, which shared
  `GraphError` with the graph model's). `wxsl_render::pipeline_doc`
  re-exports by name, so its `compile` is the only one.

**Kept as is (decided 2026-10-16):**

* **Public functions nothing in the workspace calls**, mostly accessors
  and symmetric setters:
  * `wxsl-core`: `Graph::set_macros`, `set_attributes`, `remove_attribute`,
    `clear_user_block`; `feature_for_macro`, `is_terminal`,
    `screen_context_fields`;
  * `wxsl-frame`: `with_buffer_usage`, `with_normal_bias`, `is_deferred`;
  * `wxsl-render`: `request_graph`, `set_generator`,
    `pass_parameter_layout`, `instance_capacity`, `user_block_size`, a few
    device-capability queries, and the UI's `polyline` and
    `circle_border`.
  They stay public.

### G1 — Scene document v2 *(prerequisite for G2 and E1)*

The scene must be able to say what a glTF file contains.

* **`textures`:** an image by path (relative to the document) or by
  embedded index, plus its sampler and color space (sRGB/linear). Mips are
  generated on load (ADR 0061).
* **Bindings on a material:** parameter values and textures by declared
  name (ADR 0023), closing the gap `SceneResources::create_bindings` leaves
  open today.
* **`nodes`:** an optional hierarchy (name, local transform, parent), so
  the editor's outliner shows the file's structure. Instances still carry
  a world transform, computed on load, and the renderer never sees the
  hierarchy (the plan5 guard rail: sorting never becomes a scene graph).
* **Environment:** lights and cameras stay in `Environment` (ADR 0021). A
  scene document may carry an optional `environment` block, which is what
  a `.glb` with `KHR_lights_punctual` needs.
* Every new field has a default, so v1 scenes still load and v1 is read
  as v2. The schema version becomes 2.

**Landed (2026-10-14, ADR 0079):** all of the above.
`SceneResources::create_bindings` now takes the queue, uploads the
textures (CPU mips, averaged in linear light) and applies the bindings;
`set_images` supplies embedded images; `wxsl::scene::environment` builds
an `Environment`. Encoded images need the new `images` feature, which
`gltf` implies. Each document type now has its own schema version.

### G2 — glTF/GLB import

`wxsl::asset::import_gltf(path) -> ImportedScene` (document + mesh data +
image bytes), device-free, behind the `gltf` feature.

* **Geometry:** one mesh per primitive, transforms preserved in `nodes`
  rather than baked. The existing loader is the starting point.
* **Core materials:** `pbrMetallicRoughness`, `normalTexture`,
  `occlusionTexture`, `emissive*`, `alphaMode` (OPAQUE / MASK → discard /
  BLEND → `transparent` tag), `doubleSided`, and `KHR_texture_transform`.
* **Extensions mapped to what already exists:**
  * `KHR_materials_clearcoat`, `sheen` and `iridescence` → the matching
    models and channels (ADRs 0028, 0058, 0059);
  * `transmission`/`volume` → the refraction tier (ADR 0067);
  * `emissive_strength`;
  * `KHR_lights_punctual` → `Light` with `range` (ADR 0076).
  Any other extension is **reported by name** in `ImportedScene::warnings`,
  never silently ignored.
* **Images:** PNG and JPEG (add the `jpeg` feature to `image`). KTX2/Basis
  are reported as unsupported.
* **Tests:**
  * GLBs generated in-test (no committed binaries) for each mapping;
  * a handful of Khronos glTF-Sample-Assets downloaded into `refs/`
    (ignored) for a manual check, with `DamagedHelmet` as the reference
    image;
  * the release gate (ADR 0077) extended to the `gltf_pbr` graph under
    every config.
* **Demo:** `examples/viewer.rs`, which opens a `.glb`, orbits it, and
  runs on native, WebGPU and WebGL2. This closes `todo.md`'s "good PBR
  demo" item.

**Landed (2026-10-14, ADR 0080), native part:**

- The import lives in a new device-free crate, `wxsl-gltf`, which takes
  the node registry as an argument. It is reused by:
  - `wxsl::asset` with `SceneResources::load_imported`;
  - `wxsl-render`'s `file` meshes;
  - the C ABI (`wxsl_import_gltf`, `wxsl_import_glb`, with the meshes in
    the `WxslVertex` layout and the images encoded), checked by the C and
    C++ smoke tests.
- The mappings above are done. Extension textures are reported rather
  than mapped.
- The Khronos samples (DamagedHelmet, CarConcept, GlamVelvetSofa, with
  their licenses) are in `resources/glb/`, not `refs/`. A test compiles
  every material.
- `viewer` shows them under the lakeside HDR, with the car's glass
  through the refraction tier.
- Not yet done:
  - the viewer on WebGPU and WebGL2;
  - the `DamagedHelmet` reference image;
  - the release gate over `gltf_pbr`.

The `gallery` example was split into modules under `examples/gallery/`.

### G3 — Large scenes

Large scenes are what the editor will show. The renderer stays
draw-list-in, pixels-out; this work is in `SceneResources`, on the
application side.

* Batch identical (mesh, material) pairs into instanced draws.
* Frustum culling by bounding box.
* Share materials whose graph and config are identical, so variants are
  compiled once.
* **Budget:** Sponza or Bistro at 60 fps on a mid-range GPU. Measured, not
  assumed (`timing.rs`).

**Landed (2026-10-15, ADR 0086).**
- **Instanced runs.** The renderer plans and draws runs: adjacent selected
  draws with consecutive rows and the same mesh, material, bindings and
  user group are one `draw_indexed`, checked and looked up once.
  `SceneResources::draw_list` orders by (material, mesh), and
  `draw_instance` maps a draw back to its document instance.
- **Culling per view.** `DrawItem::with_bounds` gives a world box. Each
  geometry pass skips a box outside its view's side or far planes
  (`wxsl_frame::environment::outside_view`), the camera's or its shadow
  tile's, so a caster out of view still casts. `SceneResources` boxes
  every instance from its mesh, except for a material with a vertex stage,
  which may displace.
- **Shared materials:** an entry identical to an earlier one (graph and
  config) clones its generated material. GPU variants were already shared
  by content.
- **Counts:** `Renderer::pass_call_count`, and `calls` in
  `Query::Frame` and the frame panel, beside the draws.
- **Measured** with `viewer --grid N --bench FRAMES` (release, 1280×720,
  Apple M5, forward + IBL; this adapter has no timestamps, so the wall time
  includes waiting for the GPU):

  | scene | before | after, framed | after, inside the grid |
  |---|---|---|---|
  | 900 helmets (1800 draws) | 1800 calls, 21 ms | 2 calls, 16 ms | 1264 draws in 56 calls, 14 ms |
  | 100 cars (10,900 parts, 24,800 draws) | 24,800 calls, 110 ms (88 ms CPU) | 248 calls, 23 ms (8 ms CPU) | 16,630 draws in 2232 calls, 28 ms (15 ms CPU) |
- **Tests:** device-free, the clip-space test and box transform
  (`wxsl-frame`); on a device, a grid drawn in two calls with the hidden
  cube culled, matching the unbatched picture (`scene`), and a caster
  culled from the camera's passes that still shadows the ground
  (`shadows`).
- Not yet done:
  - Sponza and Bistro are not in `resources/glb/` (Sponza is CC BY 4.0,
    Bistro CC BY 4.0 and about 1 GB), so the 60 fps budget is not
    measured on them yet;
  - the per-draw work left is planning and the instance upload, both
    O(draws) per frame; the car grid from inside is CPU-bound at 15 ms.

### G4 — Skinning and animation *(optional, after the rest)*

A `skinned` vertex stage (joints and weights as streams, a matrix buffer
in the frame group) and animation sampling on the host. It needs its own
ADR and its own plan if the demand is real.

### M0 — The command layer *(before E1)*

* **`wxsl_editor::command`:** one serializable `Command` enum covering
  every mutation the editor makes, in the shape of the document APIs:
  * graph: add/remove a node, connect/disconnect, set a value, set a macro,
    rename;
  * pipeline: the same, plus `set_pass_param`;
  * functions: save a `user.*` function;
  * later, the scene commands (E1).
  `Editor::apply(Command) -> Result<Outcome, CommandError>` is the only
  path that mutates. The UI translates its gestures into commands.
* **Undo/redo** falls out of it: each command returns its inverse. Ctrl+Z
  is the immediate payoff, valuable even without MCP.
* **`Query`:** the read-only counterpart, covering graph, diagnostics,
  generated WXSL/WGSL, registry, stats and the pass list.
* **Tests:** command → state → inverse → the initial state, all
  device-free. A transcript format (JSON lines) gives replayable editing
  sessions for regression tests.
* **ADR:** commands as the editor's single mutation path.

**Landed (2026-10-14, ADR 0081):**

- `Command` covers every edit the canvases, inspectors, palette and
  toolbar made, on either document, plus `batch` (atomic), `undo` and
  `redo`. `Editor::apply` is the only path, and `graph_mut`,
  `pipeline_graph_mut` and `set_pass_param` are gone.
- Undo restores a snapshot rather than running an inverse, because
  generic re-resolution makes inverses inexact. A drag, a slider or a
  rename merges into one step. `Ctrl`/`Cmd`+`Z` and `Shift`+`Z` or `Y` are
  bound.
- `Query` and `Answer` cover the graph, diagnostics, source, nodes,
  passes, stats and history. Transcripts are JSON lines.
- Tests: the round trip of every command, an atomic batch, merging and
  transcript replay, all without a device; a UI drag undone with `Cmd`+`Z`
  in `editor_frame`.
- Not yet done:
  - saving a function is not a command, because it needs the device;
  - a batch cannot refer to a node it adds;
  - the selection is not a command.

  These come back with E1 and M1.

### E1 — The scene mode in the editor

* **A third `CanvasMode::Scene`.** The viewport becomes the main panel and
  renders the scene through the active pipeline: the same renderer, and
  the pipeline canvas still applies (ADR 0043).
* **Panels:**
  * **outliner:** the G1 hierarchy, filterable by tag;
  * **inspector:** the selected instance's transform, material, tags,
    `render_order` and `max_layers`, plus the environment's lights and
    camera.
* **Selection:** CPU picking (ray vs. bounds, then triangles), device-free
  and testable. Transform gizmos are drawn with the UI layer.
* **Selection and the material canvas:** selecting an instance opens its
  material in the material canvas, and edits recompile live across the
  whole scene (one variant cache).
* **Files:** opening and saving `.scene.json`; drag-and-drop of a `.glb`
  imports it (G2) into the open scene.
* Every action goes through M0 commands (`scene.select`,
  `scene.set_transform`, `scene.import`, …).

**Landed (2026-10-14, ADR 0082):**

- `SceneResources` moved to `wxsl_render::scene` (re-exported as
  `wxsl::scene`), with `mesh_data`, `load_resolved`, `set_material` and
  `set_instances`, so the editor uses the same loader as an application.
- `CanvasMode::Scene`: the preview's renderer draws the scene at the
  viewport's size, through the pipeline the toolbar picks. Drag orbits,
  the wheel zooms, `R` frames the selection.
  - The outliner shows the node hierarchy, or the instances matching a
    name or tag filter.
  - The inspector shows the instance's name, transform and material, the
    material's tags, `render_order` and `max_layers`, and the lights.
- Picking: a ray against world bounds, then triangles, with no device.
  A click selects an instance and opens its material in the material
  canvas. Edits there are written back and recompile that one material
  in the scene.
- Commands:
  - `open_scene`, `import_gltf` (which appends to an open scene),
    `close_scene` and `save_scene`;
  - `select_instance`;
  - `set_transform`, `set_instance_material`, `set_instance_name`;
  - `set_material_tags`, `set_render_order`, `set_max_layers`;
  - `set_light` and `remove_instances`;
  - `Query::Scene`.

  The scene is a third snapshot document, its imported geometry shared
  behind `Arc`.
- The renderer takes the scene's own lighting set, which keeps sheen and
  similar layered scenes within the G-buffer budget, and gets its own set
  back when the scene closes.
- The `editor` example has `--scene <FILE>`, accepts dropped files, and
  saves the scene with `Ctrl+S` in the scene mode.
- Tests:
  - picking, merging, transform decomposition and every scene command's
    round trip, without a device;
  - in `editor_frame`, the sheen scene drawn, picked by a click, its
    material edited and undone;
  - DamagedHelmet imported, with a second file appended and the append
    undone.
- Not yet done:
  - transform gizmos;
  - lights and cameras in the viewport;
  - the refraction tier for `refractive` materials (the viewer routes
    them, the editor draws them opaque);
  - saving an imported scene, which `save_scene` refuses by name;
  - per-instance attribute values.

### E2 — Scene debugging

Extends pass stepping (N9) from a material to a whole scene.

* **Views:**
  * G-buffer channels;
  * normals, UVs, wireframe;
  * overdraw;
  * **lights per cluster** (the ADR 0076 heatmap);
  * shadow atlas and cascades;
  * mip level;
  * material and instance ids.
  These are editor effects, like `editor.inspect`, and never part of the
  document vocabulary.
* **Pixel probe:** click a pixel to read back the value of every resource
  at that point, plus the instance and material drawn there. The readback
  is generic over the pass list, not specific to each view.
* **Frame panel:**
  * per-pass GPU timing;
  * draws, triangles and pipelines;
  * `cache_stats`;
  * the light grid's `dropped` count;
  * capability check warnings.
* Each view and the probe are a `Query` (M0), so MCP gets them for free.

**Landed (2026-10-15, ADR 0085).**
- **Surface views follow the shading.** `DebugView` gains a stage:
  - the base views (albedo, normal, depth, metallic-roughness, emissive,
    occlusion) show in forward too, where codegen swaps the shading
    function for `lighting::debug_surface`;
  - `uv` and `mip level` are forward-only;
  - the feature channels stay deferred-only.
  The scene mode compiles every scene material under the view. The menu
  offers a view only where a running pass shades it.
- **Forward id views:** `instance id` (the draw's instance row, carried
  in the uv varying under that view) and `material id` (a colour per
  scene material, from `wxsl_debug_material`).
- **Frame views:** `wireframe`, `overdraw`, `light count` (the ADR 0076
  heatmap), `cascades` and `shadow atlas`.
  - The last three are `editor.` effects appended live to the running
    pass list with the stepping cut, reading the camera's depth or the
    atlas.
  - Wireframe is `Renderer::set_wireframe`: the camera's geometry passes
    as lines, shaded, on a device with `POLYGON_MODE_LINE`.
  - Overdraw rewrites the forward passes (no depth test or culling,
    additive), and the materials return a constant.
  - They work on forward, deferred and documents (overdraw in forward
    only), and survive a pipeline change, through the swap.
- **The pixel probe:**
  - `Renderer::probe` copies one texel of every 2D texture, array layer
    and cube face right after the pass that writes it, so aliased
    transients read true. Depth is copied whole. A buffer gives its first
    64 words. `wxsl_render::probe` decodes the formats without a device.
  - The editor adds the displayed pixel, and the instance and material
    from the CPU pick.
  - `Command::ProbePixel` (Alt+click on the preview or the viewport) and
    `Query::Probe`.
- **The frame panel:** `Query::Frame`, shown in the frame tab while live.
  - Per pass: GPU time, draws and triangles.
  - Pipelines, variants, cache hits and misses.
  - The light grid's binned and dropped counts.
  - The capability check's findings for the open scene.
- **MCP:**
  - `view_set` takes every view;
  - a new `pixel_probe` tool;
  - `frame_stats` draws a frame, then answers `Query::Frame`.
- **Tests:**
  - device-free: forward views validate with naga (`graph_to_wgsl`), the
    decoding (`wxsl-render`), the views' sources on both presets
    (`wxsl-editor`);
  - on a device: every view on both pipelines and a view across a
    pipeline change, a probe of the G-buffer and depth, a scene with a
    shadowed sun whose cascades, atlas and probed instance are checked
    (`editor_frame`), a probe of every face of a baked cube (`ibl`) and of
    a compute-written buffer (`buffer_resources`), and the MCP session
    (`mcp`).
- By design: the probe does not read the imported target, whose value is
  the displayed pixel the editor adds; nor volumes or mips past the base.

### M1 — The MCP server

* **A new crate, `wxsl-mcp`** (depends on `wxsl-editor`, behind a
  facade feature `mcp`, never on by default). The server thread receives
  JSON-RPC and pushes `Command`/`Query` messages onto a channel, which
  `Editor::frame` drains on the render thread. Responses come back through
  a oneshot channel.
* **Tools:**
  * **shaders:** `nodes.search`, `graph.get`, `graph.edit` (an atomic
    batch of commands), `graph.compile` (WXSL, WGSL, diagnostics),
    `function.save` (WXSL source → a `user.*` node), `macros.set`;
  * **pipeline:** `pipeline.get`, `pipeline.edit`, `pass.param`;
  * **scene:** `scene.list`, `scene.select`, `scene.set_transform`,
    `scene.assign_material`, `scene.import_gltf`, `environment.edit`;
  * **inspection:** `view.screenshot` (a PNG as MCP image content),
    `view.debug`, `view.step`, `pixel.probe`, `frame.stats`,
    `problems.list`.
* **Resources:** the node reference (M2), the ADR index, the shader ABI.
* **Safety:**
  * localhost and token only;
  * filesystem access limited to the project root;
  * every agent action can be undone (M0) and is marked in the history;
  * a visible indicator shows that an agent is connected.
* **Tests:** an in-process MCP client drives a headless editor. The
  device-free tools run in CI; the screenshot and probe tools skip
  without an adapter, and their skips are not counted as passes.
* **ADR:** transport, threading, the tool ↔ command mapping, and the
  security model.

**Landed (2026-10-15, ADR 0083):**

- `wxsl-mcp`, behind the facade's `mcp` feature. The transport is
  hand-written on `std`, with no tokio and no HTTP crate:
  - local HTTP: `POST /mcp`, JSON replies, a bearer token, and local
    origins only;
  - stdio: one message per line.

  Both share one dispatcher. A call is planned on the transport's thread
  and run on the render thread by `Server::serve`, once per frame, or
  after `Server::wait` with no window.
- 24 tools. Their names use underscores, because clients refuse dots:
  - shaders: `nodes_search`, `graph_get`, `graph_edit`, `graph_compile`,
    `function_save`, `macros_set`;
  - pipeline: `pipeline_get`, `pipeline_edit`, `pass_param`;
  - scene: `scene_list`, `scene_open`, `scene_import_gltf`, `scene_save`,
    `scene_select`, `scene_set_transform`, `scene_assign_material`,
    `environment_edit`;
  - inspection: `view_get`, `view_set`, `view_step`, `view_screenshot` (a
    PNG), `frame_stats`, `problems_list`;
  - `history`, which lists, undoes or redoes steps.

  The resources are a node reference (`wxsl://nodes`), the shader ABI and
  the ADR index.
- `graph_edit` applies raw commands as one batch. An `add_node` can carry
  a `"ref"`, and later commands write `"$name"` for its id, so a batch can
  name a node it adds (M0's gap). This needs `Graph::next_id`.
- In the editor:
  - `apply_as_agent` marks an agent's step, and `Query::History` lists
    the steps;
  - `set_agent` puts the agent's name in the status bar;
  - `sync` is the compile half of `frame`;
  - `define_function` saves a function given as source (M0's other gap);
  - the session commands `set_canvas_mode`, `set_pipeline`,
    `set_debug_view` and `step_frame`, with `Query::View`, are not undo
    steps.
- Every path, including those inside raw commands, resolves under a
  `Root` or is refused.
- The `editor` example has `--mcp <PORT>` (it prints the `claude mcp add`
  line), `--mcp-stdio --headless` and `--mcp-root`.
- Tests:
  - without a device: the protocol, every tool's plan, refs, HTTP parsing,
    the token and origin checks, and confinement;
  - in `--test mcp`: an agent's session over HTTP (edit with refs,
    compile, a refused batch, undo, a function saved, a debug view, a
    screenshot, a step, refused paths, resources), and the line transport
    until its input ends.
- Not yet done:
  - ~~`pixel.probe` and the E2 debug views~~: landed with E2;
  - ~~a generated node reference~~: landed with M2;
  - server-sent notifications, such as a document changing under the
    agent.

### M2 — Making WXSL writable by an LLM

* **A reference generated from the registry**
  (`cargo run --example node_reference`): one entry per node, with its
  signature, defaults, domains and doc. It is published as an MCP
  resource and as `docs/reference/nodes.md`, and a test checks it is up to
  date.
* **A short authoring guide:** the shader ABI, one-`fn`-per-file, the
  common errors and how to read their diagnostics. Its snippets are
  checked like the existing guide's.
* **An end-to-end scenario recorded as a transcript (M0):** "make a
  material for rusty metal, look at it, fix it". This is the acceptance
  test of the whole MCP track.

**Landed (2026-10-15, ADR 0084):**

- The reference, `wxsl_mcp::node_reference`, is generated from the shader
  and pipeline registries: every node's sockets, types, defaults,
  generics, settings, macros and domains (210 nodes).
  - `docs/reference/nodes.md` is written by
    `cargo run -p wxsl-mcp --example node_reference`, and a test fails
    when it is stale;
  - `wxsl://nodes` is generated from the running editor's registries, so
    it includes the session's `user.*` functions.
- The guide is `crates/wxsl-mcp/authoring.md`, served as `wxsl://guide`
  and named first in the server's instructions. It covers:
  - a whole edit, and how values and types are spelled;
  - the shape of a function, imports, generics and macros;
  - the common errors, quoted.

  Every snippet is checked twice: device-free in `wxsl-mcp`, and run in
  order against the example's graph in `--test mcp`.
- The acceptance session was recorded by driving a headless editor over
  HTTP: `crates/wxsl/tests/transcripts/rusty_metal.jsonl`, 14 calls with
  notes. Over its three screenshots it fixed a flat albedo with a
  `user.rust_albedo` function, then a pale rim with a sharper metal mask.
  The replay checks:
  - every answer;
  - that each fix changed the picture;
  - that the last picture is rust over steel;
  - that four agent steps are in the history.
- What recording the session found, and what was fixed:
  - **An invalid shader panicked the editor.** Any agent or user function,
    or any hot-reloaded file, could crash it. Shader modules are now
    created in a validation error scope
    (`wxsl_render::variants::shader_module`), and a refusal is a
    `RenderError::ShaderCompile` in the problems list.
  - **`function_save` accepted functions that cannot compile.** It now
    validates the module's WGSL on the device.
  - **JSON refused `vec3f`.** The reference, WXSL and the diagnostics all
    spell it that way, so the WGSL spellings are now serde aliases of
    `ValueType` and `Value`.
- Not yet done:
  - a templated user function is validated only once a node instantiates
    it;
  - the validator quotes the WGSL, not the WXSL line. They match for a
    function, but not inside a material.

### C4 — Stabilization and 0.1.0 *(last)*

* Freeze the stable tier as C2 defined it. *Done (2026-10-16):* the
  snapshots were already a CI gate, and `AGENTS.md` now asks for a
  `CHANGELOG.md` entry with every stable change.
* Bump the version to `0.1.0`, start a `CHANGELOG.md`, and run
  `cargo publish --dry-run` on the published crates. *The bump and the
  changelog are done (2026-10-16), and the README names the 0.1 crates, the
  scenes, the MCP server and the asset licenses. Not run, by decision:
  `cargo publish`, including `--dry-run`, and the `v0.1.0` tag the
  changelog links to.*
* Re-audit `NOTICE` and the licenses of every packaged crate (ADR 0053).
  *Done (2026-10-16):*
  * the six ported shaders are the six `NOTICE` entries, each with its
    pinned revision (`tests/provenance.rs`). The shaders that re-express a
    published technique say so and copy no text;
  * every dependency is permissive (MIT, Apache-2.0, BSD, Zlib, ISC,
    Unicode-3.0, or a choice including one);
  * each crate now packages `LICENSE-MIT` and `LICENSE-APACHE` (symlinks
    to the root's), which none did; `wxsl-stdlib` also packages `NOTICE`;
  * the lakeside HDR had no provenance: it is Poly Haven's CC0 "Lakeside
    Sunrise", recorded in `resources/hdri/README.md`. The fonts and the
    glTF models already had theirs.
  * Left for the publish: the gallery reads the HDR from outside the
    `wxsl` crate. Natively it falls back to the procedural sky when the
    file is missing; only its wasm build embeds the file, so that build
    needs the repository, not the packaged crate.
* From then on the API snapshot (C2) is a gate: a stable change requires a
  CHANGELOG entry.

## Suggested order

1. **C1.** Short and immediately useful; it clears the ground.
2. **C2.** The inventory and the tiers, before the new surfaces land.
3. **G1, then G2.** The scene document, then the import. The viewer demo
   is G2's milestone.
4. **M0.** Commands and undo, before the editor grows.
5. **E1.** The scene mode, built on commands from the start.
6. **M1 + M2.** The MCP server over the existing commands. The first
   usable version can arrive right after E1, with shader and scene tools.
7. **E2.** The debug views, and their MCP tools for free.
8. **G3, then C3, then C4.** Large scenes, cleanup, 0.1.0.

G4 stays out of the main line.

## Risks

| Risk | Mitigation |
|---|---|
| The generated `gltf_pbr` graph diverges from the glTF spec's appearance. | A reference image on `DamagedHelmet` and in-test GLBs for each mapping. Every unmapped extension produces a named warning. |
| Scene v2 breaks existing scenes. | Every field has a default; v1 is read as v2 (ADR 0044's rule); `scene_check.scene.json` stays a test. |
| The command layer becomes a second editor API alongside the `pub fn`s. | The `pub fn` mutators become wrappers over `apply`, or disappear (C2). One path, tested by the round trip with the inverse. |
| MCP pulls an async runtime into the editor. | Isolated in `wxsl-mcp`, behind a feature. The editor only knows a channel. If the transport is hand-written (JSON-RPC over `std::net`, serde_json already present), no tokio at all; the ADR decides. |
| An agent damages the user's work. | Everything can be undone, nothing is written outside the project root, saving is an explicit command, and the agent session is visible. |
| Cleanup becomes a refactoring that never ends. | C3 is driven by C2's inventory, scoped file by file, after the features. Each split is a commit with no behavior change. |
| Tier stability blocks progress. | The stable tier is small (facade + documents). Everything else stays `unstable` until 0.1.0, explicitly. |

## Guard rails

The ones from the previous plans still hold: no GUI toolkit; one graph
model; a pure, device-free scheduler; new fields default to today's
behavior; every proposal comes with an ADR and a demo; two
implementations owe an agreement test.

Four more come from this plan:

* **The importer produces documents.** No glTF type and no GPU object
  leaves `wxsl::asset`; what comes out is a scene that can be saved and
  re-read.
* **The renderer does not become a scene engine.** The hierarchy,
  the draw order and the bounds live in the scene and in
  `SceneResources`; the renderer still takes a draw list. It merges
  adjacent identical draws and tests each box against its passes' views,
  which only it knows (amended by ADR 0086).
* **MCP has no logic of its own.** A tool that cannot be expressed as a
  command or a query means the vocabulary is missing a command; add the
  command.
* **Concise documentation is a property that has to be maintained.** A
  module's doc describes the module; the reasoning goes in the ADR; the
  plans go in `docs/history/` once they are done.
