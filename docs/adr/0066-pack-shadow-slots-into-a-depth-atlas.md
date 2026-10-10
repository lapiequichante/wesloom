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

This lands the atlas part of N7, not camera-following cascades, six-face
point shadows or compute light lists. The light budget remains four.
Changing the frame texture dimension and light stride requires regenerating
host headers and backend exports. Existing exported bundles must be rebaked.
Gallery orbit controls are example-local; moving cameras also supply the
previous camera to temporal passes, while headless screenshots stay fixed.
