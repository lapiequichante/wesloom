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

* Split files where the inventory shows two modules in one:
  * `pipeline_doc.rs` (5.1k lines): node registry vs. compiler;
  * `graph.rs` (4k): model vs. serialization;
  * `codegen.rs` (3.7k);
  * the editor's `app.rs` (3k), which M0 will split anyway.
* Remove dead code and orphan examples, and reconcile the `wxsl_render`
  re-exports of `wxsl_frame` so each type has one canonical path.
* **Open question (from the old `todo.md`):** why the blurred bloom
  shows bands.

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

### C4 — Stabilization and 0.1.0 *(last)*

* Freeze the stable tier as C2 defined it.
* Bump the version to `0.1.0`, start a `CHANGELOG.md`, and run
  `cargo publish --dry-run` on the published crates.
* Re-audit `NOTICE` and the licenses of every packaged crate (ADR 0053).
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
  batching and culling live in the scene and in `SceneResources`; the
  renderer still takes a draw list.
* **MCP has no logic of its own.** A tool that cannot be expressed as a
  command or a query means the vocabulary is missing a command; add the
  command.
* **Concise documentation is a property that has to be maintained.** A
  module's doc describes the module; the reasoning goes in the ADR; the
  plans go in `docs/history/` once they are done.
