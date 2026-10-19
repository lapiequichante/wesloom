# 72. Integrate rectangular area lights with shadowed quadrature

Date: 2026-10-11

Status: Accepted

Amends [0071](0071-fit-cascades-and-pack-point-shadow-faces.md).

## Context

The gallery needs a finite emitter whose size changes its illumination and
penumbra. The shared light budget has grown to sixteen uniform rows; this
is a fixed budget, not clustered or compute-built lighting.

## Decision

A rectangular light stores its centre, two perpendicular half-edge vectors,
colour and emitted radiance. Their cross product defines the emitting side.
Integrate direct lighting with a deterministic 4x4 Gauss quadrature, including
emitter cosine, area weight and inverse-square falloff. Each point evaluates
the existing material BRDF and its own six-face shadow lookup, on both paths.
A shadowed rectangle therefore claims one indivisible run of 96 atlas tiles.
Analytical receiver-plane PCF supports perspective sample faces as well as
orthographic cascades, preventing self-shadowing at coarse resolution while
retaining the authored normal offset.

Extend the 2048-square atlas from sixteen to 112 slots by filling its
unused region with ninety-six 64-square tiles; preserve all existing rectangles.
Fixed passes and generated host layouts remain the shared renderer contract.
Enable bytemuck min_const_generics in the device-free frame crate so the
112-element generated table works without GPU-dependent feature unification.

## Alternatives considered

A single centre shadow: does not produce emitter-size-dependent penumbrae.
LTC plus a centre shadow: approximates lighting but still needs a separate
visibility integration, and introduces lookup assets. Four points: visibly separate shadows. More than sixteen points: better quality,
but additional shadow views exceed this bounded atlas policy.

## Consequences

Sixteen-point quadrature is an approximation; large emitters can still show
residual sample lobes, and low-resolution tiles limit detail. This is not LTC, stochastic
sampling or clustered lighting. A rectangle costs sixteen BRDF evaluations and
96 shadow views, while occupying one light row. Regenerate the C header and
rebake offline exports after the light stride and tile-table changes.

A rectangle that casts no shadow no longer uses the fixed points. Its samples
follow the surface's falloff and specular lobe, and it may emit a texture
([ADR 0095](0095-unshadowed-area-lights-sample-where-the-lobe-and-the-falloff-are.md)).
