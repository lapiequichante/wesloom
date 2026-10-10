# 0060. Integrate single scattering as a pure sky node

Date: 2026-10-10

Status: Accepted

## Context

Plan4 S3 has ray/sphere and phase primitives, but no optical integration.
Environment cube imports, subresource writes and prefiltering require a
separate backend contract; a sky evaluator need not wait for those bindings.

## Decision

`lighting.sky_single_scattering` is an original, pure function of planet-centred
origin, viewing direction, direction toward the sun and medium parameters.
Distances are kilometres, coefficients inverse kilometres. Midpoint quadrature
integrates exponential Rayleigh/Mie densities along both the view and solar
paths. RGB Beer-Lambert extinction includes Mie absorption; the opaque planet
clips the view and occludes sunlight. Macro variables bound the two loop budgets.

It returns linear in-scattered radiance, without exposure, a sun disc, ground
reflection, stars, ozone or multiple scattering. Zero-length directions, an
observer below ground and rays missing the atmosphere return black. A matching
`space.equirect_to_direction` node makes an editable screen graph usable as an
equirectangular sky preview. No new frame bindings or renderer pass kinds.

## Alternatives considered

- A two-colour gradient is not optical integration.
- Coupling the evaluator to camera/environment bindings would prevent surface,
  screen and eventual bake graphs from sharing the same numerical function.

## Consequences

This implements S3-C's radiance evaluator, not S3-D's lighting environment.
The screen preview retains the existing graph-effect image input solely for its
extent. HDR imports, cube faces/mips, diffuse convolution and GGX prefiltering
remain open and need a further ADR covering both wgpu and Dawn. Stock lighting
and pipelines remain unchanged.

These follow-ups subsequently landed in ADRs 0061–0064. The pure evaluator stays
independent; its result now feeds material IBL and a separate camera background.
