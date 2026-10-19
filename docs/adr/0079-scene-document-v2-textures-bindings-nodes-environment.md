# 79. Scene document v2: textures, bindings, a node hierarchy and an optional environment

Date: 2026-10-14

Status: Accepted

## Context

The v1 scene document (ADR 0021) holds meshes, materials and instances. It
cannot name an image, so a material that declares a texture (ADR 0023) is
finished by hand through `SceneResources::bindings_mut`. Nor can it hold a
parameter value, a file's node structure, or a light. glTF import (plan6
G2) and the editor's scene view (E1) both need a document that can say
what a `.glb` contains. The version check also compared every document
type against the node format's schema, so one document type could not
move on its own.

## Decision

The scene schema becomes version 2. Every new field has a default, so a v1
file reads as v2 unchanged.

- **`textures`.** Each entry is an image plus how it is sampled:
  - the source is a `file` (a path relative to the document) or an
    `embedded` index into images the host supplies with the document (a
    GLB's buffer views);
  - `color_space` is `srgb` (the default) or `linear`;
  - the sampler has filters and wrap modes, defaulting to linear and
    repeat.

  Mips are generated on load, on the CPU, filtered in linear light
  (ADR 0061).
- **Material `bindings`.**
  - `params` maps declared parameter names to values.
  - `textures` maps declared resource names to texture indices. A
    *sampler* slot named there takes that texture's sampler, which is
    glTF's pairing.

  Names are checked against the compiled interface when the material is
  bound, and an error names the material.
- **`nodes`.** Each node has a name, a local transform and an optional
  parent. A parent must come before its child, which rules out cycles
  without a graph walk.
  - An instance, a light or a camera may name a node. An instance's world
    transform is its node's world transform times its own.
  - World transforms are resolved on load. The renderer still takes a draw
    list and never sees the hierarchy.
- **`environment`** (optional): lights and cameras, each placed by a node
  and pointing down the node's −Z, as glTF does.
  - The light kinds are directional, point and spot, each with an
    optional `range` (ADR 0076).
  - The facade translates the block into an `Environment`. `Environment`
    itself stays the renderer's per-frame input, never a document.
- **Per-document schema versions.** `check_document_version` takes the
  schema the document type reads.
- **Decoding images.** Encoded images (PNG) need the facade's new `images`
  feature, which `gltf` implies. Without it, an encoded image is reported
  by texture name, the same way a `file` mesh is reported without `gltf`.

## Alternatives considered

- **Textures as material fields**, with no shared table. glTF shares one
  image between materials, so this would upload it once per material.
- **Baking node transforms into instances on import.** This loses the
  structure the editor's outliner shows (E1), and lights and cameras in
  glTF are placed by nodes anyway.
- **Serializing `Environment` directly.** It holds per-frame state
  (jitter, the previous camera, time, the floating origin) that a document
  must not carry, and it lives in `wxsl-frame`, which `wxsl-core` cannot
  depend on.

## Consequences

- `SceneResources::create_bindings` takes a queue and returns a `Result`:
  it uploads the scene's textures and applies its bindings.
  `SceneResources::set_images` supplies embedded images before that call.
- `SceneError` gains dangling-texture, dangling-node and parent-order
  variants.
- Light intensity is stored in the renderer's units. Converting from
  glTF's photometric units is the importer's job (G2).
- [ADR 0080](0080-gltf-import-produces-a-scene-document-with-generated-graphs.md)
  adds an `embedded` mesh source, for geometry supplied with the
  document, within schema 2.
- [ADR 0094](0094-a-normal-maps-mips-keep-their-spread-and-widen-the-roughness.md)
  adds a `normal` colour space, for normal maps whose mips keep their
  spread, within schema 2.
