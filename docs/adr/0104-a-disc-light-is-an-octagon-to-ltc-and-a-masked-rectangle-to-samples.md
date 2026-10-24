# 104. A disc light is an octagon to LTC and a masked rectangle to samples

Date: 2026-10-23

Status: Accepted

Amends [0095](0095-unshadowed-area-lights-sample-where-the-lobe-and-the-falloff-are.md)
and [0096](0096-area-lights-are-integrated-with-ltc-by-the-models-that-can.md).

## Context

Round emitters (a headlight's optic, a downlight, a porthole) were either a
point light, which reflects as a dot and has no size close up, or a
rectangle, which reflects as a square. The drive demo lights each
headlight's housing from within its optic, a few centimetres from the
surfaces it lights, which is where an emitter's size matters most.

## Decision

- `LightKind::Disc` (`Light::disc(centre, normal, radius, …)`) is a
  one-sided ellipse with its semi-axes in `area_u` and `area_v`, its
  emitting side `cross(area_u, area_v)` as for a rectangle, and kind 5 in
  the light row. It never casts a shadow and is never local.
- A model with an `area` function integrates it as the octagon of the same
  area: eight edges in `ltc_edge` instead of four, the horizon clipped
  with the sphere approximation as before. `AreaLight` keeps its layout;
  for `shape` 3 it carries the centre in `corner0` and the semi-axes in
  `corner1` and `corner2`.
- Other models sample it with the unshadowed rectangle's 2 × 16 samples
  over the square around it (ADR 0095). Samples outside the disc carry
  nothing.
- Unlike rectangles and tubes, a disc honours `range`, with the same
  window as a point light measured from its centre. A disc out of range
  costs neither the integral nor the samples.
- A cookie layer maps over the square around the disc.

## Alternatives considered

- **Heitz and Hill's analytic disc under LTC** (2017). It is exact, but it
  needs an ellipse fitted per pixel. The octagon's error is below the LTC
  fit's own.
- **A point light per optic.** It has no size, and it was what we were
  replacing.
- **Concentric mapping of the samples onto the disc.** It would need the
  near and mirror distributions mapped through it; masking keeps
  ADR 0095's distributions as they are.

## Consequences

- With an LTC model, a disc costs about as much as a rectangle. Sampled,
  it costs 32 BRDF evaluations wherever it is in range, and the masked
  edge can show faint banding in sharp reflections.
- `bindings.wxsl` gains `LIGHT_KIND_DISC` and `range_window`. The point
  and spot window now goes through `range_window` too, unchanged.
- The device test is `a_disc_light_lights_like_its_point_from_one_side_within_its_range`
  in `crates/wxsl/tests/shadows.rs`.
- A disc is sampled by `sample_light_at`, never by `sample_light_value`,
  which the light grid's loop shares (ADR 0076). With the disc branch in
  it, every clustered light came out dark on Metal, although no disc is
  ever local (`clustered_lights`).
