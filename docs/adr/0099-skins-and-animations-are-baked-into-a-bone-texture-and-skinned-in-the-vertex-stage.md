# 99. Skins and animations are baked into a bone texture and skinned in the vertex stage

Date: 2026-10-19

Status: Accepted

Amends [0080](0080-gltf-import-produces-a-scene-document-with-generated-graphs.md)
and [0025](0025-a-material-graph-spans-shader-stages.md).

## Context

Plan6 left skinning, morph targets and animation out (decision 5; G4
optional, "its own ADR"). A rigged, animated glTF opened in the viewer
drew its bind-pose vertices under its mesh node, which for a typical
exporter is a 1 cm figure lying under a scaled armature.

A graph's vertex stage could only offset positions (`output.vertex`'s
`position_offset`), so a skinned surface would keep its bind-pose normals.
Sampling a clip and deforming 60k vertices on the CPU every frame works,
but costs a re-upload per frame and per mesh.

## Decision

Skin on the GPU from a texture of baked joint matrices, as Babylon.js's
baked vertex animation does. Four joints per vertex.

- **`output.vertex` gains optional `normal` and `tangent` inputs**, in
  object space, which replace the mesh's frame before the model transform.
  The generated `wxsl_vertex` returns a `VertexDisplacement` (offset,
  normal, tangent) and the vertex entry calls
  `transform_vertex_displaced`. The velocity stage still reads the offset
  only. `VertexContext` gains `object_tangent` (node
  `input.object_tangent`).
- **`animation.skin`** (stdlib) blends four joints' matrices, read with
  `textureLoad` from a bone texture: one row per frame, three texels per
  joint (the rows of an affine 3×4). Its outputs wire into the vertex
  output.
- **The importer keeps skins and animations as data.** `ImportedScene`
  gains `skins`, `animations` and `material_skins`. `JOINTS_0` and
  `WEIGHTS_0` become the float streams `joints` and `weights`. A
  primitive drawn by a skinned node gets its own material per (glTF
  material, skin), whose graph adds the skinning branch reading the
  texture slot `bone_texture` and the parameter `bone_frame`.
- **Joint matrices are relative to the mesh as placed in the file**:
  `inverse(mesh node world) · joint world · inverse bind`, with the mesh
  node's world taken from the import, not the pose. The instance stays
  where the document puts it, and a clip's root motion survives.
- **`BoneTexture::bake`** samples every clip at a fixed rate (row 0 is the
  imported pose). The host uploads it and, per frame, sets `bone_frame`
  only. Sampling is device-free and lives in `wxsl-gltf`.

## Alternatives considered

- **CPU skinning, re-uploading vertices each frame.** No ABI change, but
  per-frame cost grows with vertex count, and every pass would draw a
  mesh the CPU rewrote.
- **A matrix palette updated each frame** (a uniform or storage buffer of
  joint matrices). Less memory than baking every frame, but it needs a
  new per-draw binding the material group does not have. A one-row bone
  texture rewritten every frame is the same idea within this decision.
- **Skinning as a fixed material flag** rather than graph nodes. It would
  need a binding outside the graph's declared resources (ADR 0023), and
  the editor could not show it.

## Consequences

- The bone texture must be filterable to bind in the material group, so
  hosts upload it as `Rgba16Float`. Half precision is about a millimetre
  for a human-sized rig; a larger rig may want a scale folded into the
  rows.
- Frames are not interpolated: a clip plays at the bake rate (the viewer
  uses 60 fps).
- Skinned draws are never culled (their material has a vertex stage, ADR
  0086), and the velocity stage sees no skinning motion: both frames read
  the same `bone_frame`.
- A PRT bake (ADR 0092) does not follow a pose; the viewer ignores bakes
  beside a skinned model.
- Not done: morph targets (warned about), and the bone texture through
  the C ABI and the editor, which bind nothing at `bone_texture` yet. A
  skin placed by two nodes at different places follows the first, with a
  warning.
