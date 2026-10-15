# 86. Large scenes: instanced runs, per-view culling by draw bounds, shared materials

Date: 2026-10-15

Status: Accepted

## Context

Plan6 G3: the scenes the editor now loads (ADR 0082) are glTF files with
thousands of instances. The renderer issued one `draw_indexed` per draw
list item per pass, and planned each item separately: a variant lookup,
the lighting and attribute checks, a pipeline and its bind groups. Nothing
was culled, and two scene materials with the same graph and configuration
were generated twice.

The plan put batching and culling in `SceneResources`, keeping the
renderer draw-list-in (ADRs 0005, 0082). Batching fits that. Culling does
not, entirely: one draw list feeds every pass of the frame, and the shadow
passes render from views the renderer derives from the environment
(cascades, spot tiles). A list culled to the camera drops the casters that
throw shadows into view.

## Decision

- **The scene forms runs, the renderer issues them.** Adjacent selected
  draws whose instance rows are consecutive and that share the mesh, the
  material, the material bindings and the user group (by identity) become
  one instanced draw. They are planned once: one variant, one check, one
  pipeline. The output is unchanged, since an instanced draw rasterizes
  its instances in order. Indirect draws are never merged.
  `SceneResources::draw_list` orders its draws by (material, mesh), so
  identical pairs are adjacent.
- **A draw may carry world bounds** (`DrawItem::with_bounds`), an
  axis-aligned box. A geometry pass skips a draw whose box lies outside
  its view's side planes or far plane (the camera's, or the shadow tile's),
  tested in clip space by `wxsl_frame::environment::outside_view`. The
  near plane is not tested, so a caster behind a shadow view's near plane
  still renders. A draw without bounds is always drawn, as before.
  `SceneResources` computes every instance's bounds from its mesh.
- **Shared materials.** `SceneResources` generates a material once per
  distinct (graph, configuration) and clones it for the other entries.
  Each entry keeps its own bindings, so editing one entry still affects
  that entry only.
- **Counts.** `pass_draw_count` still counts objects drawn;
  `Renderer::pass_call_count` counts draw calls. The frame panel and
  `Query::Frame` report both.

## Alternatives considered

- **Cull to the camera in `SceneResources`.** Wrong shadows, unless the
  application also re-derived the renderer's shadow views.
- **A `DrawItem` with a slice of transforms.** It changes what an item is
  for every caller, and the per-instance attributes and previous
  transforms would need slices too. Merging adjacent items gives the same
  calls without that.

## Consequences

- The plan's guard rail "culling lives in the scene" is amended: the
  scene supplies the bounds, the renderer tests them per view. The
  renderer still holds no scene.
- A draw list's order is now a performance concern: interleaving
  materials splits runs. Sorted passes (transparency) split them too,
  which is correct.
- The `instance id` debug view colours the draw row, which in a scene is
  no longer the document's instance index.
- An application drawing an animated object must update its bounds, or
  give none.
