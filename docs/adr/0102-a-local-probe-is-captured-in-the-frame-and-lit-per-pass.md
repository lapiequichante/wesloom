# 102. A local probe is captured in the frame and read per pass

Date: 2026-10-20

Status: Accepted

Amends [0063](0063-environment-cubes-are-explicit-frame-resources.md)

## Context

A frame has one environment: the diffuse and GGX cubes a graph declares
are bound in the frame group and read by every lit material (ADR 0063).
For a car in the drive demo (ADR 0100), that environment is the sky. Its
paint and glass mirror a sky with no road, no city and no lamps, however
close those are. The gallery's local probes (ADR 0069) get around this
with application materials that sample their own cube, captured by a
second renderer. That route does not serve a glTF car, whose materials
are the stock lighting models.

Two things are missing. A frame cannot render from a point that is not
the camera or a shadow tile. And a draw cannot read a cube other than the
frame's.

## Decision

- **Probe views.** `PassView::Probe { face }` adds six views after the
  shadow tiles, from `Environment::probe` (a centre, a near and a far
  plane). Face `f` looks along `CUBE_FACE_DIRECTIONS[f]`, 90° square,
  with its up vector chosen so that the rendered image, flipped in V, is
  the WebGPU cube face. The flip is the composite pass's job: projection
  matrices and winding stay as every other view has them. With no probe,
  the six slots hold the camera's view, as unclaimed shadow tiles do.
- **A recipe.** `ibl::append_probe` captures and filters the cube every
  frame:
  - six forward passes from the probe views, drawing a tag selector;
  - the `wxsl.probe_face` composite, which puts the environment's
    radiance behind them, flips each face into the cube, and divides the
    shaded colour by the exposure and the environment scale, so that the
    probe holds the frame cube's units;
  - the filtered chain and the convolutions of `append_filtered_bake`,
    on the `per frame` policy, each texel importance-sampled `samples`
    times. `wxsl.ibl_diffuse` and `wxsl.ibl_specular` gain that
    parameter. Its default, 0, keeps a bake's quadrature: the 32-texel
    grid of a broad lobe, 4096 samples for a sharp one. That quadrature
    cannot run every frame. One texel's loop over the grid is 6144
    dependent reads, about 2.5 ms per pass on the drive's GPU however
    small the face, and 60 such passes make a probe.
- **Environment per pass.** `PassDesc::environment` names the diffuse and
  specular cubes that one pass binds in place of the frame's. The
  renderer keeps one frame bind group per override. The scheduler holds
  an override to the frame maps' rules and to their specular mip count,
  since the scene uniform carries one level count. It also orders the
  pass after the override's writers. An override needs the frame maps to
  be declared, since it changes what they point at, not whether the
  environment is on.
- **Documents and the C ABI** declare neither. The field is left out of
  a serialized pass list when it is unset, so exports do not change. A
  probe view reaches Dawn as one more view slot, as a shadow tile does.

## Alternatives considered

- **Make the probe the frame's environment.** No renderer change, but the
  road and the towers would mirror a city seen from the car.
- **Render each face into the cube directly**, with a mirrored
  projection. That saves the composite pass, but it flips every pass's
  winding and `front_facing`, and both backends would have to agree on
  that. The 2D target and the flip are ordinary pass data.
- **A second renderer, as the gallery does.** It needs six more renders a
  frame and a copy into a cube the frame does not own. That also does
  nothing about the car's materials reading the frame's cube.

## Consequences

- A per-frame probe at 128² with eight mips is about 160 small passes:
  six forward passes, six composites, the source chain, and six diffuse
  and 48 GGX faces. The drive pays it every frame.
- Amended: `append_probe` takes the policy its passes run on. Under any
  but `PerFrame` its faces are persistent, as a skipping pass's targets
  must be (ADR 0035). The drive runs its probe `OnDemand` and marks it
  every N frames, its `probe every` slider; the car reflects the last
  capture in between.
- Its forward passes see the camera's light grid. A local light binned
  only outside the camera's frustum does not reach the probe. A uniform
  light (the sun, the moon, the shadowed lamps, the headlights) does.
- Deferred lighting is one screen pass over every pixel, so an override
  on it would apply to the whole frame. The drive's deferred frame keeps
  the sky on the car's body, and only its glass reads the probe.
- The editor's views and the probe ignore probe views, as they ignore
  shadow tiles.
