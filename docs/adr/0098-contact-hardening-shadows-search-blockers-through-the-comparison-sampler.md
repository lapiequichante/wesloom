# 98. Contact-hardening shadows search blockers through the comparison sampler

Date: 2026-10-19

Status: Accepted

Amends [0026](0026-shadows-a-view-per-light-and-two-flags-on-the-material.md)
and [0071](0071-fit-cascades-and-pack-point-shadow-faces.md).

## Context

A shadow-mapped light filters every edge with the same 3×3 PCF kernel,
so a shadow is equally sharp at its caster's foot and two metres away. A
real lamp has a size: its penumbra grows with the distance between
blocker and receiver. Percentage-closer soft shadows (Fernando, 2005)
get that by averaging the blockers' depth around the receiver and
scaling the filter by the similar-triangles penumbra. The blocker search
reads raw depth, and the atlas cannot be read that way under WebGL2: naga
makes it a `sampler2DShadow`, and a second, non-comparison sampler on it
has no GLSL spelling (ADR 0075).

## Decision

- `Light::with_source_radius` gives a point, spot or directional light a
  source size: world units for a point or spot, an angular radius for a
  directional light. It rides in the light row's `radius`, which only a
  tube read before. A tube casts no shadow, so the two meanings never
  meet. A directional light stores the tangent of the angle. Zero keeps
  the fixed kernel. A rectangle keeps its own quadrature (ADR 0072).
- `shadow.wxsl`'s `soft_shadow` runs only when the radius is nonzero:
  - Twelve search taps on a Vogel disk, turned per pixel. The disk is
    sized for a blocker halfway to a point-like source, or for the widest
    penumbra under a parallel one.
  - A tap that the receiver-plane depth finds shadowed places its blocker
    by four halvings of the penumbra it would cast, in atlas texels over
    [0, 32]. Each halving is one more comparison. The tile's
    view-projection gives the depth of each candidate: a perspective
    tile's rows 2 and 3 are parallel, which gives depth = A + B / reach,
    and an orthographic tile's depth is linear.
  - A blocker in the narrowest interval (contact, or the receiver seen
    past its bias) marks the point as shadowed. It is left out of the
    average.
  - The filter is 24 hardware-bilinear comparisons on a Vogel disk as
    wide as the average penumbra, with each tap's depth taken from the
    receiver plane.

## Alternatives considered

- **Read depth with `textureLoad`.** It works on wgpu's native backends,
  but the GL backend draws no shadows at all (ADR 0075's incomplete
  texture).
- **A second copy of the atlas as `r32float`.** One more binding in the
  frame group, a copy pass per frame, and a change to the Dawn host.
- **Bisecting depth instead of penumbra.** A directional cascade's depth
  range is tens of metres, so four halvings leave one-metre steps, and
  every caster within a metre of the ground reads as contact.
- **Variance or moment shadow maps.** They need a filterable, blurred
  copy of the atlas and they leak light. That is a different shadow
  pipeline, not a knob.

## Consequences

- A soft light costs 12 comparisons per pixel where nothing is shadowed.
  Inside a search region it costs up to 12 × 5 + 24 = 84. A light with no
  radius costs what it did before.
- Penumbrae stop at 32 atlas texels, so their world width scales with the
  tile's resolution. A blocker closer than halfway to a point-like source
  has its penumbra underestimated. Turning the disk per pixel trades
  banding for grain, which TAA resolves.
- A penumbra is clamped to its own tile, so a wide one shows a seam at a
  point light's cube-face edges.
- The light row's layout and the C header are unchanged. Only the
  meaning of `radius` grows.
- The gallery's `soft-shadow` demo (a 20 cm lamp over pillars and
  floating cubes) and its `headlights` demo show it. `shadows.rs` checks
  the penumbra on both sides of the edge, and that open ground stays lit,
  for a point and a directional light on both pipelines.
