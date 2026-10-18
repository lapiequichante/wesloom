# 0068. Compose optical peels back to front by default

Date: 2026-10-10

Status: Accepted

## Context

ADR 0067's layered ray marcher costs 120 ms/frame on an M5 at 1280×720;
the same scene and geometry peeling without that resolve cost 5 ms. Its
512-step/12-boundary search is not an acceptable default for real-time glass.
The user requests back-to-front composition with front/back thickness, while
retaining the more precise solver for optional future use.

## Decision

Keep the optical interface peel and its two arrays. For disjoint closed
solids, consecutive interfaces form entry/exit pairs. A torus can have two
pairs; the sphere plus torus gallery budgets three. Compose these pairs
from farthest to nearest, ping-ponging linear radiance and nearest composed
depth. Depth peeling supplies per-pixel ordering rather than an object-centre
sort, so submission order is irrelevant.

Each composition uses front/back world positions to estimate thickness,
Snell refraction at both interfaces, Fresnel and Beer absorption. Two local
exit-plane reprojections and two background-depth reprojections are fixed
work, not a ray march. The background being sampled already contains the
farther glass. Clear depth/off-screen samples use the explicit HDR cube.
Colour and R32F depth are separate single-attachment screen passes, matching
the existing effect contract; neither is a storage write or backend exception.

`pass.refraction` has `quality=layered` by default; `quality=precise` selects
the retained ADR 0067 solver. Programmatic plans expose the same choice through
`refraction::append_with_quality`; existing `append` selects the fast default.
No new backend recording branch, dependency, or material ABI is necessary.

## Alternatives considered

* Lowering the ray-march cap: still searches every layer per step and trades
  unpredictable missed exits against frame cost.
* Ordinary alpha over: cheap but cannot deform the already-composed backdrop.
* Only an environment lookup: cannot show one refractive object through another.

## Consequences

This is an explicit local-thickness/screen-space approximation, not exact
ray transport. Disjoint closed solids and their front/back pairing remain
the contract; nested/interpenetrating media, hidden/off-screen geometry,
rough transmission and caustics are not promised. The original precision
implementation remains shipped and compiled, but never runs by default.
Tests cover pair order, thickness, IOR=1, successive objects, opaque masks,
both quality selections and backend parity. A warmed gallery measurement
must compare against the old solve and check that bakes/shaders remain cached.

The ignored `gallery` test `glass_quality_timing` measures the same warmed
1280×720 sky scene with GPU completion on an Apple M5: layered 7.89 ms,
precise 165.75 ms, then layered 8.41 ms after switching back. This is roughly
20× faster, not a portable timing guarantee. The local HDR screenshot also
retains the sphere's inverted environment and the torus's blue absorption.

[ADR 0093](0093-thin-walled-glass-and-the-transmission-factor-are-optical-layer-data.md)
adds thin-walled layers and a transmission factor to this composition: a
third optical target, and a thin front composed as one interface rather
than as half of a pair.
