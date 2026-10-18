# Changelog

Changes to the stable surface: what each crate exports at its root, the
document formats and the C ABI
([ADR 0078](docs/adr/0078-three-api-tiers-pinned-by-public-api-snapshots.md)).
A change to a stable item updates its `api/<crate>.txt` snapshot and adds an
entry here. Module paths and `wxsl-frame` are unstable and change without
an entry.

## [Unreleased]

### Added

- `output.fragment`, and a `color` output on `output.surface`: nodes
  between the two change a material's lit colour, under either pipeline
  ([ADR 0089](docs/adr/0089-the-surface-output-hands-its-shaded-colour-to-a-fragment-output.md)).
- An `exposure` input on the tonemap nodes, and an `exposure` parameter on
  the `tonemap` effect. Both default to 1.
- Every shipped lighting model in `default_set()`, now that they fit.
- `double_sided` on a material (`MaterialConfig`, and the scene
  document's material entries): both faces are drawn, a back face lit
  with its normal flipped. Opaque passes stop culling it; blended passes
  draw its back faces, then its front faces. glTF's `doubleSided` sets it
  and is no longer a warning
  ([ADR 0090](docs/adr/0090-a-double-sided-material-turns-off-its-passes-culling.md)).
- The `wxsl.gltf_clearcoat` lighting model, and `clearcoat`,
  `clearcoat_roughness` and `clearcoat_normal` inputs on `output.surface`:
  a clear coat with glTF's parameters. The glTF importer maps
  `KHR_materials_clearcoat` onto it, factors, textures and coat normal
  included, instead of onto `wxsl.clearcoat`
  ([ADR 0091](docs/adr/0091-the-gltf-clear-coat-is-a-model-of-its-own-authored-on-the-surface.md)).
- `diffuse_occlusion` and `specular_occlusion` inputs on `output.surface`,
  both 1 by default. They scale the environment maps' diffuse and
  specular light on the forward path; deferred draws without them.
- The `wxsl-prt` crate and its `wxsl-prt` binary. They bake precomputed
  radiance transfer for a glTF file on the CPU: shadows, plus diffuse
  bounces coloured by each material's albedo sampled at the hit's UV.
  The bake is written in grayscale and in colour, to `<model>.gray.prt`
  and `<model>.color.prt`. It also rewires imported materials to read
  the bake through the new `lighting.prt_*` and `lighting.sh_irradiance`
  nodes. The `viewer` example loads both files, and `P` cycles off,
  grayscale and colour
  ([ADR 0092](docs/adr/0092-precomputed-radiance-transfer-is-baked-offline-and-applied-as-environment-occlusion.md)).
- `ImportedScene::material_sources`: the glTF material each imported
  material was built from.

### Changed

- A material may declare six per-vertex attributes, up from four.
- The G-buffer stores the normal octahedrally, with the lighting model's
  id beside it, and every model's data in one shared `model` target:
  24 bytes per sample, or 32 with model data, whatever the set
  ([ADR 0088](docs/adr/0088-one-shared-model-channel-and-the-id-in-the-normal-target.md)).
  The `lighting` target and the per-model targets are gone.
- The editor's shortcuts follow the keyboard layout, and its glyphs are
  always generated on the GPU (the toolbar's `msdf` button, the `G` key
  and the example's `--msdf` option are gone).

## [0.1.0] - 2026-10-16

The first release. The workspace's crates share one version.

### Added

- **Materials.** Node graphs compiled to WXSL, then to WGSL, one module
  per material stage, over a library of about 100 nodes (`wxsl-stdlib`).
- **The WXSL compiler** (`wxsl-lang`): WGSL plus imports, conditional
  compilation, macro constants and templates.
- **Pipelines as documents.** Forward and deferred presets, effects as
  nodes (bloom, TAA, depth of field, motion blur, refraction, depth
  peeling), compiled to a pass list the renderer schedules and records.
- **The renderer** (`wxsl-render`): wgpu on native, WebGPU and WebGL2;
  shadows, image-based lighting, clustered lights, instanced draws and
  per-view culling for large scenes.
- **Scenes and glTF.** Scene documents, and GLB import into them with
  generated material graphs (`wxsl-gltf`, the `gltf` feature).
- **The editor** (`wxsl-editor`, the `editor` feature): materials,
  pipelines and scenes, with undoable commands, debug views, a pixel
  probe and a frame panel.
- **An MCP server** (`wxsl-mcp`, the `mcp` feature) that lets an agent
  drive the editor through its commands.
- **The C ABI** (`wxsl-ffi`) and the Dawn backend for C and C++ hosts.

[Unreleased]: https://github.com/lapiequichante/wxsl/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/lapiequichante/wxsl/releases/tag/v0.1.0
