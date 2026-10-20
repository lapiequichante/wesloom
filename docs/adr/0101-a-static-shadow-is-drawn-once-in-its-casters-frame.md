# 101. A static shadow is drawn once, in its casters' frame

Date: 2026-10-20

Status: Accepted

Amends [0026](0026-shadows-a-view-per-light-and-two-flags-on-the-material.md)

## Context

Every shadow tile is cleared and redrawn every frame (ADR 0026): the first
shadow pass clears the whole atlas, and each tile pass draws every caster
its view admits. That is right for a light whose view of the world
changes, and pure waste for one that moves rigidly with everything it
shadows. A car's headlight is the case: what it shadows that matters is
the car itself, which never moves in the headlight's own space. In the
drive demo (ADR 0100) its two tiles cost 2 to 4 ms a frame at 1280×720.

Two things stood in the way of drawing such a tile once:

- **The clear.** One atlas-wide clear in the first pass wipes a kept tile
  along with the rest. A depth clear cannot be confined to a rect, and
  WebGPU copies depth only by whole subresource.
- **What the tile sees.** A redrawn tile shadows everything, the road under
  the car included. A kept one would freeze the road where it was. A
  spot's view also takes its roll from world up, so a car that banks
  rolls inside a kept map.

## Decision

**A light can say that its shadow is static, and which draws cast it.**

- `Light::shadow_casters` is a `TagExpr` over draw tags. It defaults to
  every draw, and a shadow pass for that light draws only the draws it
  matches. `Light` is no longer `Copy`, because a tag expression is not.
- `Light::static_shadow` is an `Option<Mat4>`: the frame its casters move
  in. It is only for spot lights, whose one perspective tile is read
  through its matrix alone. A point light picks its cube face by world
  axis, which a rotating frame would break. The light's position, aim and
  cone are taken in that frame.

**The cache is pure and lives in `wxsl-frame`.**
`wxsl_frame::shadow_cache::ShadowCache` decides each tile, each frame:

- **Redraw** when the slot has no entry, or the light, seen in its frame,
  changed (kind, position, aim, cone, range, casters).
- **Keep** otherwise. A kept tile's lookup matrix is the one it was drawn
  with, carried by the frame's motion since: `V_drawn · F_drawn · F_now⁻¹`.
  That stays exact when the car rolls under a world-up view.

The cache keeps nothing across `Renderer::set_graph`, which reallocates
the atlas. `Renderer::invalidate_static_shadows` drops it for an
application whose casters changed shape.

**On a frame that keeps a tile, the wgpu renderer clears by drawing.**
The atlas-wide clear becomes a load, and each tile pass that draws first
writes the far plane over its own rect. That is one depth-only fullscreen
triangle, clipped by the viewport and scissor the pass already sets. A
frame that keeps nothing records exactly what it did before.

**The pass list does not change.** The stock pipelines, the presets and
the exported plans keep one clear followed by one pass per slot. A static
shadow is a renderer optimisation over that list: a backend that ignores
it, Dawn included, redraws the tile every frame and gets the same picture.

## Alternatives considered

- **A second atlas for static tiles.** It never needs clearing. But it
  adds a frame-group binding, a second lookup path in `shadow.wxsl`, and
  the matching C ABI and Dawn work, all to avoid one fullscreen triangle.
- **Per-tile clears in the pass list.** The plan would then say what runs,
  but every backend would need a depth-writing screen pass, and the frame
  graph has no such kind.
- **An application-supplied version key instead of a frame.** It cannot
  correct the roll, and it has to be bumped by hand whenever the light
  moves on the car.

## Consequences

- A static light pays for its tile once. A frame that keeps one pays a
  depth-only triangle for each tile it does redraw.
- `Light` is `Clone` only. A caller that copied lights clones them now.
- A static shadow shows only its casters, and only casters that move with
  the light. The drive demo tried it on its headlights, then had to draw
  the deck into their shadows to keep the beams off the piers below, and
  the deck streams past the car. Its headlights stay static anyway, over
  the car and the deck: the kept deck is the stretch under the car when
  the tile was drawn, close enough on a gently curving viaduct to keep
  the beams off the piers, though a sharp bend or crest would show it.
- A caster that changes shape on the move (a door opening on the car)
  needs `Renderer::invalidate_static_shadows`. The cache cannot see a
  mesh change.
- Point and area lights could follow later, once a static cube is drawn
  and read in its frame's axes.
