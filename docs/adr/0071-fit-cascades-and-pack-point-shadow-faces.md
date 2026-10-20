# 71. Fit cascades and pack point-shadow faces into shared tile runs

Date: 2026-10-11

Status: Accepted

Amends [0066](0066-pack-shadow-slots-into-a-depth-atlas.md).

## Context

N7 needs camera-following directional shadows and omnidirectional point shadows.
One matrix per light cannot describe either. Both render paths and offline
exports must consume the same frame data.

## Decision

Each light owns a contiguous run in the shared shadow-tile table. Fixed
per-tile passes and dynamic camera offsets consume the same computed views
as the sampling shader. A directional light uses an origin box or up to four
camera-fitted cascades; a point light uses six 90-degree views, in axis order.
A light whose whole run does not fit casts no shadow.

Cascades use view-depth splits, eight-corner bounds and texel-snapped bounding
spheres at the allocated tile resolution. Their depth bounds include casters
towards the light. A 10% overlap blends adjacent lookups; receiver-plane PCF
retains a fixed normal offset. Cube-face selection uses the offset position.

## Alternatives considered

Generate passes each frame: would invalidate schedules and pipeline caches.
Keep a matrix on each light: cannot represent a cascade or cube-face run.

## Consequences

Shadow matrices and rectangles belong to tiles, not lights. The host and
shader split/face conventions must agree. GPU tests cover camera movement,
acne, shadow extent, point shadows and face edges. Headers and exported bundles
must be regenerated when the tile-table shape changes.

Drift (2026-10-20): the splits and slice spheres no longer depend on the
camera's near plane. The logarithmic splits start from a thousandth of the
range and the first slice from the eye, so a camera whose near plane moves
every frame (the drive's chase camera) keeps every cascade's texel size, and
its snapped grid stays put instead of popping. `Light::shadow_cascade_split`
weighs logarithmic against even splits (0.5 as before), and
`Light::shadow_caster_reach` takes casters further towards the light than
`shadow_distance`, for tall casters under a low light. The last cascade
fades out over the same 10% band the cascades blend over, rather than ending
on a line.
