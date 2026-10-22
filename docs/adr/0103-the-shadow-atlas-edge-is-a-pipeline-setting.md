# 0103. The shadow atlas edge is a pipeline setting

Date: 2026-10-22

Status: Accepted

Amends the atlas of [0071](0071-fit-cascades-and-pack-point-shadow-faces.md).

## Context

Every shadow tile lives in one depth atlas of `abi::SHADOW_ATLAS_SIZE`²
(2048²), packed by `abi::shadow_tile`. An application had no way to trade
memory and fill rate for sharper shadows, or the reverse: the drive demo
wants a quality setting that scales its shadows from half to twice the
default. The shader already reads the atlas size with `textureDimensions`
and the scene uniform carries each tile's rect relative to the atlas, so
only the host side assumed the constant.

## Decision

- `PipelineConfig::shadow_atlas_size` sets the atlas edge of a document
  pipeline: a power of two from `abi::MIN_SHADOW_ATLAS_SIZE` (512) to
  `abi::MAX_SHADOW_ATLAS_SIZE` (8192), checked by
  `abi::valid_shadow_atlas_size`; anything else is
  `PipelineError::ShadowAtlasSize`, named by the `source.lights` node.
  The default is `SHADOW_ATLAS_SIZE`.
- The tiles keep their packing, scaled with the edge
  (`abi::shadow_tile_in`): a tile's rect relative to the atlas, and so the
  scene uniform and `shadow.wxsl`, do not change.
- `Environment::shadow_atlas_size` tells the cascades which texel grid to
  snap to. The renderer refuses a frame whose environment disagrees with
  the installed pass list (`RenderError::ShadowAtlasSize`);
  `Renderer::shadow_atlas_size` says what the pass list has.
- The hand-built reference graphs and the C ABI keep `SHADOW_ATLAS_SIZE`:
  the header's constant still sizes the Dawn atlas.

## Alternatives considered

A uniform carrying the atlas size: unnecessary, `textureDimensions`
already gives it to the shader. Reading the size from the renderer inside
`Environment`: the environment is host data the renderer only borrows, so
the frame states it and the renderer checks it. A different tile ladder
per size: it would change which slot gets how many texels relative to
the others, and the ladder's order is what applications plan around.

## Consequences

`PipelineConfig` and `Environment` gain a field, so code building either
with a struct literal adds it. A host changing the atlas sets both, or its
frames are refused rather than shimmering. Below 2048 the smallest tiles
are 16², usable only for distant point-light faces. Making the size a
C ABI setting is follow-up work: the header, Dawn's atlas and the parity
harness would read it from the config.
