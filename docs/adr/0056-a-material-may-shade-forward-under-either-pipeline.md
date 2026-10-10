# 0056. A material may shade forward under either pipeline, routed by the preshaded model

Date: 2026-10-10

Status: Accepted

Amends [0022](0022-material-stages-replace-the-render-path-enum.md)'s stage
contract (a `gbuffer` fragment returns a `GBuffer` — except a
forward-shaded material's, which returns shaded radiance in it) and
[0028](0028-lighting-models-dispatched-by-a-g-buffer-id.md)'s model
registry (one entry is a route, not a shading function).

## Context

Plan5's proposal: what derivatives and screen-space context blocked in
deferred should be marked *forward only* — shaded in the geometry pass
even under the deferred pipeline, "l'albedo est l'output", the lighting
pass passing the stored colour through. Fragment-only nodes (ADR 0054) are
legal in a G-buffer pass's fragment too; what they cannot do is ride an
interpolant into a *lighting pass* that reconstructs the surface from a
G-buffer they never wrote. So the shading that needs derivatives must run
where the derivatives exist — the geometry pass — and its answer must
travel to the frame unreshaded, with forward and deferred pictures
identical by construction.

## Decision

A material's configuration grows `forward_shaded`. Under a forward
pipeline it changes nothing. Under a deferred one, the material's own
lighting model runs in the G-buffer pass, and the radiance lands where the
G-buffer keeps **emissive**; the deferred lighting pass returns that texel
for pixels whose dispatch id names `wxsl.preshaded`, before the light loop
can run.

* **The route is a model row.** `wxsl.preshaded` in `DEFAULT_MODELS` (id
  5): its lighting function returns `surface.emissive`, and the generated
  pass answers a pixel of that id with
  `unpacked.surface.emissive` before anything else. The model registry,
  the id channel and the named-mismatch machinery are reused wholesale. A
  set of *only* that model makes the pass pure passthrough, and no
  shading function is generated at all.
* **The emissive target, not a new one.** Storing radiance in a fresh
  model-extra target would push the shipped full set past
  `MAX_GBUFFER_BYTES_PER_SAMPLE` (32, the spec's floor); storing it in the
  albedo target would clamp it — albedo is `rgba8unorm`. Emissive is HDR,
  already in the base layout, and a preshaded pixel's lighting pass reads
  nothing else. The pack of a forward-shaded material stores
  `shade_surface(...)`'s output there; the occlusion it displaces only
  ever attenuated ambient, which a preshaded pixel's lighting pass never
  runs.
* **The material's model stays its own.** `forward_shaded` is not a
  model; the material names `pbr` (or nothing) and that model shades it,
  in the G-buffer pass, with the material's macros — receive-shadows
  pinned off, so the shadow lookup compiles out.
* **Resolution refuses what it cannot honour**, by name: the set must
  enable `wxsl.preshaded` (otherwise the pixels carry an id no route
  answers), `receive_shadow` must be false (the radiance is computed
  before any shadow map exists), and the material may not name
  `wxsl.preshaded` as its own model (the route is not a shading
  function; there would be nothing left to shade with).
* **Parity is the acceptance test.** The same fragment output travels
  both paths, with one float16 round trip between them, so
  `--forward-shaded pbr_cube` measures `mean |forward - deferred| =
  0.0000` — exact, where the ordinary models' agreement is a tolerance
  over two different shading sites. The GPU suite asserts it.

## Alternatives considered

* **A stage variant instead of a model row** — a `gbuffer` stage flag
  saying "this pass shades". Lost to the route: the lighting pass still
  needs to know *which pixels* not to re-shade, which is per-fragment
  information, and the id channel already carries per-fragment answers.
  The model spelling also buys the capability check and the named errors
  for free.
* **A fresh HDR target requested by the route** — cleaner semantically
  ("the preshaded radiance has its own channel"), but the shipped full
  set crosses the attachment budget (30 + 8 > 32), and a budget raise
  trades a spec guarantee for a convenience.
* **Storing in albedo, as the owner first spelled it** — the honest
  reading of "l'albedo est l'output" — fails on arithmetic: albedo is
  normalized, and radiance is not. The emissive target is where the
  G-buffer already keeps *output-shaped* radiance, which is what a
  preshaded pixel's texel is.

## Consequences

* A forward-shaded material pays full shading in the geometry pass and a
  lighting pass that skips its pixels — the honest cost of screen-space
  context, stated in plan5 and now real.
* The `emissive` G-buffer field is overloaded for preshaded pixels: its
  doc says "rgb = emissive radiance", and for those pixels it carries the
  whole shaded answer. Anything that reads the G-buffer directly (a
  decoding tool, a future SSAO) must route on the id first.
* Two-model sets with no requested channels generate no `ModelExtras`
  struct — WGSL refuses an empty one, and the first such set to reach a
  device (`pbr` + `preshaded`) found the latent bug. The switch's
  signature collapses to `model_id: u32` alone.
* Naming `wxsl.preshaded` without the flag stays legal (the registry and
  the gate iterate it like any model) and shades inconsistently — the
  ADR's word that it is not the mechanism; the mechanism is the flag.
* What this unblocks, with ADR 0054: derivative-driven shading
  (`roughness_aa` and the fwidth family) under the deferred pipeline,
  and the plan4 leftovers that wanted it (spectral iridescence's grazing
  terms, screen-space-context models).
