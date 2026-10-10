# 0058. Iridescence is authored on the surface and carried by a model channel

Date: 2026-10-10

Status: Accepted

Extends ADR 0008 and ADR 0028 (plan4 S2-D).

## Context

The spectral iridescence node exists, but colouring albedo with its response
does not change the specular light loop. A film needs graph-authored strength,
thickness and IOR, transported from the fragment to the model. Recomputing a
model's pack from the reconstructed deferred surface loses these inputs.

## Decision

Append `iridescence_strength` (default 0), `iridescence_thickness` (300 nm)
and `iridescence_ior` (1.3) to the surface ABI and output sockets. Old graphs
keep their defaults. `wxsl.iridescent` (id 6) requests one HDR vec4 channel:
strength, thickness in nm, IOR, unused. Its direct-light GGX Fresnel blends
the existing spectral response at the half-vector angle, with a bounded
reflection budget subtracted from diffuse. Zero strength or thickness uses
the original PBR function exactly.

Forward packs once before the light loop. Deferred passes the stored extra
to the same loop, including in a single-model pipeline; it never regenerates
the film channel from a reconstructed surface. The shared ambient remains
the existing GGX approximation; compatible spectral IBL is follow-up work
with S3, not claimed here.

The single-model layout costs the portable 32-byte floor. Adding a dispatch
id exceeds it, so this model is explicitly selected via `DEFAULT_MODELS`,
not automatically enabled by the historical `default_set()` helper. That
helper retains its existing six-model set and layout. The existing budget
checks refuse oversized sets. Registration and computed export layouts are
shared with the C ABI; no backend-specific schema is added.

## Alternatives considered

Colouring albedo or emissive cannot represent a directional specular film.
Adding the model to every default set would make existing portable demos
exceed the attachment budget. Packing thickness into a normalized colour
would unnecessarily quantize the spectral input.

## Consequences

The stock PBR layout and old documents remain unchanged. The added sockets
are optional and no existing socket changes meaning, so the document ABI
revision stays unchanged, like other additive registry entries. Older builds
cannot validate a graph using the new sockets or model. Sheen inputs,
subsurface and physical IBL remain separate
increments. Tests cover the single-model channel, switch code generation,
zero-film equivalence and per-fragment thickness across both render paths.
