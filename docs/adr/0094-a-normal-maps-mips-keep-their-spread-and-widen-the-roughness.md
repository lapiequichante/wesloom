# 0094. A normal map's mips keep their spread, and it widens the roughness

Date: 2026-10-19

Status: Accepted

## Context

Scene textures get their mips on the CPU as a 2×2 box filter of the stored
values (ADR 0061). For a normal map that loses the detail twice: the
averaged vector is renormalised in the shader, so a mip of a flake or
brushed-metal map turns flat, and the spread of normals it averaged away,
which is what a distant viewer sees as a wider and dimmer highlight,
disappears. Metallic car paint shows it worst: a flake map tiled 80 times
with only a roughness factor, so the paint is a sparkling lobe near the
camera, a mirror a few metres away, and the boundary between two mips
reads as a line on the body. glTF has no roughness texture to bake the
spread into in that case.

## Decision

- `ColorSpace::Normal` (`"normal"` in a scene document) marks a
  tangent-space normal map. Its mip chain keeps, in f32, the unnormalised
  mean of the full-size unit normals each texel covers. RGB stores the
  mean direction; alpha stores `√(2/κ)`, the GGX alpha a von Mises–Fisher
  lobe of mean length `r` adds, with `κ = (3r − r³) / (1 − r²)`. Alpha is
  0 at full size. Storing an alpha rather than a variance keeps 8-bit
  precision where the spread is small.
- `lighting.normal_spread_roughness` folds it in: `α'² = α² + (s · spread)²`,
  `α = roughness²`, `s` the normal map's strength. It runs before the
  surface output, so the G-buffer stores the widened roughness and both
  pipelines agree.
- The glTF importer reads `normalTexture` and `clearcoatNormalTexture` in
  the `normal` space and widens the matching roughness. A texture also
  read as data becomes a second scene texture.
- Scene samplers whose filters are all linear ask for 16× anisotropic
  filtering; wgpu clamps it to 1 where the adapter lacks it.

## Alternatives considered

- Toksvig in the shader from the unnormalised length: 8-bit RGB makes
  `1 − r` noisy exactly where the surface is smooth.
- Baking the spread into the roughness texture: there often is none, it
  may be packed with metalness and occlusion, and it may use another UV
  set or transform.
- A GPU mip generator: the CPU chain is already there and runs once per
  load.

## Consequences

A hand-written material samples a `normal` texture and wires its alpha
into the new node to get the same filtering. Roughness textures are still
box-filtered in perceptual roughness rather than in `α²`. A build before
this one refuses a scene document that uses `"normal"` as an unknown
colour space. ADR 0079's list of colour spaces gains `normal`.
