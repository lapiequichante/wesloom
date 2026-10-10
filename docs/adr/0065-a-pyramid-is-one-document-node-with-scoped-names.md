# 0065. A multi-pass recipe is one document node with scoped names

Date: 2026-10-10

Status: Accepted

## Context

plan3 queued the "sub-document synthesis" twice (plan2-architecture, ADR 0034):
a real bloom pyramid wants internal transients and a chain of passes whose
count is a parameter, and a pipeline document is a fixed set of nodes. The
scheduler already orders and aliases transients of any shape; only the
*name allocation* was missing. S3's `pass.environment` (ADR 0064) then set
the precedent: one document node whose compiler expansion declares frame
resources and emits passes. S4's done-when forces the second consumer —
bloom's chain must be a real pyramid authored as documents.

## Decision

A pipeline document node may expand to scoped resources and passes. The
expansion is Rust logic in the document compiler, shared by both backends
through the compiled pass list — no new serialization, no graph-level loop
construct, no second registry. The generated labels are derived from the
node's own label and are part of the contract: `pass.bloom` labelled *bloom*
generates `<label> extract`, `<label> down k`, `<label> up k`, `<label>
combine`, plus `<label> level k` and `<label> up k` resources the document's
namespace never sees — no document socket names an intermediate, and the
expansion refuses nothing the scheduler would not.

The node's settings are the recipe's knobs: `pass.bloom`'s `levels`,
`threshold`, `knee` and `strength` seed the generated passes' effect
parameters ([ADR 0042](0042-effect-parameters-are-uniforms-the-descriptor-declares-them.md)),
so the host re-tunes a generated pass live through
`set_pass_param` on its stable label. The node's `color` output resolves to
its final pass's write target — a wired `resource.color`, or the frame's
own target, subject to the same "nobody samples the target" rule as a
`pass.screen`.

`pass.bloom` is the second instance of the pattern (after
`pass.environment`); it owns the four shipped effects
`wxsl.bloom_extract`/`bloom_down`/`bloom_up`/`bloom_combine`, which a
document may also wire by hand. A pyramid level is half its parent —
`Extent`'s viewport rule already rounds up and clamps to a texel — and the
glow resources are HDR transients, because the threshold reads linear
radiance and an 8-bit intermediate would clamp the highlight. Level counts
are capped (12) and a bad count is a named `PipelineError`.

## Alternatives considered

- Hand-authored level chains: possible (the four effects accept it) and
  the right *test* of the shape, but a document that says "levels: 5" in
  forty nodes is the repetition the node exists to retire.
- A graph-level loop or sub-document language: a second serialization to
  version, validate and expose in two canvases, for what scoped name
  allocation already buys.
- One multi-mip resource for the pyramid: ADR 0061 ties mips to fixed
  extents; a bloom pyramid tracks the frame. Separate per-level
  transients alias in the scheduler exactly as well.

## Consequences

The stock presets and the separable bloom pair do not change; the
pyramid is a document node the gallery demonstrates. Editor palettes grow
`pass.bloom` through `document_registry` with no editor work. A future
multi-pass recipe (DOF is the likely next) extends the vocabulary the same
way — a node, an expansion, stable generated labels — rather than a new
mechanism. The expansion runs per compile, not per frame; its resources
participate in transient aliasing like any others.
