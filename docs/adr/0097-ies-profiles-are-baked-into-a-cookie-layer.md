# 97. IES profiles are baked into a cookie layer

Date: 2026-10-19

Status: Accepted

## Context

Real luminaires, such as headlights, street lamps and downlights, do not
radiate as a point or a smooth cone. Their makers publish the candela
over directions as an IESNA LM-63 file. We want point and spot lights to
use those profiles, on wgpu and on Dawn, without a new binding, and
without the C++ host parsing anything.

## Decision

- `wxsl_frame::ies::IesProfile` reads LM-63 files. Only type C
  photometry is read, with the symmetries its horizontal angles imply.
  `texels(w, h)` bakes the profile normalized to its brightest
  direction, with u = C / 360° and v = γ / 180°. The host uploads those
  texels into a layer of the light cookie array that spots and area lights
  already sample.
- `Light::with_profile(layer)` works on a point or a spot light. The light
  row's second padding word becomes `profile_layer`, with `u32::MAX` for
  none. The shader's `light_profile` multiplies the light's intensity by
  the layer's red channel. It reads γ from `spot_direction` (straight down
  for a point light) and C from the light's right towards its up, in the
  same frame the spot cookie uses. A spot's cone still applies.
- The intensity stays the light's own. The profile only shapes it, as in
  engines that ignore the file's absolute candela by default.

## Alternatives considered

- **A dedicated profile texture binding.** It would change the frame
  group, the host header's bindings and the Dawn host, for data the cookie
  array can already hold.
- **Profiles as a 1D table, symmetric about the axis.** That covers
  downlights, but not asymmetric luminaires like headlights or street
  lamps, which are the reason to support IES at all.
- **Types A and B.** They are rare outside automotive and floodlight data.
  Type B is the automotive convention, but such files can be converted.

## Consequences

- A profile costs one more texture read per point or spot light.
- The baking resolution is the cookie array's. At 512², a texel spans
  0.35° of γ and 0.7° of C, which is sharp enough for a headlight's cutoff
  at a few tens of metres.
- The C header's `_padding2` is now `profile_layer`. Hosts that build the
  light row themselves must write `UINT32_MAX` there, as they already do
  for `cookie_layer`.
- The gallery's `headlights` demo is the visual check. It uses
  `resources/ies/headlight.ies`, a synthetic low beam.
