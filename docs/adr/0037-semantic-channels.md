# 0037. Semantic channels

Date: 2026-09-13

Status: Accepted

Implements plan2's P12 — including the part the plan deferred ("its first
consumer does not exist yet, so it lands with the first material feature
that needs it"): the consumer is shipped with the refactor, because the
refactor without a second source is a table with one column.

## Context

ADR 0028 built the load-bearing half: per-model targets carry names,
precisions and pack functions; the layout is computed; the budget check
is real. But the only *source* of channel requests was the lighting set —
a channel existed because a model's `extra` asked for it. request.md's
G-buffer section wants channels with semantics and types, **derived from
what materials and lighting actually require**: a material feature (the
plan's example is subsurface) asking for a channel without going through
a lighting model. One source cannot answer "who asked for this field?" —
which is exactly the question a collision raises.

## Decision

* **Channel requests are a collected list with source tags**:
  `ChannelRequest { source, target }`, `ChannelSource::{BaseLayout,
  Model, Feature}`, and `GBufferPlan` — the collected, validated answer
  to "what does the deferred path attach, bind and read" that
  `LightingSet::gbuffer_layout` always was, extended to the second
  source. Model requests ride first in id order (the pack order),
  feature requests follow, and a field claimed twice is
  `LightingError::DuplicateChannel` naming *both* claimants — including
  the base layout, which is a claimant like any other.
* **The feature is data**: `MaterialFeature { name, macro_name, module,
  pack, target, doc }` in `wxsl_core::lighting::FEATURES`, with
  **subsurface** the first entry. The feature owns its channel on the
  models' behalf: its module (`package::wxsl::features::subsurface`,
  shipped beside the ABI modules) declares the `wxsl_subsurface` macro
  knob and `pack_subsurface`, which produces the channel's value.
* **Materials turn it on with a macro pin** — an ordinary pin, part of
  the variant cache key like any macro. The generated pack fills a
  feature channel under `@if(wxsl_subsurface)` (through the feature's
  pack) and zeros it under `@if(!…)`; conditional translation keeps one
  arm, and an unused import dies with it. The channel rides in the
  `ModelExtras` struct the dispatch hands every model, so a
  feature-*aware* model reads `extras.subsurface` from its own module —
  no contract change, the clearcoat precedent generalized.
* **The pipeline's side is config**: `PipelineConfig.features`, set by
  name with `Renderer::set_features(&["subsurface"])` — validated
  against the registry, budget-checked with the same spec-table
  arithmetic, and folded into the lighting pass's variant key, because
  its generated module now names the channel. The document compiler
  declares the widened G-buffer from the same plan; a colliding config
  is `PipelineError::ChannelCollision` at compile, not a device surprise.
* **The handshake is checked by name**: `MaterialLighting` carries the
  plan it was resolved against (`MaterialOptions.features`), and the
  frame compile refuses a material whose plan differs from the
  renderer's. The sharper case — a material that *pins* the feature's
  macro while its pipeline does not carry the channel — is its own named
  error, because that is a demand going silently unanswered.

## Alternatives

* **Extend the model contract** — a `feature` field on `LightingModel`.
  Rejected: it puts the feature *inside* a model, which is the coupling
  P12 exists to break — a feature's channel should outlive any one
  model and serve several.
* **A `Surface` field for subsurface strength** — the physically richer
  shape (a graph node writing per-fragment strength) is a schema change
  to the shipped `Surface` struct, and this ADR records it as future
  work rather than smuggling it in. The first feature's strength is a
  per-material macro *constant*: honest, cache-key-visible, and a real
  round trip through the G-buffer.
* **Defer again, per the plan's own note** — the plan landed the
  refactor "with the first material feature that needs it", and leaving
  the mechanism with no second source and no feature row would have made
  it a refactor in search of a user. Subsurface is the smallest honest
  consumer: its channel exists, packs, round-trips, and is *checked* —
  only its reader is future work.

## Consequences

* The shipped models do not read `extras.subsurface` yet. The first
  feature-consuming model is the first real subsurface model's work; the
  seam it builds on — channel, packing, unpacking, budget, validation —
  is what this ADR ships. Until then the demo and tests prove the
  plumbing, not a new look.
* `gbuffer_layout` on the set is now `plan(&[])`; codegen, the compiler,
  the scheduler's attachment check and the budget arithmetic all read
  the plan, so a second source could not fork the layout if it tried.
* The budget arithmetic is unchanged but now applies to features: base
  (24) + the subsurface pair (4) = 28 of the 32-byte floor, checked at
  `set_features`.
* The corpus gate grew the feature half: the feature-enabled lighting
  pass compiles under both arms of the feature's macro, and the feature
  module compiles standalone like a model module.
* The handshake check means enabling a feature is a *whole-pipeline*
  decision — materials resolved against a different plan are named
  errors, which is the P8 capability check arriving early for the one
  case that already had the facts computed.
