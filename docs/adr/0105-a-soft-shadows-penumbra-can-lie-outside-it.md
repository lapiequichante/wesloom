# 105. A soft shadow's penumbra can lie outside it, each kind of blocker at its own width

Date: 2026-10-24

Status: Accepted

Amends [0098](0098-contact-hardening-shadows-search-blockers-through-the-comparison-sampler.md)
and [0050](0050-host-shared-layouts-are-generated.md).

## Context

ADR 0098 filters a soft shadow with a filter centred on the hard edge,
as wide as the average penumbra of the blockers its search finds. The
penumbra straddles the edge, and half of it lights the shadow's inside.

In the drive demo, a tower's shadow on the road under a low sun lies
beside the barrier's. The tower's penumbra lights the inside of its own
shadow, so the tower's shadow is paler there than the barrier's shadow
next to it, and the two meet on a line. Outside the tower's shadow, the
search finds both blockers, and their average blurs the barrier's
contact shadow away.

## Decision

- A light chooses which side of the hard edge its penumbra lies on:
  `Light::with_penumbra(Penumbra::Inside | Penumbra::Outside)`.
  - `Inside`, the default, is ADR 0098 unchanged.
  - The value rides in the light row: the first float of `_padding3` is
    now `penumbra` (0 inside, 1 outside). The C header is regenerated.
- `Outside` stretches the centred filter's reading `v` to
  `saturate(2v - 1)`. The filter reads half the light at the edge and
  all of it a penumbra out, so the shadow stays whole and its edge
  softens outward. The stretch halves the penumbra, so the filter is
  twice as wide (`WXSL_SHADOW_OUTSIDE_SCALE`): the penumbra is as wide
  as a centred one, from the edge outward.
- Under `Outside`, the search keeps each tap's penumbra. The blockers are
  two kinds when the widest penumbra is more than `WXSL_SHADOW_MIXED`
  (4) times the narrowest (contact counting as one texel).
  - The two kinds are split at the geometric mean of the two penumbrae,
    whose blocker depth comes from the same similar triangles as the
    bisection (`blocker_depth`).
  - The far blockers are filtered at their own average penumbra, by
    comparing at the split depth. The near ones are filtered at theirs,
    as what is shadowed at the receiver but not at the split depth.
  - Each filter is stretched outward, and the darker of the two wins.
- The drive demo puts every soft shadow outside.

## Alternatives considered

- **Splitting the blockers for a centred penumbra too, multiplying the
  visibilities.** The far caster's inner penumbra still lit its own
  shadow next to a near one's: the line got sharper.
- **The darker of the soft shadow and the hard one.** It keeps every
  edge hard on the inside.
- **The narrowest blocker's penumbra for the whole filter.** It sharpens
  the far caster's penumbra wherever a contact shadow is near.

## Consequences

- An outside shadow is as large as its hard shadow plus its penumbra,
  which is the centred one's full width.
  It reads as a slightly larger shadow with a soft rim, not as one the
  source's size would cast.
- Mixed pixels pay 72 comparisons instead of 24. The others are
  unchanged.
- A far blocker hides whatever is behind it in the map, so a near
  blocker under a far one counts as lit for the near filter.
- The device tests are the outside half of
  `a_source_radius_softens_the_edge_and_leaves_open_ground_lit`, and
  `a_contact_shadow_stays_narrow_inside_a_far_casters_penumbra`, which
  fails with the split disabled. Both are in `crates/wxsl/tests/shadows.rs`.
