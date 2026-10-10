# 0063. Environment cubes are explicit frame resources

Date: 2026-10-10

Status: Accepted

## Context

S3-D's convolutions need real readers and HDR host input. Frame-group reads
must order lighting after every face writer without adding pass-group bindings.

## Decision

A frame graph optionally declares its diffuse and GGX cubes. Scheduling expands
these into dependency reads for forward-lit geometry and the lighting effect,
without changing their recorded pass-group interfaces. Cubes must be stable,
pooled stable rgba16float textures. Both backends bind placeholders while a
pass writes an environment cube. Former Scene padding floats become the
environment-enabled flag and radiance scale, leaving its size and offsets unchanged. Bindings 9/10
carry the cubes; old documents still use the analytic environment by default.

The default ambient response samples irradiance/PI along world normal and GGX
radiance along world reflection, with trilinear roughness LOD and the existing
BRDF LUT. Model-specific ambient responses (notably Charlie sheen) stay analytic:
a GGX convolution is not a Charlie convolution.

HDR decoding stays with the application. The gallery enables the existing
image dev-dependency's Radiance decoder, uploading validated linear RGB as
rgba32float (manual equirect filtering needs no float32 filtering feature).
No image decoder is added to any runtime library. The supplied HDR is a local
test asset, not an embedded or redistributed dependency; sky is the fallback.
The upload scales peaks to at most 16384 before convolution, recording a
positive multiplier on the graph to restore source radiance in the light loop.
Thus a sun exceeding float16's range is not silently clipped. The caller still
chooses frame exposure, as with every other HDR light source.

`append_filtered_bake` builds a full source chain by box filtering separate
single-level staging cubes, then copying each into the radiance cube's mip.
This avoids read/write aliasing without subresource-aware hazards. These are
filtered texels, not additional GGX convolutions; PDF-selected LOD now has real
levels to sample. Finite quadrature and cubemap resolution remain approximations.

Small bright HDR emitters exposed sparse-hit artifacts in the original cosine
sampling, even with PDF-selected source mips. Diffuse now integrates a filtered
cube grid deterministically, weighted by each texel's exact solid angle and N.L.
GGX lobes with roughness >= 0.35 use the same grid weighted by D(H)*N.L
(V=N); sharper lobes retain importance sampling, increased to 4096 samples.
Normalization preserves constant environments. The source grid is bounded at
32 texels per face, so one-level sources without a filtered chain remain less
reliable for tiny emitters. The existing linear roughness-to-mip contract stays
unchanged in the bake and reader; this is not three.js's nonlinear PMREM layout.

The hybrid strategy was checked against `src/extras/PMREMGenerator.js` in the
local three.js reference at `8d486d4cc1a2b420b2585ea52a3e0f783c919282`
(`_applyPMREM` / `_getIntegrationMaterial`, MIT). Technique-only comparison,
not a code port: this shader uses original WXSL, native cube faces, exact texel
solid angles and its own work/roughness thresholds. No reference dependency or
upstream code is shipped. A synthetic polar emitter's closed-form cosine
integral and the real HDR grids gate the bright-emitter regression.

## Consequences

Environment lighting has explicit scheduler edges and identical host layouts
on wgpu/Dawn. Replacing an imported view or effect invalidates Once passes
conservatively; repeated imports of the identical view are free. In-place uploads
and edited source parameters explicitly call `Renderer::invalidate_bakes`.
ADR 0064 completes pipeline-document authoring, application-owned native Dawn
HDR decoding/imports and the depth-aware camera background. Both generated
sources and imported HDR run through the same bake and lighting.
The demo material's procedural properties use interpolated object position,
with the nonlinear noise pinned to Fragment so rotation does not move its pattern.

The gallery's stationary `ibl-grid` / `ibl-grid-sky` compare exact 5×5 uniform
roughness/metallic ranges under HDR and sky, with a matte grey floor and no
direct lamps. The sky graph's extent input matches its equirectangular output;
using the HDR view's size (or a 1×1 placeholder) would normalize its UVs wrongly.
Frozen-frame tests pin bake persistence and source switching. This diagnostic
does not claim temporal antialiasing for a rotating procedural material.
