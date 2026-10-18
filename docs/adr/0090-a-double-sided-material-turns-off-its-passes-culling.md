# 90. A double-sided material turns off its passes' culling

Date: 2026-10-18

Status: Accepted

Refines the culling convention of `docs/history/plan.md` (culling is a
pass knob). Extends [ADR 0038](0038-a-materials-configuration-is-one-value.md)
(a new field) and [ADR 0080](0080-gltf-import-produces-a-scene-document-with-generated-graphs.md)
(the importer no longer warns about `doubleSided`).

## Context

Culling is a field of a pass's `PassState`, and the stock opaque passes
cull back faces. A leaf, a cloth or a glTF material marked `doubleSided`
must be visible from both sides, and its back faces must be lit with the
normal facing the viewer. The importer could only tag such materials and
warn that the stock passes ignore the tag. Writing a second set of passes
for these materials would double every pipeline document.

## Decision

* **`MaterialConfig::double_sided`**, off by default, is a document field
  beside `cast_shadow`. The glTF importer sets it from `doubleSided` and
  keeps the `double_sided` tag.
* **The material decides about faces, the pass about everything else.**
  `PassState::sides(double_sided)`, in `wxsl-frame`, gives the states a
  draw uses, in order:
  - a single-sided material, or a pass that culls nothing: the pass's
    state, once;
  - a double-sided material in an opaque pass: culling off, once;
  - a double-sided material in a blended pass: back faces (cull front),
    then front faces (cull back). A closed transparent surface then
    composites its far side under its near side.
* **The fragment flips the frame.** Codegen gives a double-sided
  material's fragment entry `@builtin(front_facing)` and negates the
  context's normal, tangent and bitangent on a back face, so lighting and
  normal maps see the side the viewer looks at. The refraction-surface
  stage keeps the geometric normal, because it reads the normal's sign
  to tell entry from exit.
* **One rule, in Rust.** The wgpu renderer calls `sides`. The Dawn bundle
  exports, per pass, the cull modes of each draw, and the C++ renderer
  draws once per entry.

## Alternatives considered

* **Tag-selected passes with no culling.** Every preset would need a twin
  of each geometry pass, and the back-face normal would still be wrong.
* **Always draw twice.** Costs a second draw for opaque geometry, where
  turning culling off is free.
* **A per-material cull mode.** More general, but nothing asks for culling
  front faces only, and a flag is what glTF carries.

## Consequences

* A double-sided material is a different variant (the fragment differs)
  and a different pipeline per side (the state differs).
* In a blended pass a double-sided draw issues two calls, and the frame's
  call count says so.
* Passes that already cull nothing (shadows, peeling, refraction) are
  unchanged.
