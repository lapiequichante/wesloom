# 66. Pack shadow slots into a depth atlas

Date: 2026-10-10

Status: Accepted

Amends [0026](0026-shadows-a-view-per-light-and-two-flags-on-the-material.md).

## Context

N7 starts with shadow quality: fixed array slices spend equal resolution on
every light. The frame group and both backends must agree on the replacement.

## Decision

Use one 2048² depth texture with stable, non-overlapping tiles assigned by
light slot. The first two slots get 1024², the next four 512², and the next
ten 256². Only the currently supported four light slots are rendered.
The generated light layout carries a normalized rectangle instead of a slice.
Shadow passes retain their dynamic camera offset and declare a viewport/scissor
rectangle. The first clears the whole atlas; subsequent passes load it.
PCF advances by an atlas texel and clamps taps inside the tile's texel centres.

The shared scheduler rejects empty or out-of-bounds raster rectangles and
orders loading depth writers through their write chain. Both wgpu and Dawn
record the same rectangles. Host definitions remain generated (ADR 0050).

## Alternatives considered

Keep array slices: simpler, but prevents this resolution ladder. Dynamic
packing: useful later, but would require per-frame pass rectangles and a
separate allocation policy before cascades and light lists are designed.

## Consequences

The initial landing covered the atlas with four light slots.
[0071](0071-fit-cascades-and-pack-point-shadow-faces.md) adds camera-following
cascades and six-face point shadows;
[0072](0072-integrate-rectangular-area-lights-with-shadowed-quadrature.md) records
sixteen uniform lights, sampled rectangular emission and 112 tiles.
[0073](0073-project-spotlight-cookies-through-the-frame-group.md) adds shadowed
spotlights and projected cookies.
Compute-built/clustered light lists remain open.
Changing the frame texture dimension and light stride requires regenerating
host headers and backend exports. Existing exported bundles must be rebaked.
Gallery orbit controls are example-local; moving cameras also supply the
previous camera to temporal passes, while headless screenshots stay fixed.

Camera-fitted cascades use view-depth splits and full eight-corner slice
bounds, with a stable snapped sphere sized for the actual allocated tile.
Adjacent slices overlap over the last 10% of the preceding interval and
the lookup blends their PCF results there. The depth range includes casters
towards the light. Cascades retain the authored normal offset and correct
each bilinear PCF comparison against the receiver plane at that texel, so
coarser cascades do not require larger offsets that shorten shadows. The
host and shader blend fractions must stay in sync.

Point-light face selection uses the normal-offset receiver position, the
same position projected for the depth comparison. Selecting a face before
the offset can move the lookup outside it and create a lit gap at cube edges.
