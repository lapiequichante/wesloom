# 88. One model channel shared by every model, the id in the normal target

Date: 2026-10-16

Status: Accepted

Amends [ADR 0028](0028-lighting-models-dispatched-by-a-g-buffer-id.md) (the
id channel and the per-model targets),
[ADR 0037](0037-semantic-channels.md) (a feature's channel is no longer a
target of its own), and the budget notes of
[ADR 0058](0058-iridescence-is-authored-on-the-surface-and-carried-by-a-model-channel.md)
and [ADR 0059](0059-sheen-is-a-surface-layer-with-model-owned-ambient.md).

## Context

ADR 0028 dispatches the deferred lighting pass on a model id, but gives
every model that needs data a target of its own, and the id a one-byte
target. The base targets cost 24 of WebGPU's 32 bytes per sample, so two
models with data do not fit together: iridescence and sheen were left out
of the default set, and clearcoat had to fit in a pair. A pixel is shaded
by exactly one model, so the data of two models never has to be stored at
the same pixel.

## Decision

* **The id rides in the normal target.** The normal is stored octahedral
  in two channels (`package::wxsl::gbuffer`), roughness in the third, the
  model id in the fourth. Half floats hold integers exactly, so the id
  needs no scale. The `lighting` target is gone.
* **One `model` target, shared.** A plan that carries any request (a
  model's `extra`, a feature's channel) adds one HDR `vec4` target. Each
  pixel's material writes its own model's pack there; a material whose
  model has no data of its own writes the feature its model consumes, or
  else the first feature the plan carries, under the feature's macro. The
  lighting pass hands the texel to the arm of the pixel's model, which
  reads it in its own precision. The registry still names each meaning (`clearcoat`,
  `sheen`, …), for documentation and debug views.
* **A model has one source of data**, its own `extra` or the feature it
  consumes, never both: `LightingSet::new` refuses the pair.
* **The cost is 24 bytes per sample, or 32 with the model target**,
  whatever the set. `default_set` therefore holds every shipped model.
* A debug view of a model's data shows the model target where that model
  shaded the pixel; a feature's view, where the pixel's model has no data
  of its own. It is black elsewhere.

## Alternatives considered

* **Keep the id target, share a two-channel target** (29 bytes). Less
  change, but sheen and iridescence need four channels.
* **Pack the id into `base_color.a` beside metallic.** Eight bits do not
  hold both, and metallic is continuous.
* **Keep a target per model** (ADR 0028). Two models with data overflow
  the budget, which is what this ADR fixes.

## Consequences

* The plan's layout is the base three targets, plus `model` if any
  request. `wxsl-render`, the scheduler's attachment check and the C ABI's
  exported layouts already read it from `GBufferPlan`, so they follow.
* A pixel whose model has data of its own does not store the pipeline's
  feature, so a feature view is black there.
* A model that needs more than four channels is a schema change, as
  ADR 0028 already said.
* Octahedral encoding at 16 bits per channel is finer than the shading
  needs; the forward/deferred image tests keep their tolerances.
