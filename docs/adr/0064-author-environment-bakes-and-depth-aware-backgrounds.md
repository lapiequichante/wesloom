# 0064. Author environment bakes and depth-aware backgrounds

Date: 2026-10-10

Status: Accepted

## Context

S3's filtered cubes work programmatically, but documents cannot declare the
environment and the gallery still clears the camera background. Dawn cannot
import application-decoded HDR pixels into an offline plan.

## Decision

`pass.environment` names one equirectangular colour resource and expands the
existing filtered Once recipe, declaring its diffuse/GGX frame resources. Fixed
cube size, diffuse size, mip count and restoration scale are document settings;
its radiance output can feed ordinary effects. One environment per document.
Imported `resource.color` may use `float` precision for unfilterable rgba32float
HDR. The scheduler remains the validator of physical cube shapes.

A depth-image effect input types a `DepthTarget` socket. The background effect
reads linear colour, opaque depth and the radiance cube; only untouched depth
pixels receive camera-oriented radiance, scaled and exposed before tonemap.
No new pass kind or frame binding. Camera rays unproject near and far points,
so translation and orthographic cameras are handled without a sky mesh.

Dawn accepts validated linear RGB imports by resource label, using the same
reversible float16 safety scale as wgpu. HDR file decoding remains application
owned: the C++ demo uses its existing stb decoder, not a new renderer dependency.
Replacement conservatively invalidates Once bakes, including same-size uploads.

## Alternatives considered

Manual six-face document wiring duplicates a recipe already shared by backends.
Background masking by colour or alpha confuses dark surfaces and transparency
with empty geometry; opaque depth is the explicit contract.

## Consequences

Old presets and analytic defaults do not change. HDR imports and background
composition are exercised on both backends. This closes S3's sky/GGX environment
workflow, not multiple scattering, a solar disc, Charlie or spectral convolution,
or temporal/mirror antialiasing. Transparent layers must composite after the
opaque background; depth >= 1 denotes the existing standard-depth clear.
