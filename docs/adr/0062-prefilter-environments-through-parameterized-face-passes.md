# 0062. Prefilter environments through parameterized face passes

Date: 2026-10-10

Status: Accepted

## Context

ADR 0061 supplies cube attachments and mips. S3-D now needs conversion and
convolution, with different face/roughness values on otherwise identical passes.
ADR 0042 intentionally deferred authored initial parameter values.

## Decision

`PassDesc.parameters` carries initial effect values. The effect's computed
layout validates and packs them over descriptor defaults; wgpu preserves live
tuning across same-label/layout replacements, as before. Offline exports carry
per-pass bytes, consumed by Dawn without duplicating layout computation.

IBL conversion, cosine-weighted diffuse convolution (stores irradiance / PI),
and GGX split-sum prefiltering are ordinary screen effects. A shared recipe
expands them into stable, Once face/mip passes, reading a separate radiance cube.
Faces follow WebGPU +X/-X/+Y/-Y/+Z/-Z, with downward texture V. Specular mip
roughness is mip / (count - 1), with mip zero sampled directly. Sampling uses
the existing filtering sampler; no new lighting ABI is introduced here.

The initial recipe converts a one-mip radiance cube. GGX computes source LOD
from its PDF and texel/sample solid angles, clamped to available levels; richer
host-provided source chains can use the same effect. This initial bake therefore
has finite-sample aliasing on small bright emitters, not an invented mip chain.

## Alternatives considered

Distinct shaders per face bake values into cache identity unnecessarily.
Inferring face/roughness from labels makes names hidden executable state.
In-place mip convolution violates the conservative whole-resource hazards.

## Consequences

This increment provides real conversion and convolution, not material IBL yet.
ADR 0063 subsequently adds the filtered source recipe, explicit material
environment readers, native HDR uploads and invalidation.
HDR file decoding/imports in both hosts, environment bindings, document-level
cube authoring and invalidation on source replacement remain S3-D follow-ups.
Those follow-ups subsequently landed in ADRs 0063–0064; `pass.environment`
authors this recipe without exposing its individual face passes.
Changing initial values under an existing label does not override live tuning;
new labels or a new renderer start from the new recipe values. Programmatic
plans author parameters first; document parameter settings remain deferred.

Shaders are original implementations of the techniques described in
[Filament's IBL theory](https://google.github.io/filament/Filament.html), not
copies of its implementation. Constant HDR, orientations, endpoint roughness
and skipped-bake persistence need GPU proofs, including the offline backend.
