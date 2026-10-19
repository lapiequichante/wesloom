# 96. Area lights are integrated with LTC by the models that can

Date: 2026-10-19

Status: Accepted

Amends [0028](0028-lighting-models-dispatched-by-a-g-buffer-id.md),
[0039](0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md) and
[0095](0095-unshadowed-area-lights-sample-where-the-lobe-and-the-falloff-are.md).

## Context

ADR 0095 samples unshadowed rectangles and tubes: 32 and 16 full BRDF
evaluations per pixel. The neon demo took 48 ms a frame at 1280×720 on
an M5, against about 4 ms for a point-lit scene. Babylon.js and three.js
use linearly transformed cosines instead (Heitz, Dupuy, Hill and
Neubelt, 2016): a 3×3 transform, looked up by (N·V, roughness), turns
the GGX lobe into a clamped cosine. A polygon's integral under a clamped
cosine is closed-form, a sum over its edges. A texture on the emitter is
read once, at the projection of the shading point, at a level set by
the distance. LTC gives the diffuse and specular terms separately, and
our lighting models only answer one `LightSample` at a time.

## Decision

- A `LightingModel` may name an `area` function:
  `fn(surface, ctx, area: AreaLight, extra) -> vec3f`. The light loop
  hands it any unshadowed rectangle or tube whole. A model without one
  keeps ADR 0095's samples, so cloth, sheen and the rest stay correct,
  just slower. `wxsl.pbr` and `wxsl.lambert` implement it.
- `AreaLight` is four corners from the shading point, plus the radiance
  and the cookie layer. A tube becomes the ribbon of its diameter that
  faces the point. The models build on two library functions:
  `area_irradiance` (∫ L cos) and `area_specular`. The second returns
  the light's integral under GGX's LTC; the caller weights it with the
  split-sum `F0·x + y` from `environment_brdf`, which holds the same two
  Fresnel moments as LTC's second table. Horizon clipping uses the
  sphere approximation.
- The LTC table is fitted in-house: `cargo run -p wxsl-frame --release
  --example fit_ltc` writes `shaders/ltc_table.wxsl`, a 64×64 constant
  array. It is a reimplementation of the published method, checked
  against a published table to three digits. The environment LUT grows
  to 128², in four 64² quadrants:
  - top left: (scale, bias, Minv₀₀, Minv₀₂);
  - top right: (Minv₂₀, Minv₂₂);
  - bottom half: empty.

  The existing bake writes all of it, on both arms. The table rides in
  the bake's shader, so wgpu and Dawn receive it with no host change.
  `environment_brdf` clamps into its quadrant.
- The emission texture is the cookie layer, mipmapped by the host. It is
  read at the shading point's projection onto the transformed polygon,
  at `log2(distance / √area · size)`.

## Alternatives considered

- **Keep sampling, with fewer samples.** At most about 2× faster, and it
  dots again on glossy surfaces.
- **LTC for every model.** It needs each model's lobe fitted;
  non-GGX lobes have no table.
- **A separate LTC texture binding.** It would change the frame group,
  the host header and the Dawn host. The quadrants reuse what exists.
- **The published table.** Its provenance would need an audit under
  ADR 0053. Fitting our own takes 20 s.

## Consequences

- A rectangle or tube costs about two BRDF evaluations and three texture
  reads with an LTC model. The neon demo dropped from 48 to 9 ms and
  area-texture from 14 to 6 ms.
- LTC is approximate: the fit's relative L1 error is a few percent near
  normal incidence and grows towards grazing, as in the reference
  tables. A texture is one prefiltered read rather than an integral, so
  very close to a sharp emitter it blurs differently.
- The table is f16 in the LUT. Its smallest entries, a mirror at
  grazing angles, underflow, as in Babylon's half-float table.
- The bake shader carries a 4096-entry constant array. naga emits it as
  a program-scope constant on Metal and as a `const` array in GLSL ES.
- Shadowed rectangles still use ADR 0072's quadrature.
- A model added later that wants fast area lights implements `area`;
  `LightingModel` literals must name the field.
