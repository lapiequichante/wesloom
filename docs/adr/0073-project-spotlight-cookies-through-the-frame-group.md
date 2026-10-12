# 73. Project spotlight cookies through the frame group

Date: 2026-10-11

Status: Accepted

## Context

N7 needs focused lights whose cone can be masked by a texture. Forward and
deferred shading must apply the same cone, cookie projection and visibility.

## Decision

A spot stores position, direction, inner/outer half-angles and an optional
cookie layer. Its smooth cone has inverse-square falloff and one perspective
shadow tile matching the outer angle. Cookie UVs use the same light basis and
projection whether or not the spot casts a shadow.

The application supplies a filterable RGBA 2D-array texture view through
Renderer::set_light_cookies. White is open, black masked, RGB tints the light
and alpha attenuates it. A disabled cookie is a sentinel layer; unavailable
layers mask the light. Sampling explicitly uses level zero. The renderer reuses
the frame group's clamp sampler and binds a placeholder until the application supplies a view.
Both backends publish these frame bindings. Uploading or replacing a view does
not recompile shaders. The gallery uploads a procedural window-mask layer.

## Alternatives considered

Per-light bind groups: break the shared light loop and deferred lighting pass.
Reuse the shadow atlas for masks: conflates depth visibility and light emission.

## Consequences

One texture binding and spotlight fields extend the generated host contract;
regenerate headers and rebake exports. The view is host-owned and applications
must keep it outside resources being written by a pass using the frame group.
Cookies mask emitted light; they do not alter a material's cast-shadow flags.
