# 95. Unshadowed area lights sample where the lobe and the falloff are

Date: 2026-10-19

Status: Accepted

Amends [0072](0072-integrate-rectangular-area-lights-with-shadowed-quadrature.md).

## Context

ADR 0072 integrates a rectangle at sixteen fixed Gauss points, because
each point needs its own shadow views. A light that casts no shadow has
no such constraint, but it inherits the fixed points: on a glossy
surface each one reflects as its own small highlight, so the reflection
is sixteen dots. We also want a texture on the emitter (a screen, a
stained-glass panel) and tube lights (neons). Both have the same
problem, and a tube is worse: seen at a glancing angle on a floor, its
reflection is thinner than the gap between samples.

The light loop evaluates each lighting model's full BRDF once per
sample, and the models do not split diffuse from specular. LTC and
representative-point methods need that split, so they would mean
changing every model.

## Decision

- An unshadowed rectangle (one that claimed no tiles) and a tube
  (`LightKind::Tube`: a centre, a half-axis in `area_u`, and `radius` in
  what used to be the light row's first padding) draw their samples
  from two Cauchy distributions over the emitter's parameters. One is
  centred on the point nearest the surface, with a scale equal to the
  surface's distance: that is the inverse-square falloff's shape, and it
  becomes uniform from afar. The other is centred where the mirror ray
  meets the emitter, with a scale equal to the lobe's width there. The
  lobe is an ellipse of 2α in the plane of incidence and 2α·cos θ
  across it; for a rectangle the scale is the ellipse's marginal along
  each edge, for a tube its slice along the axis. Each strategy pushes
  the same Gauss grid through its inverse CDF, and the balance heuristic
  combines them.
- A rectangle takes 2 × 16 samples and a tube 2 × 8. The loop passes
  each sample the surface's `specular_lobe` (mirror direction, α,
  normal). The generated light loop calls `sample_light_lobe`; a light
  that has neither kind falls back to `sample_light_value`.
- A rectangle or a tube may name a cookie layer as its emission texture:
  u runs along `area_u`, v runs down `area_v` (a tube reads the middle
  row). Each sample reads it prefiltered to its share of the emitter,
  `1/√(n·density)`, so a sharp reflection reads fine mips and a rough
  one coarse mips. The cookie sampler is trilinear.
- A tube never casts a shadow and is never local.

## Alternatives considered

- **Uniform samples plus a window of samples around the mirror point,
  with strips for the rest.** In 2D, GGX's tails hold about 30% of the
  lobe outside ±3α, so the sparse strips streaked.
- **Uniform plus mirror-centred samples, MIS-combined.** The specular
  part was smooth, but close to a tube the inverse-square peak fell
  between uniform samples, which showed as lumps.
- **LTC, or representative points.** Each needs a diffuse/specular
  split in every lighting model, and LTC needs lookup tables.
- **More fixed points.** This costs as much and still dots below some
  roughness.

## Consequences

- Glossy reflections of rectangles and tubes are continuous, and a
  textured emitter reflects as its texture blurred by roughness. Close
  to an emitter the light is smooth.
- An unshadowed rectangle now costs 32 BRDF evaluations instead of 16. A
  shadowed one is unchanged.
- The quadrature is deterministic and approximate. Very anisotropic
  cases can still show faint banding, and the emitter's texture only
  shows down to the sample spacing.
- `LightUniform._padding1` is now `radius`, so the C host header is
  regenerated.
- The gallery's `area-texture` and `neon` demos are the visual check.
  Their light textures come from `light_textures.wxsl`, drawn into the
  cookie array with mips at startup.
- A lighting model with an `area` function now integrates these lights
  analytically, and the samples above are what a model without one gets
  ([ADR 0096](0096-area-lights-are-integrated-with-ltc-by-the-models-that-can.md)).
