# 92. Precomputed radiance transfer is baked offline and applied as environment occlusion

Date: 2026-10-18

Status: Accepted

Extends [ADR 0008](0008-surface-graphs-and-a-named-shader-abi.md) (two
surface fields), uses [ADR 0024](0024-a-material-declares-the-geometry-it-requires.md)
(per-vertex streams, whose budget it raises) and adds a crate to the
table of [ADR 0002](0002-cargo-workspace-crate-boundaries.md).

## Context

Image-based lighting samples the environment cubes along the normal and
the mirror direction, and nothing in between can block them. A car's
wheel wells, its underbody and the inside of its cabin are lit by the
whole sky. Precomputed radiance transfer (PRT, Sloan et al. 2002) fixes
the diffuse half for static geometry. For each vertex, the
cosine-weighted visibility of the environment is projected onto
spherical harmonics (SH) once, offline, and so is the light the
surroundings bounce onto it. At run time the irradiance is then a dot
product with the environment's own SH coefficients, so the lighting can
still change. Low-order SH cannot carry a sharp reflection, so the
specular half needs a coarser visibility summary instead.

Three things had to be decided:

- where the ray tracer lives;
- how colour bounces fit in a vertex;
- how the result reaches every lighting model without each one knowing
  about PRT.

## Decision

* **`wxsl-prt`, a device-free crate**, depends on `wxsl-core`,
  `wxsl-gltf`, `wxsl-stdlib` and `gltf`. Its binary is the offline step:
  `cargo run -p wxsl-prt --release -- model.glb`.
  - It traces rays on the CPU against a BVH of the imported scene, in
    world space.
  - Transparent and refractive materials do not occlude.
  - It projects an equirectangular HDR onto order-3 SH (9 coefficients).
  - It rewrites an imported material graph to use a bake.
* **Bounces are gathered from the previous pass.** Each pass casts the
  same rays from each vertex.
  - The first pass keeps the rays that escape: the shadowed transfer.
  - Each bounce pass follows the blocked rays to the first front face
    they hit. There it takes the previous pass's transfer, interpolated
    across the triangle, times the diffuse albedo at that point.
  - The albedo is glTF's base colour (factor times texture, sampled at
    the hit's UV with its transform and wrap) times `1 - metallic`.
  - `ImportedScene::material_sources` maps each imported material back to
    its glTF material for this.
* **Two modes, from the same rays, in two files.**
  - Grayscale (`<model>.gray.prt`): weighs each bounce by the albedo's
    luminance; 9 coefficients.
  - Colour (`<model>.color.prt`): one set of coefficients per channel;
    27.
  - Both store the bent normal times the ambient occlusion beside the
    coefficients.
* **Packed into float vertex streams.** Two values share a float as
  12-bit codes over ±4, `high * 4096 + low`: an integer that a 32-bit
  float holds exactly and any vertex fetch passes through unchanged.
  - Grayscale needs two `vec4` streams, colour four (`prt0` to `prt3`).
  - `MAX_VERTEX_ATTRIBUTES` rises from 4 to 6, so that colour fits
    beside a glTF material's `uv1`.
  - The decode nodes are pinned to the vertex stage, so only their
    results are interpolated.
* **The surface gains `diffuse_occlusion` (vec3) and
  `specular_occlusion` (f32), both 1 by default.** They scale the
  *environment maps'* diffuse and specular terms in `default_ambient`
  and in every model ambient that samples the maps. PRT is one producer
  of these fields, and a hand-painted map could be another.
* **The diffuse field is a ratio.** It is the transferred SH irradiance
  over the unshadowed SH irradiance at the same normal, per channel,
  clamped to [0, 4].
  - The cube keeps its full resolution, and the normal map its detail.
  - The environment's scale cancels.
  - With PRT off, the image is exactly what it was.
  - Shadowing alone keeps it under 1; bounced light can lift it above.
* **The specular field maps the occlusion through Lagarde and de
  Rousiers' approximation** (Frostbite, 2014), from N·V and roughness. A
  first version intersected a visibility cone with the reflection lobe.
  It cut the reflection of a panel open to 90% of the sky to nothing at
  grazing angles, which turned a metallic car black.
* **Stdlib nodes** compute the fields:
  - `lighting.prt_diffuse_occlusion` (grayscale) and
    `lighting.prt_diffuse_occlusion_color`;
  - `lighting.prt_visibility`;
  - `lighting.prt_specular_occlusion`.

  They share `lighting.prt_unpack` and `lighting.sh_irradiance`.
  `wxsl_prt::apply` wires them in. The environment's coefficients and a
  strength are material parameters, so relighting or turning PRT off
  never recompiles a shader.

## Alternatives considered

* **SH visibility for the specular half (9 more floats).** Order-3 SH
  cannot resolve a reflection either, and it would not fit the budget.
* **Replacing the diffuse lookup with the SH result.** This loses the
  cube's resolution and the normal map, and needs a sentinel default.
* **Using the scalar `occlusion`.** It loses the colour of directional
  shadowing and of bounces, and it scales the specular by the diffuse
  visibility.
* **Integer vertex attributes, or half floats.** The geometry ABI takes
  float formats only, and an integer varying needs flat interpolation.
  Packing codes into floats needs neither change.
* **Colour as a luminance transfer plus a tint.** That is fewer floats,
  but the colour is wrong wherever two differently coloured surfaces
  bounce onto one point.
* **Baking on the GPU.** Faster, but `wgpu` is not allowed in an offline,
  device-free crate, and the CPU bake takes under a minute for the
  half-million-vertex car.

## Consequences

* **Forward only.** The G-buffer has no room for the two fields, so the
  deferred unpack resets them to 1 and a deferred frame draws without PRT.
* **The bake is tied to the import.** The sidecars are keyed by the
  imported mesh order and vertex counts, so they must be re-baked when
  the model changes; a mismatch is a load error. The transfer is in
  world space for the scene's static transforms. A mesh drawn by
  several instances is baked for its first instance.
* **Diffuse bounces only.** A metal has no diffuse albedo, so a mostly
  metallic model (the sample car) gains almost nothing from bounces;
  its interreflections are specular, which this transfer cannot carry.
* **Back faces.** A bounce off the unbaked back of a surface sends
  nothing, and a double-sided material's back faces read the front
  side's transfer.
* **Precision.** Values beyond ±4 are clamped, and a code is
  4/2047 wide.
* **The analytic hemisphere ambient ignores both fields**, which describe
  the environment maps.
* A model that adds an ambient over the maps must apply both fields, as
  `ambient_gltf_clearcoat` does.
* Changing the packing changes `Bake::streams` and the PRT nodes
  together; `wxsl-prt`'s unit tests decode at the positions the nodes
  read.
