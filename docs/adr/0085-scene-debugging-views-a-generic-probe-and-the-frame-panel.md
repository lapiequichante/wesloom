# 85. Scene debugging: debug views on both paths, frame views as editor passes, a generic probe

Date: 2026-10-15

Status: Accepted

## Context

Plan6 E2 extends pass stepping (plan3 N9) from a material to a whole
scene. What existed before:
- the G-buffer views, a macro (`wxsl_debug_view`) that only the deferred
  lighting pass honoured, so forward had no views;
- the scene's materials never received the macro, so the views did not
  work in the scene mode;
- pass stepping's inspect effects, which draw one resource of a frozen,
  cut pass list.

Nothing read a value back, and the frame tab showed timings only while
stepping. Agents (ADR 0083) were promised the views and a probe as
queries, with no tool of their own.

## Decision

- **Surface views follow the shading.** `lighting::DebugView` gains a
  `stage`:
  - `Both`: albedo, normal, depth, metallic-roughness, emissive,
    occlusion;
  - `Forward`: uv, mip level (the level a 1024² texture would be
    sampled at), instance id and material id, and overdraw's constant;
  - `Deferred`: the feature channels, as before.
  Under the macro, codegen gives a forward stage
  `lighting::debug_surface` in place of the shading function, with the
  same encoding as the lighting pass's arm. Default shaders are unchanged.
  The scene mode adds the macro to every scene material's config. A view
  is offered when the running pass list has a pass that shades it.
- **Ids without an ABI change.** The fragment stage has no instance
  index. Under the `instance id` view only, the vertex entry writes the
  draw's instance row into the uv varying, which that view reads instead
  of a uv. `material id` colours by `wxsl_debug_material`, an `i32` macro
  the scene mode sets per material while that view is on.
- **Frame views are views of the running pass list.** `views::FrameView`
  is `wireframe`, `overdraw`, `light count` (ADR 0076's heatmap),
  `cascades` and `shadow atlas`.
  - Wireframe is `Renderer::set_wireframe`: every camera geometry pass
    rasterized as lines and shaded as usual, so a prepass agrees with the
    pass after it. The pipeline key carries it. It needs
    `POLYGON_MODE_LINE`, now wanted, so the menu greys it out on the web.
  - Overdraw installs the pass list with its forward camera passes
    rewritten: no depth test or write, no culling, additive blending. The
    materials return a constant under the overdraw surface view, so the
    sum counts fragments. Deferred has no forward pass to count.
  - The other three are screen effects in the `editor.` package. They
    never enter the document vocabulary.
  - Showing one installs `RenderGraph::stepped(all passes, (source,
    effect))`, the cut pass stepping already uses, and keeps the frame
    running live.
  - The source is found by role: the depth of the camera's last geometry
    pass, or `shadow_maps()`. One mechanism works for forward, deferred
    and any document.
  - A view outlives a change of pipeline or features: it is kept, and
    shown again over the new pass list once a swap lands, if that list
    can show it.
- **One view command and one menu.** `SetDebugView` takes a label from
  either list. `Query::View` lists those the running pass list can show.
- **The probe is generic over the pass list.**
  - `Renderer::probe(at)` makes the next frame copy one texel of every 2D
    texture, array layer and cube face right after each pass that writes
    it. Aliased transients are therefore read before they are reused, and
    the last write wins. A buffer has no point, so its first 256 bytes
    are read as words. Volumes and mips past the base are not read.
  - Depth textures are copied whole, the only copy wgpu allows of them.
    Pool textures gain `COPY_SRC`.
  - `wxsl_render::probe` decodes the common formats without a device.
  - The editor adds the target's displayed pixel, and in a scene the
    instance and its material, found by the existing CPU ray pick.
  - `Command::ProbePixel` (Alt+click in the preview or the viewport) is a
    view command, not an undo step. `Query::Probe` answers.
- **The frame panel is a query.** `Query::Frame` gives, per pass, the GPU
  time, the draws and the triangles. Over the frame it gives pipelines,
  variants, cache hits and misses, the light grid's binned and dropped
  counts (`FrameBindings::light_grid_counts`), and the capability check's
  findings for the open scene. The frame tab shows it while the frame is
  live.
- **MCP gets them as tools over the same queries:**
  - `view_set` takes the new views;
  - `pixel_probe` applies the command, draws, then answers `Query::Probe`;
  - `frame_stats` draws, then answers `Query::Frame`.

## Alternatives considered

- **A frame view as a renderer feature (an overlay pass).** It would have
  survived pipeline swaps. Rejected: it puts an editor concept into the
  renderer, and the stepping cut already exists.
- **Probe readback after the frame.** It is simpler, but wrong for
  transients: aliasing reuses their memory within the frame.
- **Disabling aliasing for a probed frame.** It changes what is measured,
  and it reallocates the pool twice.
- **The instance index as a new varying.** It would change the
  vertex-output ABI, which Dawn shares, for a debug view; the uv varying
  is free while the view is on.
- **Wireframe as a `PassState` field.** That state is document vocabulary
  and crosses the C ABI. Barycentrics need non-indexed geometry.
- **Overdraw as a renderer mode.** The pass list is data, and the
  editor already installs rewritten pass lists for its views.

## Consequences

- `RenderGraph::record` takes an optional `TexelProbe`. `Renderer` has
  `probe`, `take_probed`, `pass_triangle_count`, `supports_wireframe`,
  `set_wireframe` and `wireframe`. Pool buffers gain `COPY_SRC`.
- A new surface view is a `BASE_DEBUG_VIEWS` row, plus its expressions in
  `debug_surface` and in the lighting pass's arm.
- A new frame view is a `FrameView` variant: an effect and a source, or
  a pass list it installs.
- The instance id view colours by instance row, not by scene instance;
  the probe names the scene instance.
- A probe stalls its frame once, waiting for the GPU. The views run at
  full speed.
