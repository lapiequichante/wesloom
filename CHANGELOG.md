# Changelog

Changes to the stable surface: what each crate exports at its root, the
document formats and the C ABI
([ADR 0078](docs/adr/0078-three-api-tiers-pinned-by-public-api-snapshots.md)).
A change to a stable item updates its `api/<crate>.txt` snapshot and adds an
entry here. Module paths and `wxsl-frame` are unstable and change without
an entry.

## [Unreleased]

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
