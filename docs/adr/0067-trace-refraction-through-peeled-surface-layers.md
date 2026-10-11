# 0067. Trace refraction through peeled surface layers

Date: 2026-10-10

Status: Superseded by [0068](0068-compose-optical-peels-back-to-front-by-default.md)

The interface ABI and ray-marched implementation remain available as the
opt-in `quality=precise` path. ADR 0068 replaces the default composition.

## Context

Alpha peeling cannot bend light. Sampling only the opaque background cannot
show one glass object through another. A solid sphere needs both interfaces
to produce its lens inversion; a uniform-thickness UV offset is insufficient.

## Decision

Add `pass.refraction`, separate from the unchanged alpha `pass.peel`.
It selects the `refractive` tag and gathers the nearest optical interfaces,
including back faces, with the baseline depth-peel/equal-shade pair. A new
`refraction_surface` stage stores world normal/depth in RGBA32F and
unit-distance transmission/signed IOR in RGBA16F texture arrays. A separate
`refraction_depth` row keeps a float-scale separation tolerance (`2e-7`),
not the alpha peel's `1e-5` bias that can remove thin grazing exits.
Surface sockets are `ior`
(default 1.5) and `attenuation_distance` (default 1 world unit); `base_color`
is the transmitted colour at that distance.

A fullscreen effect traces these layered depth fields, applying Snell
refraction, dielectric Fresnel, total internal reflection and Beer absorption.
Opaque screen-space hits sample the lit scene; missing/off-screen hits sample
an explicit HDR radiance cube in the final ray direction. All calculations
precede tonemap. The pass's `surface_count` (1..4, default 2) budgets closed
volume crossings, doubled for their entry/exit interfaces; each interface
costs a depth pass and an equal-depth gather. A torus may cross twice (four
interfaces); together with a sphere it needs `surface_count=3` (six interfaces,
twelve geometry passes). Two arrays keep the binding cost independent of
the count, within baseline texture limits. This optical budget is separate
from the unchanged alpha peel's eight-pass budget. No float blending or
ray-tracing API is required. The recipe is shared frame data, also exposed
programmatically for host-composed IBL graphs.

Existing alpha materials, sorting tiers and layer budgets are unchanged.
Optical glass uses its dedicated tag, not low alpha. The initial optical
model is clear glass in disjoint closed solids; rough transmission, nested
media, dispersion, caustics and cameras inside a solid are not promised.

## Alternatives considered

* Back-to-front colour warps compose objects but do not retain the incoming
  refracted ray at the next interface. Strong lenses need both interfaces.
* A background capture per object is sorting/intersection dependent and
  still needs exit-surface reconstruction for a sphere.
* Full scene ray tracing removes visibility limits but changes the baseline
  backend and geometry contract.

Technique references (original code, no copied implementation):
[McGuire and Mara, layered screen-space ray tracing](https://jcgt.org/published/0003/04/04/),
[Imai et al., front/back depth and two refractions](https://cgg.cs.tsukuba.ac.jp/projects/2014/liquid_rendering/nicoint_2014.pdf).

## Consequences

Up to eight visible interfaces are bounded, not arbitrary transparent scene ray
tracing. Hidden/off-screen geometry is unavailable; HDR fallback cannot
recover it. Thin features and layer truncation can miss ray hits. When a bent
exit is missed, its visible entry/exit camera chord supplies a local thickness
and exit-normal approximation; absorption and the second refraction still
run, rather than sampling HDR with a ray left inside the glass. Ray steps
follow contiguous half-pixel footprints, bounded to 512 steps and 6.4 world
units per segment. Interpolation
never mixes uncovered texels or opposing interface normals; a depth-layer
discontinuity is not accepted as a ray hit. The gallery
shows a clear sphere and slightly blue smoked torus, orbitable into overlap.
Tests cover IOR=1, consecutive solids, submission order, opaque occlusion
and the unchanged alpha path. All four torus layers are also checked for
alternating entry/exit orientation. A white-environment torus test checks that
blue/red increases with geometric tube thickness, not just a surface tint.
