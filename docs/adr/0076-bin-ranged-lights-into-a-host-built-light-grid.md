# 76. Bin ranged lights into a host-built light grid

Date: 2026-10-14

Status: Accepted

## Context

N7's last open item: sixteen lights is the length of an array in the scene
uniform, not a light list that scales. Every fragment loops over every
uniform light, so the array cannot simply grow: the cost would be per pixel
per light, whether or not the light reaches the pixel. Shadows, cookies and
area lights already work. What is missing is "many cheap lights".

The constraints come from earlier decisions. Both backends consume one
shared frame description (ADRs 0048–0051). WebGL2 has no compute and no
storage buffers (ADR 0075). Anything a test can check without a device
should be checked without one. And forward and deferred must shade the same
lights the same way.

## Decision

A **local light** is a point or spot light with a finite `Light::range`
that casts no shadow. Local lights leave the scene uniform for a clustered
list; every other light keeps the sixteen uniform slots and the existing
loop, unchanged.

* **Range is a window, not a cutoff.** A ranged light's inverse square is
  multiplied by `saturate(1 - (d/r)^4)^2`, so it reaches exactly zero at
  `r` and binning it is exact. Zero range — the default — is the unbounded
  falloff every light had before, so no existing scene changes.
* **The grid is 16 × 9 × 24 clusters of the camera's frustum**
  (`abi::LIGHT_GRID_DIMENSIONS`). Across and down are in NDC, independent
  of the viewport. Depth slices are exponential in view depth between the
  camera's near and far planes.
* **The host bins, in `wxsl-frame`** (`light_grid::LightGrid`). Each
  light's range sphere is bounded by its box, the box's eight corners are
  projected, and the light is listed in every cell of the resulting cell
  box. That bound is conservative: the hull of the projected corners
  contains the projected sphere, and clip `w` is affine. Lights wholly off
  screen, nearer than the near plane or past the far plane are culled. A
  cell lists at most `MAX_LIGHTS_PER_CELL` (256) lights, a frame at most
  `MAX_LOCAL_LIGHTS` (4096). Anything refused is counted in
  `LightGrid::dropped`, never silently.
* **Two frame-group bindings carry it.** Binding 13 is the light rows, in
  the uniform's own `Light` layout. Binding 14 is one `u32` word array in
  `vec4u` rows: a (first, count) pair per cell, then the index runs. The
  cell mapping (the binning matrix, the grid size with the light count,
  the depth slicing) is in the scene uniform, so the shader's
  `local_light_span` and `GridMapping::cell` are the same arithmetic over
  the same numbers. Under `wxsl_instance_texture` both arrays are
  `rgba32uint` textures read by loaders, exactly like the instance rows.
* **The light loop gains a second loop:** the cell's run, one sample per
  light, no shadow lookup. Both run in `shade_surface`, so forward,
  deferred and every lighting model see the same lights.
* `Environment::cluster_lights` (on by default) is the A/B switch. Off,
  every light competes for the uniform slots in list order, as before.

## Alternatives considered

**Compute-built clusters.** A compute pass reading a light buffer and
writing the cell lists is the GPU-scale answer, and the plan named it. It
loses here for now. It needs compute, so WebGL2 would need a second
implementation. Both backends would each record it. And it can only be
checked on a device. CPU binning is sub-millisecond at these bounds and
is one pure function both hosts share. The buffer format is the
interface: a compute builder can later write the same two bindings
without touching a shader.

**Growing the uniform array.** The cost stays per light per pixel, and the
uniform is near WebGL2's 16 KiB limit.

**Tiled (2D) light lists.** Cheaper to build, but depth discontinuities
put far lights in near tiles. Exponential depth slicing is what makes the
lists short in a deep scene.

**Clustering shadowed lights too.** A shadow is a run of atlas tiles
addressed through the uniform slot. Shadowed lights stay in the slots,
which keeps the shadow code untouched; the tile budget, not the light
count, is what bounds them anyway.

## Consequences

* A frame carries thousands of lights. A fragment's cost is the lights its
  cluster lists. The gallery's `clustered` demo draws 1024.
* The generated host layout changed: `Light.range` replaces a padding
  word, the scene uniform gains a 96-byte tail, and the C header and Dawn
  bundles were regenerated. Bundles carry `local_lights`/`light_grid` (and
  their texel forms under a fallback profile). `bake_docs --local-lights N`
  exports a scene with ranged lamps for the renderer smoke test.
* The grid answers for the camera's frustum. A pass that shades from
  another view would read an edge cell and could miss lights. Today every
  shading pass draws from the camera. A future reflection or probe view
  needs its own grid or the unculled list.
* Spot lights are bounded by their range sphere, not their cone. That is
  conservative, but a narrow spot occupies more cells than it lights.
* Tests: `light_grid`'s unit tests (every visible point inside a light's
  range finds that light in its own cell; culling; cell overflow), and
  `crates/wxsl/tests/clustered_lights.rs`. The latter checks that the grid
  and the uniform draw the same picture forward and deferred, full and
  WebGL2 profile, and that a thousand lamps light only their reach. Dawn
  is checked by `run_renderer_smoke.sh`'s `local-lights` bundles.
