# Using WXSL from Rust

The reference documentation is rustdoc:

```sh
cargo doc -p wxsl --all-features --open
```

Start at the `wxsl` crate page. It has the feature table and a complete
frame in two short examples. Each crate's root page describes its scope and
has a **Stability** section.

## The main types

In the order a program meets them:

| Type | Crate path | Role |
|---|---|---|
| `Graph` | `wxsl::core` | A material, screen effect or pipeline, as nodes and edges. Loads from the JSON node format. |
| `NodeRegistry` | `wxsl::core` | The node vocabulary. `wxsl::stdlib::registry()` is the shipped one. |
| `ShaderLibrary` | `wxsl::render` | The WXSL modules the renderer compiles against. `wxsl::stdlib_library()` is the shipped one. |
| `Material` | `wxsl::render` | A graph compiled for the renderer, plus its `MaterialConfig`. |
| `Renderer` | `wxsl::render` | Owns the pass list, pipelines and variant cache, and renders a frame. |
| `StockPipeline` | `wxsl::render` | The shipped forward and deferred pipelines. Use `Renderer::set_pipeline`, or `request_pipeline` to swap without a hitch. |
| `compile_pipeline` | `wxsl::render` | A pipeline document compiled to a `RenderGraph`, which you install with `Renderer::set_graph`. |
| `Effect` | `wxsl::render` | A declared screen or compute pass. Register more with `Renderer::add_effect`. |
| `Mesh`, `MeshData` | `wxsl::render` | Geometry on the GPU, and on the CPU. |
| `Environment`, `Camera`, `Light` | `wxsl::render` | The frame's camera, lights, exposure and time. |
| `DrawList`, `DrawItem` | `wxsl::render` | What one frame draws: a mesh, a material, a transform, tags and bindings. |
| `Scene` | `wxsl::core` | The scene document: meshes, materials, instances, and since v2 textures, bindings, nodes and an optional environment. |
| `SceneResources` | `wxsl::scene` | A scene loaded onto the GPU. `create_bindings()` uploads its textures and applies its bindings; `draw_list()` turns it into a `DrawList`; `wxsl::scene::environment` turns its environment block into an `Environment`. |
| `import_gltf`, `ImportedScene` | `wxsl::asset` | A `.gltf`/`.glb` file as a scene document with generated `gltf_pbr` materials, its geometry and images, and named warnings (`gltf` feature). `SceneResources::load_imported` uploads it. The device-free crate under it is `wxsl-gltf`. |
| `Editor` | `wxsl::editor` | The visual editor, behind the `editor` feature. |

The examples in `crates/wxsl/examples/` are complete programs:

- `pbr_cube`: one material, both pipelines;
- `scene_check`: a scene document;
- `viewer`: a glTF file under an HDR environment (`--features gltf`;
  `damaged-helmet`, `car-concept` and `glam-velvet-sofa` are the samples in
  `resources/glb/`);
- `gallery`: pipeline documents and effects;
- `editor`: embedding the editor in a winit loop.

## Stability

The workspace is pre-`0.1` and moving towards a stable surface in three
tiers ([ADR 0078](adr/0078-three-api-tiers-pinned-by-public-api-snapshots.md)):

- **stable:** the names each crate exports at its root, the document
  formats and the C ABI;
- **unstable:** everything reached only through a module path, and the
  whole of `wxsl-frame`;
- **internal:** private.

Every public item is listed in `api/<crate>.txt`. A change to the public
API shows up as a diff of those files, which `bash
scripts/public-api.sh --bless` regenerates.

For C and C++, see [`cpp.md`](cpp.md).
