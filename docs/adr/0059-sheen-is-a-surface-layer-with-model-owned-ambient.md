# 0059. Sheen is a surface layer with model-owned ambient

Date: 2026-10-10

Status: Accepted

Extends ADR 0008 and ADR 0028; completes plan4 S2-D's sheen inputs.

## Context

The historical cloth model derives sheen tint and roughness from the base
surface. Graph-authored sheen needs its own inputs and stored channel.
The shared GGX ambient cannot describe a Charlie layer, even if the direct
light loop can. Replacing cloth would change existing materials and widen
their portable six-model G-buffer beyond its 32-byte budget.

## Decision

Append optional surface sockets `sheen_color` (linear RGB, default black)
and `sheen_roughness` (default 0.5). Add `wxsl.sheen` (id 7): PBR under a
Charlie/Neubelt sheen layer, stored as RGB colour plus perceptual roughness
in one HDR vec4 target. The colour's maximum component controls the base
layer's energy attenuation through the existing hemispherical Charlie fit,
evaluated for view and light directions. Black sheen exactly reproduces
PBR. The historical `wxsl.cloth` and `default_set()` retain their behavior;
sheen, like iridescence, is explicitly selected from `DEFAULT_MODELS` and
fills the portable attachment budget in a single-model set.

A `LightingModel` may name an ambient function in its own module:
`fn <ambient>(surface: Surface, ctx: SurfaceContext, extra: vec4f) -> vec3f`.
Absent means the existing shared GGX ambient. The generator selects both
direct and ambient functions using the same model id and stored extra.
Occlusion, exposure and emissive remain applied once by the shared loop.
The ambient function participates in the set signature.

Sheen's ambient attenuates the underlying GGX response and uses the shipped
`sheen_ibl_response` fit for a Charlie hemispherical response to the current
two-colour environment. This is an analytic approximation, not a cubemap
prefilter; S3 still owns HDR environment imports and distribution-compatible
image prefiltering. The fitted response is reused with its existing notices.

## Alternatives considered

Changing cloth would alter old graphs and their G-buffer budget. A layer
only in the direct-light loop leaves ambient unchanged and unbalanced.
Special-casing sheen in the shared loop would make the generator know a
shipped model's name rather than read its registry contract.

## Consequences

The shader ABI adds sockets without changing existing meanings or host
buffer layouts. Both backends consume the same generated lighting and
computed targets. Every future custom ambient owes direct/switch corpus
coverage and render parity; no ambient override changes old models.
