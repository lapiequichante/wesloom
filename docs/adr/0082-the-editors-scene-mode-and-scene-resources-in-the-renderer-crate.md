# 82. The editor's scene mode, with `SceneResources` in the renderer crate

Date: 2026-10-14

Status: Accepted

## Context

Plan6 E1. The editor shows one material on a primitive mesh. It should open
a whole scene, either a `.scene.json` or an imported `.glb` (ADRs 0079,
0080), and render it through the pipeline it is running. The user should
be able to pick an instance, edit it, and edit its material live, and an
agent (M1) should be able to do all of this through commands (ADR 0081).

`SceneResources`, which turns a scene document into a draw list, lived in
the `wxsl` facade, because ADR 0021 said that only the facade has both the
registry and the device. But `wxsl-editor` is a dependency of the facade,
so the editor could not reach it. Meanwhile `wxsl-render` already takes a
registry (`Material::from_graph`) and already reads glTF geometry. The
facade had been the place for `SceneResources` out of habit, not need.

## Decision

- **`SceneResources`, `SceneImage`, `LoadError` and `environment` move to
  `wxsl_render::scene`.** `wxsl::scene` re-exports them, so the stable
  path does not change. The renderer still takes a draw list and never
  holds a scene: the module is a loader that sits beside it. `wxsl-render`
  gains an `images` feature for decoding, which the facade's `images`
  feature turns on. Three things are added:
  - `scene::mesh_data`, which resolves one mesh source on the CPU;
  - `SceneResources::load_resolved`, which takes the meshes already
    resolved;
  - `SceneResources::set_material`, which recompiles and rebinds one
    material.
- **`CanvasMode::Scene`, a third mode of the same editor** (ADR 0043):
  - The viewport is the main panel. The preview's own renderer draws the
    scene into a target the size of that panel, so the scene uses the
    same pass list and the same variant cache as the material preview.
    Dragging orbits the camera and scrolling zooms it.
  - The outliner takes the palette's place. It lists the node hierarchy
    and the instances, and can be filtered by name or by tag.
  - The inspector shows the selected instance: its name, its transform
    as translation, rotation and scale, and its material. It also shows
    that material's tags, `render_order` and `max_layers`, and the
    environment's lights.
- **Scene commands** join `Command`:
  - `open_scene`, `import_gltf` (into the open scene) and `close_scene`;
  - `set_transform`, `set_instance_material` and `set_instance_name`;
  - `set_material_tags`, `set_render_order` and `set_max_layers`;
  - `set_light` and `remove_instances`.

  They snapshot the scene document. Imported geometry and images are
  shared behind `Arc`, so a snapshot copies only the document.
  - `select_instance` and `save_scene` change nothing that undo would
    restore, so the history does not record them.
  - `Query::Scene` returns the document and the selection.
- **The selected instance's material opens in the material canvas.** The
  material canvas then edits the scene's copy of that graph. Each edit
  recompiles that one material in the scene, and the material preview
  keeps showing it on its mesh. An undo step records which material was
  open when it was taken. The graph the editor started with is kept, and
  returns when the scene closes.
- **Picking runs on the CPU and needs no device.** A ray from the viewport
  pixel is first tested against each instance's world bounds. Then the
  triangles of the candidates are tested, nearest bounds first. The
  meshes are resolved once on the CPU and also handed to
  `load_resolved`, so each file is read once.

## Alternatives considered

- **A new `wxsl-scene` crate** between the renderer and the facade. That
  adds a crate whose only content would be a loader, while the renderer
  already depends on everything that loader needs.
- **The host passes a loaded scene into the editor through a trait.**
  Then the editor could not open a file or import a `.glb` on its own,
  and an agent working through commands would need a second path.
- **A second renderer for the scene.** That means two variant caches and
  two copies of every pass list, and it would contradict ADR 0043's
  single renderer.

## Consequences

- ADR 0021's "the facade's job" now reads "`wxsl_render::scene`,
  re-exported by the facade". `docs/architecture.md`, `docs/rust.md` and
  `AGENTS.md` follow.
- With the `gltf` feature, `wxsl-editor` depends on `wxsl-gltf` for
  `import_gltf`. Without the feature, the command returns an error.
- `save_scene` refuses a scene that holds embedded meshes or images,
  meaning an import, and names the reason. Writing those out as files is
  left for later.
- Not done:
  - transform gizmos;
  - lights and cameras placed in the viewport;
  - more than one scene.
