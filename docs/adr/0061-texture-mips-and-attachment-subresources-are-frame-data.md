# 0061. Texture mips and attachment subresources are frame data

Date: 2026-10-10

Status: Accepted

## Context

S3-D needs six cube faces and one specular mip per roughness. `Dimension::Cube`
exists, but both backend allocators create only one mip and attachment views
cannot select one. An enum alone does not constitute an IBL pipeline.

## Decision

Texture resources and physical slots carry `mip_levels` (one by default).
Colour/depth attachments carry `mip` alongside `layer` (both zero by default).
Both recorders allocate the declared chain and create a 2D, one-layer,
one-mip attachment view. Whole-resource sampled views keep the full chain.
Mip count participates in allocation/cache identity and transient aliasing.

The device-free scheduler validates counts, dimension/layer consistency and
attachment selectors. Multi-mip resources require fixed extents, so their
maximum legal mip count is known before a device is involved; cube extents
are fixed and square. Dependencies remain conservative and whole-resource:
every reader waits for every writer, and sampling a resource while attaching
another mip of it remains refused. Prefiltering reads a separate source cube.

Imported views remain exactly the host-selected view, not a licence to derive
another view from the underlying texture. Nonzero imported attachment selectors
are refused by name. Multi-mip storage writes and cube storage views are also
refused until write subresource views exist; render-face passes are the supported
route. Offline Dawn bundles default absent new fields to one mip/zero selector.

## Alternatives considered

- One unrelated texture per roughness loses native cube mip sampling.
- Silently selecting the whole mip chain produces invalid attachments.
- A subresource-aware scheduler is unnecessary for separate-source convolution;
  retaining whole-resource hazards is simpler and safe.

## Consequences

Existing one-mip pass lists and imports keep their behavior. This is S3-D's
allocation/recording prerequisite, not HDR import, convolution or environment
lighting. Those remain tracked separately. No shader ABI binding or host layout
changes; the wire additions are additive. Shared-plan tests and GPU face/mip
readback cover both backend implementations before prefiltering is added.
