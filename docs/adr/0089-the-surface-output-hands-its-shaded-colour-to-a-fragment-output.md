# 89. The surface output hands its shaded colour to a fragment output

Date: 2026-10-16

Status: Accepted

Extends [ADR 0025](0025-a-material-graph-spans-shader-stages.md) (a new
terminal) and [ADR 0056](0056-a-material-may-shade-forward-under-either-pipeline.md)
(when a material shades forward). Keeps
[ADR 0039](0039-tonemap-is-an-effect-and-ambient-reads-the-lut.md).

## Context

`output.surface` ends a material: the pipeline lights the surface, and the
graph cannot touch the result. Fog, a grade or a stylized curve that should
apply to one material's lit colour has nowhere to go.

## Decision

* **`output.surface` has an output, `color`**: the surface lit by the
  pipeline's lights, as the shading function returns it (linear radiance
  times the scene's exposure, and alpha).
* **`output.fragment` is a new terminal** with one input, `color`
  (`vec4f`). When that input is connected, the colour stages return it
  instead of the shaded colour: `forward_lit`, the peel colour stages and
  the peel resolve. Nodes between the two run in the fragment stage, in a
  function of their own (`wxsl_fragment`), where `color` reads the shaded
  value.
* **The colour stays linear.** The frame's `tonemap` effect still applies
  afterwards (ADR 0039). A tonemap node in the chain is a stylization on
  top; a pipeline whose `tonemap` has `curve` at 0 leaves it alone.
* **Under deferred, such a material shades forward automatically**: its
  G-buffer stage shades, runs the chain, and stores the result for the
  preshaded route (ADR 0056), with shadows compiled out of that stage. A
  lighting set without `wxsl.preshaded` cannot route it, and codegen says
  so by name.

## Alternatives considered

* **A separate `shade` node** taking the surface: it would duplicate
  every surface socket, or need a struct value the graph model does not
  have.
* **Display-referred fragment output** that the frame tonemap skips: it
  needs a per-pixel mask in the tonemap pass, and bloom and TAA would see
  compressed values.

## Consequences

* `GraphOutputs` gains `fragment`, and `NodeBody` gains `FragmentOutput`.
* A node fed by both the surface and the chain is emitted in both
  functions, as with every partition (ADR 0025).
* Under deferred, a material with a chain receives no shadows, as for any
  forward-shaded material (ADR 0056), and pays its shading in the geometry
  pass.
* The depth, shadow, velocity and refraction stages ignore the chain;
  they write no colour.
