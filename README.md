# WXSL

Shader node graphs, a render pipeline you author as data, and a visual
editor, for Rust on [`wgpu`](https://wgpu.rs). The same documents also run
on C++ through [Dawn](dawn/README.md).

![The cinema demo](gallery/cinema.png)

- **WXSL** is WGSL plus imports, conditional compilation, macro constants
  and templates. Graphs, hand-written shaders and the node library all
  compose through the same imports.
- **One material graph serves every pass.** It compiles separately for
  each pass that draws it (forward colour, G-buffer, depth, shadow,
  velocity), so switching between forward and deferred needs nothing from
  the graph's author.
- **Pipelines are documents.** A pipeline is a list of passes over
  resources, which the engine schedules, allocates and records. Forward and
  deferred are two shipped presets, and effects (bloom, TAA, depth of
  field, …) are nodes you wire in.
- **A library of about 100 base nodes:** math, colour, noise, SDFs, PBR and
  other lighting models, sky. They are original or audited permissive ports
  ([ADR 0053](docs/adr/0053-permissive-shader-ports-retain-their-provenance.md)).
- **An optional node editor.** It draws itself with the renderer, so it
  pulls in no GUI toolkit
  ([ADR 0013](docs/adr/0013-the-editor-draws-itself-with-wxsl-render.md)).
- **Targets:** native through wgpu, the browser on WebGPU or WebGL2, and
  C++ through a data-only C ABI.

## Try it

```sh
cargo run -p wxsl --example pbr_cube                        # one material, F/D switch pipeline
cargo run -p wxsl --example gallery                         # every pipeline feature, arrow keys to browse
cargo run -p wxsl --features editor --example editor        # the node editor
cargo run -p wxsl --features editor --example editor -- --screenshot out.png
```

`pbr_cube`'s material is
[a node graph in a JSON file](crates/wxsl/assets/pbr_cube.wxsl.json), with
comments explaining the format. Each example's `--help` lists its options.

## Use it

```toml
[dependencies]
wxsl = "0"                                              # renderer + node library
wxsl = { version = "0", default-features = false }      # graph model and codegen only, no wgpu
wxsl = { version = "0", features = ["editor"] }         # plus the editor
wxsl = { version = "0", features = ["gltf"] }           # plus glTF/GLB geometry import
```

The `wxsl` crate's documentation (`cargo doc -p wxsl --open`) starts with a
minimal program.

## Crates

| Crate | Role |
|---|---|
| [`wxsl-core`](crates/wxsl-core) | Graph model, documents, shader ABI, codegen. No wgpu. |
| [`wxsl-lang`](crates/wxsl-lang) | The WXSL → WGSL compiler. |
| [`wxsl-frame`](crates/wxsl-frame) | Device-free frame plans: pipeline documents, scheduling, effects. |
| [`wxsl-stdlib`](crates/wxsl-stdlib) | The node library, one `.wxsl` file per node. |
| [`wxsl-render`](crates/wxsl-render) | The wgpu renderer and its 2D UI layer. |
| [`wxsl-editor`](crates/wxsl-editor) | The visual editor. |
| [`wxsl-ffi`](crates/wxsl-ffi) | The C ABI that C/C++ hosts and the Dawn backend use. |
| [`wxsl`](crates/wxsl) | The facade: re-exports the above behind features. |

## Documentation

- [`docs/architecture.md`](docs/architecture.md): how the pieces fit together.
- [`docs/glossary.md`](docs/glossary.md): the terms used throughout.
- [`docs/rust.md`](docs/rust.md) and [`docs/cpp.md`](docs/cpp.md): using the
  libraries from Rust, and from C or C++.
- [`docs/adr/`](docs/adr/README.md): every design decision, with its reasoning.
- [`plan6.md`](plan6.md): the current plan. Earlier plans are in
  [`docs/history/`](docs/history).
- [`AGENTS.md`](AGENTS.md): working on the repo (commands, tests, rules),
  for humans and coding agents alike. See also
  [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

Workspace code is dual-licensed MIT / Apache-2.0
([`LICENSE-MIT`](LICENSE-MIT), [`LICENSE-APACHE`](LICENSE-APACHE)). Shader
ports keep their original licenses and notices: see [`NOTICE`](NOTICE).
Applications can redistribute the attributions from
`wxsl_stdlib::THIRD_PARTY_NOTICES`.
