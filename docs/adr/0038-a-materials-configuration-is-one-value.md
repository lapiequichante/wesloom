# 0038. A material's configuration is one value, resolved once

Date: 2026-09-14

Status: Accepted

## Context

Everything an author sets on a material that is not a node had three
spellings and no single owner:

* `wxsl_core::scene::MaterialEntry` carried `macros`, `tags`,
  `cast_shadow`, `receive_shadow` and `lighting` as five fields beside the
  graph;
* `wxsl_render::material::MaterialOptions` carried four of the same five
  (no tags) plus `features`, the pipeline's channel plan (ADR 0037);
* `wxsl_core::codegen::CodegenOptions` carried two more — `override_macros`
  and `lighting` — that were the *results* of the above.

The facade's scene loader existed partly to copy field by field between the
first two, and the resolution steps that turn one into the other happened in
two places: `MaterialOptions::effective_macros` pinned
`abi::FEATURE_RECEIVE_SHADOWS`, and `Material::with_lighting` resolved a
model name against the enabled set. A third check — a material pinning a
feature's macro under a pipeline that carries no channel for it — lived at
the far end, in `Renderer::compile_frame`, a whole frame away from the place
that had the facts.

The pattern was the one ADR 0030 already named for pipelines: knobs that are
facts about *one* thing carried as independent parameters, so each new one
costs a change at every site it passes through. ADR 0037's features were the
fourth and fifth knob to pay that bill. plan3's P7 called the consolidation
overdue rather than speculative, and it is: this is ADR 0030's argument,
owed to materials.

## Decision

`wxsl_core::material::MaterialConfig` is the one value:

```rust
pub struct MaterialConfig {
    pub macros: MacroSet,              // above the graph's own pins
    pub model: Option<String>,         // a name, resolved against the set
    pub cast_shadow: bool,             // a selection
    pub receive_shadow: bool,          // a macro, spelled as a flag
    pub tags: Tags,                    // what this material *is*
    pub features: Vec<ChannelRequest>, // the pipeline's plan, handed down
}
```

It travels document → options → codegen without being respelled:

* `MaterialEntry` holds one, `#[serde(flatten)]`ed, so the *document* is
  unchanged — the fields sit where they always did. The one rename,
  `lighting` to `model`, carries a serde alias, so a document written before
  this ADR still loads.
* `Material::with_config` and `Material::with_lighting` take one, replacing
  `MaterialOptions` (deleted).
* `CodegenOptions.material` is a `ResolvedMaterialConfig`, replacing its
  `override_macros` and `lighting` fields.

`MaterialConfig::resolve(&LightingSet) -> ResolvedMaterialConfig` is the one
resolution point: a model name becomes an id in the set that enables it, and
the receive-shadows flag becomes its macro — the flag beating anything the
graph or the caller pinned under the same name, because that is the field's
whole meaning. `ResolvedMaterialConfig` holds facts rather than requests,
which is what lets the renderer compare two of them by value.

The feature handshake moves to the same place, in two halves, because the
demand has two sources. A config that pins a feature's macro is visible at
`resolve`; a *graph* that pins it is only visible once codegen has overlaid
the graph's own values, so `ResolvedMaterialConfig::check_feature_demands`
takes the generated macro set and `Material::with_lighting` calls it right
after generating. Either way the error is named where the material is
compiled, not at the first frame.

A material carries its tags now, and `DrawItem::new` uses them —
`draw.rs` already documented a draw as carrying "the `Tags` the material was
authored with" while making every caller pass them by hand.

## Alternatives considered

* **Keep `MaterialOptions` and add `MaterialConfig` beside it, converting at
  the boundary.** A fourth spelling to keep in sync, for the sake of not
  touching call sites. Rejected: the whole cost being paid is the syncing.
* **Put the pipeline's `features` outside the config**, passing it beside
  the `LightingSet` the way `with_lighting` already passes the set.
  Rejected: it would leave half the pipeline's plan inside the config value
  and half beside it, which is the inconsistency this ADR is about. The
  field is `#[serde(skip)]` instead, because a *document* cannot honestly
  author its pipeline's channels.
* **Leave the feature handshake in `compile_frame`.** Rejected: resolution
  is the earliest point the facts exist, and a material resolved against a
  plan that answers its demand cannot reach a frame carrying a different
  plan without failing the by-value comparison that is still there.
* **Move `Tags` out of `scene` into the new module.** Rejected as churn:
  tags are a scene-document vocabulary that materials use, and
  `TagExpr` lives with them.

## Consequences

* The next per-material knob is a field on `MaterialConfig` plus whatever
  `resolve` has to do with it — one site, not four.
* A material demanding a feature channel its plan does not carry is now a
  *compile*-time error rather than a first-frame one. `compile_frame` keeps
  the two plan comparisons (lighting set, feature channels) and loses the
  macro-pin loop; ADR 0037's description of where that check lives is
  amended by this note.
* `DrawItem::new` is no longer untagged: it takes the material's tags. A
  material configured with no tags gives an untagged draw, which is what
  every hand-built draw was, so nothing that exists changes behaviour.
* `SceneResources::load_with_plan` joins `load_with_lighting`: the feature
  half of the plan was previously hardcoded empty for scenes, so a scene
  could not be loaded under a pipeline with channels at all.
* P8 (plan3) wants published capability metadata with one
  `RenderSetup::check`. `ResolvedMaterialConfig` is the material side of
  what that check reads; `PipelineConfig` (ADR 0030) is the pipeline side.
* If the shape of `MaterialConfig` changes, also update the document
  description in `docs/architecture.md` and the conventions list in
  `AGENTS.md`.
