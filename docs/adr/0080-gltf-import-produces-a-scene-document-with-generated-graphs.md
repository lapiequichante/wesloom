# 80. glTF import produces a scene document with generated graphs, in a device-free crate

Date: 2026-10-14

Status: Accepted

## Context

ADR 0021 imported glTF *geometry only*. Its argument was that a glTF
material is a parameter set and this project's materials are graphs, and
that a fixed graph with the numbers poked in "would be a lie". Plan6 G2
reverses that: a file has to open with its materials, and the editor (E1),
`SceneResources`, the MCP server and C++ hosts all need the same result.
The scene document v2 (ADR 0079) can now say what a file contains. The
geometry reader lived in `wxsl-render`, which has wgpu, so a C++ host
could not reuse it through the data-only C ABI (ADR 0049).

## Decision

- **A new crate, `wxsl-gltf`**, depends on `wxsl-core`, `wxsl-frame`
  (for the G-buffer budget), `gltf`, `glam` and `serde_json`: no wgpu, no
  `wxsl-stdlib` and no unsafe code. The
  caller passes the node registry the generated graphs are built from.
  - `import(path, registry)` and `import_slice(bytes, registry)` return an
    `ImportedScene`: a `Scene` document, one `Geometry` per triangle
    primitive (vertices in the 48-byte host `Vertex` layout), the embedded
    images still encoded, the file's directory, and `warnings`.
  - `geometry::load_baked` is the old flat reader. `wxsl-render`'s
    `gltf` feature now wraps it and converts to `MeshData`.
- **Structure is kept.** The default scene's nodes become document
  `nodes`, and each placement is an instance under its node. The scene
  gains a `MeshSource::Embedded { index }` source (additive within schema
  2) for geometry supplied with the document, mirroring `embedded`
  textures.
- **Each material becomes a generated `gltf_pbr` graph** of stdlib nodes:
  - texture samples, with `KHR_texture_transform` as two dot products and
    an offset, and `TEXCOORD_1` as the declared attribute `uv1`;
  - the factors as `param.value` nodes, editable without a recompile;
  - the normal map through `space.apply_normal_map`, and occlusion mixed
    toward 1;
  - emissive, with `emissive_strength` folded into its factor;
  - `MASK` as `output.discard`, and `BLEND` as alpha plus the
    `transparent` tag.

  Only the branches a material uses are generated. Slot names are
  `<name>_texture`, each with a `<name>_texture_sampler` bound to the same
  scene texture, which carries the glTF sampler. The graph is visible in
  the editor, so it is a starting point, not a lie.
- **The extensions map to what exists:**
  - transmission and volume → the `refractive` tag, `ior` and
    `attenuation_distance` (ADR 0067), with the attenuation colour folded
    into the base colour;
  - `KHR_materials_ior`;
  - sheen → `wxsl.sheen` (ADR 0059);
  - iridescence → `wxsl.iridescent` (ADR 0058);
  - clearcoat → `wxsl.gltf_clearcoat`, with its factors, textures and
    coat normal (ADR 0091; until then, `wxsl.clearcoat`, whose coat
    followed the model's own convention);
  - `KHR_lights_punctual` → the `environment` block, intensities 1:1;
  - perspective cameras.

  A material has one model, so the first of iridescence, clearcoat and
  sheen wins. When the default model and the one layered model a file
  uses do not fit the G-buffer budget together (sheen and iridescence fill
  it alone), the plain materials take the layered model with its layer
  off, and each is warned about.
- **Nothing is dropped quietly.** These all produce an `ImportWarning`
  that names the cause:
  - an extension used but not mapped;
  - an extension texture we ignore;
  - a factor we approximate;
  - a KTX2 or WebP image;
  - an orthographic camera;
  - a non-triangle primitive;
  - a second scene.

  A double-sided material was warned about too, until
  [ADR 0090](0090-a-double-sided-material-turns-off-its-passes-culling.md)
  made the stock passes draw it: it now sets `double_sided` and keeps the
  `double_sided` tag.
- **Images** given by a file URI stay `file` textures relative to the
  glTF file. Buffer-view and data-URI images are supplied encoded and
  decoded on upload (the `images` feature, now PNG and JPEG).
- **Three consumers, one import:**
  - `wxsl::asset::import_gltf` runs it against the stdlib registry, and
    `SceneResources::load_imported` uploads it;
  - the C ABI adds `wxsl_import_gltf` (a `{"path"}` request) and
    `wxsl_import_glb` (bytes). The result's JSON carries `scene`,
    `lighting_models` and `warnings`, and new accessors expose the meshes
    (`WxslMesh`: vertices, indices, colours, `uv1`) and the images.

## Alternatives considered

- **Keep the importer in the facade.** Simpler, but C++ could only
  reach it through a second, wgpu-linked library.
- **One fixed graph with every texture slot**, bound to white or flat
  defaults when unused. This gives one shader variant for every material,
  but it binds and samples textures that do not exist, and it makes the
  editor show a graph unrelated to the file.
- **Depend on `wxsl-stdlib` from `wxsl-gltf`.** That would forbid
  `wxsl-render` from depending on it (ADR 0002), so the geometry reader
  would have to be duplicated.

## Consequences

- `wxsl-render` gains an optional in-workspace dependency on
  `wxsl-gltf`, and `wxsl-ffi` a required one. Neither brings wgpu or
  stdlib where they were not.
- ADR 0021's "geometry only" stance is amended.
- Not done:
  - extension textures (sheen, iridescence, transmission, thickness;
    clearcoat's are mapped since ADR 0091);
  - `KHR_materials_specular` and `KHR_materials_variants`;
  - KTX2;
  - skinning, morph targets and animation (plan6 decision 5).
- `examples/viewer.rs` opens a file under an HDR environment and routes
  `refractive` materials through the refraction tier. The Khronos samples
  it names are in `resources/glb/`, with their licenses.
