# 0070. Render local-probe glass as opaque surfaces

Date: 2026-10-10

Status: Accepted

## Context

The hybrid probe/screen composition of ADR 0069 still peels the sphere and
warps screen colour. Probe mode must instead own complete surface radiance
and participate in ordinary opaque visibility.

## Decision

Supersede ADR 0069's glass rendering, retaining its local colour/distance
cubes, owner exclusion, cache and orientation contract. Sphere glass computes
an analytic spherical exit and samples only its probe. Torus glass alone
gathers four self interfaces using existing optical stages and depth bounds.
An opaque forward material reads those normals/depths to estimate two
traversals and Beer absorption; no other object enters this peel.

All three visible objects write opaque depth and shade without blending.
There are no screen-colour refraction reads, object sorting, or radiance/depth
composition ping-pongs. The three objects form a triangle. Generate every
probe twice: first against global HDR, then against the completed first
generation of all three probes. The second generation writes separate final
cubes, so capture order cannot mix generations or create read/write feedback.
This bounded two-generation bake includes glass in the mirror and the mirror
in glass, not an unbounded recursive solve. Captures use the same opaque paths.
The original `glass` is unchanged.

## Consequences

The sphere no longer pays peel passes. Only torus interface bounds advance
between depth gathers; this is not colour composition. Torus exit reprojection
remains approximate at silhouettes/disocclusions. Local cubes remain
finite-resolution, view-dependent finite-bounce approximations, not recursive
transport. Tests enforce opaque state, torus-only gathering and absence of
composition passes in main/capture plans, alongside visual/cache tests.

Local sampling intersects the outgoing world ray with captured radial depth,
including its exit-position offset. A bounded 24-sample bracket and eight
bisections validate the radial residual; depth jumps are not accepted as
surface hits. The six-unit query sphere encloses this demo's static neighbours.
Misses sample a separate global HDR cube, never uncorrected local colour
(which could show a neighbour the ray missed). Coarse sampling may still miss
thin/grazing surfaces and cannot recover geometry hidden from the probe.
Conservative sphere bounds for the gallery's fixed neighbours cull empty
rays and restrict the query interval. Like the analytic lens radius, these
application-shader bounds must track any future geometry/transform edits.

With triangular placement and two generations, the ignored timing test on
Apple M5 at 1280×720 measures 295.66 ms for the thirty-six initial captures
plus first frame, then 2.77 ms/frame with unchanged capture count and shader
misses. These are measurements, not guarantees.
Reversing draw submission produces identical pixels; the original HDR
`glass` screenshot remains byte-identical.
The controlled wall query tests displaced ray hits (including an initially
empty angular lookup) and true misses; it fails under the former reprojection.
An orbit test isolates the torus contribution in normalized sphere coordinates
and checks its movement with unchanged captures.
Tests count six faces per object per generation and isolate contributions in
both directions (glass in the mirror, mirror in both glasses). Shader culling
bounds are checked against the non-collinear, disjoint object centres.
