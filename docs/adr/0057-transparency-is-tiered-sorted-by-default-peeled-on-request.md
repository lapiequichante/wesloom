# 0057. Transparency is tiered: sorted by default, peeled on request

Date: 2026-10-10

Status: Accepted

Extends [0047](0047-dual-depth-peeling-and-the-baseline-native-split-for-blendable-float-targets.md)
(peeling stays exactly what it was for the draws that enter it) and rides
[0055](0055-sorting-is-opt-in-per-pass-and-it-sorts-draws-not-passes.md)'s
sort (the sorted tier is what the sort is for).

## Context

ADR 0047 made dual depth peeling *the* transparency path: every draw
tagged `transparent` went through the peel, whatever it was. A lone glass
sphere paid two passes per layer for a sort one comparison would have
settled, and every transparent in a scene paid the peel's per-pixel cost
whether or not anything interpenetrated. Plan5's proposal: split the tag
into tiers — the painter's algorithm for the many, the peel for the few
that actually overlap — with the tier chosen per object.

## Decision

A material's configuration grows `max_layers`, the number of peel layers
the transparent may consume. The tiers:

* **`max_layers = 0` — the default, the sorted tier.** Plain alpha
  blending under a back-to-front sort; no peel pass reads the draw. A
  document draws the tier with a `pass.geometry` whose `layers` setting
  is `sorted` and whose `blend` is `alpha over`, declared *after* the
  `pass.peel` whose composite it extends.
* **`max_layers ≥ 1` — the peeled tier.** The draw enters `pass.peel`,
  spending one layer per iteration while the budget lasts. A value beyond
  the pipeline's `wxsl_peel_layers` cap is clamped by construction — the
  cap bounds which iterations are generated at all — never an error.

The plumbing is one filter, expressed as data: `PassDesc.layers`, a
`LayerFilter` of `All` (the default — every pass list's behaviour before
this field existed), `Sorted`, or `Peeled { layer }`. The document
compiler reads `layers` and `blend` settings on `pass.geometry`;
`pass.peel`'s expansion stamps each generated iteration's passes with
`Peeled { layer }`; the renderer's frame compile retains only the draws
whose material's `max_layers` the filter admits. A sorted-tier pass
*loads* the frame target rather than clearing it — compositing onto what
earlier passes wrote is the tier's whole point — which is also why
several passes may write the frame target in sequence now, provided only
the first of them clears.

**The mixing rule, stated rather than discovered**: the peel handles
every `max_layers ≥ 1` draw; the sorted tier composites *after* the peel
composite, sorted back to front. That is sound when a sorted-only
transparent does not cover a peeled one on screen — the same assumption
the sorted tier already makes about itself. The renderer does not detect
interpenetration across tiers; that problem is what peeling exists for,
and a draw that wants it asks for layers.

## Alternatives considered

* **Implicit tags** — the renderer deriving two tag buckets from the
  material. Lost to explicitness: the tier is a budget the author sets,
  not a classification the renderer guesses, and the document's
  `layers` setting keeps the pass the place where "who draws what" is
  said.
* **Growing the tag expression language** (`transparent+peeled`) —
  overloads tags, which say what a material *is*, with a budget that says
  how it is *drawn*; and it would not reach the per-iteration filter,
  which is where the per-object cost saving actually lives.
* **No document spelling for blend** — a sorted pass without blending
  writes opaque and the tier is dead on arrival. `blend` is one text
  setting with two values, and the premultiplied/max blends stay where
  they belong: inside `pass.peel`'s expansion, not in the vocabulary.

## Consequences

* **This changes who the peel draws.** A transparent that never asked for
  layers silently leaves every existing peel pipeline — the peeling test
  and the gallery demo now set `max_layers` on their peeled surfaces, and
  a peel document that draws no sorted-tier pass draws no `max_layers = 0`
  transparent at all. That is the feature, but it is a migration: a scene
  that loses an object adds `.with_max_layers(n)` or a sorted pass.
* The two-passes-per-layer budget the peel charges is now paid by the
  objects that asked for it; a scene of non-interpenetrating transparents
  runs the peel passes empty rather than paying them per draw.
* `Load`-vs-clear and target writers: the "one unconnected `into`" rule
  is now "one *clearing* writer, first". A document whose later target
  writer clears is refused by name.
* The tier's honesty lives in the mixing rule, not in the renderer: an
  object the peel composite covers is painted over, whatever its depth.
  The gallery demo positions its plate in the clear; the ADR says what
  happens when it is not.
* *Amended in the same plan's close-out:* a pass whose `blend` is set
  tests depth and does not write it. The first multi-draw sorted tier
  showed why: a blended draw that wrote depth would let each draw's z
  reject every farther draw after it, and the painter's-algorithm tier
  would silently become "nearest wins" — exactly the per-draw order the
  tier exists to honour. The per-object cost is observable where it
  lives, in the renderer's per-pass draw count, not in the run counts:
  the peel passes still run when their filters admit nothing.
* If this changes, also update AGENTS.md's peel-stage paragraph (which
  named `pass.peel` as drawing "the transparent tag" plain) and the
  peeling test's doc comment.
