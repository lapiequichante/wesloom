# 0069. Cache local colour and distance probes in the glass demo

Date: 2026-10-10

Status: Superseded by 0070 (glass rendering); probe ownership/cache retained

## Context

The screen-space glass demo cannot recover nearby geometry outside the main
camera. A second demo should compare local probes and include a mirror that
reflects the two glass objects, without restarting expensive ray marching.

## Decision

Add the application-owned `glass-probes` gallery demo alongside `glass`.
Each of the two glass objects and the mirror has a 256-square, six-face linear
RGBA16F colour cube and R16F radial-distance cube. Zero distance means sky.
Capture through ordinary renderer/pass/effect descriptions, excluding the
probe's owner. Capture glass with ADR 0068 and capture the mirror against the
global environment: this is one local bounce, not a recursive solve.

The demo shares the existing optical exit reconstruction, Snell/Fresnel and
Beer model. Glass uses its own probe at the reconstructed exit; the mirror
uses its own probe along the reflected world ray. Three bounded distance
reprojections correct local parallax; discontinuities/missing geometry remain
approximations. Glass is composed by object far-to-near, with two pairs for
the torus. Opaque depth still masks glass behind the mirror. There is no
ground plane in this demo or its captures.

Camera output is staged in 2D and both colour and distance are flipped in V
through screen effects before writing the cube faces. This matches WebGPU's
cube convention without changing projection matrices or face winding.

Probes belong to the application and are cached for this static scene.
Changing HDR/sky invalidates them; camera orbit and viewport resize do not.
The demo's fixed object transforms/materials are part of that cache contract;
future live edits must invalidate captures too. No backend recording branch,
material ABI field, dependency or change to the original demo is needed.

## Consequences

Off-screen local objects can appear in reflection/refraction. Initial capture
costs eighteen views, but steady frames do not recapture them. Object-centre
ordering, finite cube resolution, radial visibility and one-bounce captures
are explicitly approximate, especially at overlap/disocclusion boundaries.
Tests must check cube orientation, owner exclusion, local glass in the mirror,
source invalidation and unchanged capture counts/cache misses during orbit.
The red-top/blue-bottom HDR test checks that the glass sphere inverts its
environment. A power-of-two safety-scale test checks equal physical radiance
through both glass and mirror, without double exposure or float16 clipping.

Warmed 1280×720 sky measurements on an Apple M5: initial captures plus first
frame 245.44 ms, then 7.71 ms/frame, with eighteen captures and unchanged
shader misses. These are measurements, not portable timing guarantees.
