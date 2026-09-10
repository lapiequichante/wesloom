# WXSL

Shader node graphs on `wgpu`, with its own shading language, an optional
visual node editor, and an original from-scratch library of base shader
nodes.

> **Status: working, editor included.** Graphs are authored in code, in the
> node format, or in the visual editor, and rendered through a forward or
> deferred `wgpu` pipeline. The editor draws itself with this project's own
> renderer — no GUI toolkit
> ([ADR 0013](docs/adr/0013-the-editor-draws-itself-with-wxsl-render.md)).
> See [Current status](docs/architecture.md#current-status).

## What this is

- A Rust library, built around [`wgpu`](https://wgpu.rs), for authoring
  shaders as node graphs compiled to **WXSL** — a shading language that is
  WGSL plus imports, conditional translation, macro constants and templates
  — so graphs, hand-written shaders, and the bundled node library all
  compose through the same import mechanism.
- A `wgpu` renderer whose pipeline is *data*: a list of passes over a set of
  render targets, which the engine orders, allocates and records. Forward
  and deferred are two such lists, and switching between them recompiles
  nothing the graph author has to think about — each pass names the
  material *stage* it draws with, and one graph compiles for all of them.
- Materials that **declare what they need from outside themselves**:
  uniform parameters the host changes with a buffer write and no
  recompile, textures and samplers it binds by name, and a block the
  *application* fills and this library never looks inside. The uniform
  layout is computed from the graph rather than written twice, because
  there is no fixed struct to mirror
  ([ADR 0023](docs/adr/0023-a-material-declares-its-resources.md)).
- A built-in library of granular base nodes (math, color, lighting, SDFs,
  noise, …), in the spirit of libraries like [LYGIA](https://lygia.xyz) but
  written entirely from scratch — see
  [ADR 0007](docs/adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md)
  for why it isn't a port.
- An optional visual node editor, strictly opt-in via a Cargo feature, so a
  headless/runtime consumer never compiles it just to use the graph model or
  renderer. It has no GUI-toolkit dependency at all: it draws itself with
  `wxsl-render`, through a WXSL shader compiled by this project's own
  compiler, with MSDF text generated in-tree on the CPU or in a compute pass
  ([ADR 0013](docs/adr/0013-the-editor-draws-itself-with-wxsl-render.md),
  [ADR 0014](docs/adr/0014-msdf-text-with-an-own-generator-and-app-supplied-fonts.md)).

## The editor

```sh
cargo run -p wxsl --features editor --example editor              # the shipped demo graph
cargo run -p wxsl --features editor --example editor -- --graph my.wxsl.json
cargo run -p wxsl --features editor --example editor -- --msdf gpu
cargo run -p wxsl --features editor --example editor -- --screenshot out.png
```

```text
 ┌─────────────────────────────────────────────────────────────┐
 │ toolbar: name · path · mesh · MSDF backend · add · fit      │
 ├───────────┬──────────────────────────────┬──────────────────┤
 │ palette   │ node canvas                  │ live preview    │
 │ (search,  │ (pan, zoom, link, unlink,    │ selected node   │
 │  category)│  move, delete)               │ macro variables │
 ├───────────┴──────────────────────────────┴──────────────────┤
 │ WXSL │ WGSL │ problems                                      │
 ├─────────────────────────────────────────────────────────────┤
 │ status: nodes · variants · atlas · glyphs · last message    │
 └─────────────────────────────────────────────────────────────┘
```

Every node in the graph, with its ports coloured by type; drag between ports
to link, drag a connected input to move that link, right-click one to cut it.
The preview is the real renderer on the real pipelines, and the two code
panels are the WXSL the graph generated and the WGSL that compiled to — which
is most of what makes a shader graph debuggable.

The editor itself needs no window: it takes input events and records two
passes into an encoder, so it embeds in an application that already has an
event loop. `crates/wxsl/examples/editor.rs` is the winit half, and the only
file in the workspace that knows winit exists. Fonts come from the
application (the renderer embeds none); the example finds a platform default
or takes `--font`/`--mono`.

## The demo

```sh
cargo run -p wxsl --example pbr_cube               # windowed; F/D switch pipeline
cargo run -p wxsl --example pbr_cube -- --headless # one PNG per path, and their difference
cargo run -p wxsl --example pbr_cube -- --dump-wgsl --path deferred
```

A cube whose PBR material — noise-driven roughness, stepped metallic
patches, an sRGB albedo converted to linear light, a pulsing emissive — is
[a node graph in a JSON file](crates/wxsl/assets/pbr_cube.wxsl.json),
not code. Nothing in that graph mentions forward or deferred: the pipeline is
a list of passes the renderer holds, and each pass names the material *stage*
it draws with — a final colour, a G-buffer, or depth and nothing at all. In
the window, `F` and `D` switch pipeline (compiled in the background, so the
frame never stutters) and `N`/`T`/`R`/`Up`/`Down` change macro variables,
each of which compiles a new shader variant once and then hits the cache.

The same graph samples a texture the example generates and multiplies in a
`tint` **parameter**, and `[`/`]` change that parameter live: a field of
the material's uniform buffer, so the compile count printed beside it does
not move. That is the difference between a `param` node and the `const`
one row above it in the same graph.

```text
        forward pipeline                      deferred pipeline
  ┌────────────┐  ┌──────────────┐   ┌──────────────────┐  ┌────────────────┐
  │ depth_only │─>│ forward_lit  │   │ gbuffer          │─>│ G-buffer ->    │
  │ (no pixels)│  │ -> colour    │   │ -> 3 targets     │  │ shade -> colour│
  └────────────┘  └──────────────┘   └──────────────────┘  └────────────────┘
        one graph, one module per stage, one entry point in each
```

## Workspace layout

| Crate | What it is |
|---|---|
| [`wxsl-core`](crates/wxsl-core) | The typed, acyclic node/socket/graph model, its serialized node format, macro variables, the shader ABI, the computed layout of what a material declares, and graph → WXSL codegen. No `wgpu`, no GUI toolkit. |
| [`wxsl-render`](crates/wxsl-render) | The `wgpu` renderer: the render graph that turns a list of passes into a frame, WXSL → WGSL compilation, the shader variant cache, a material's bind groups, and the forward and deferred pass lists. |
| [`wxsl-render`](crates/wxsl-render)'s [`ui`](crates/wxsl-render/src/ui) | The 2D layer the editor is drawn with: a texture atlas, MSDF text from glyph outlines (CPU or compute pass), an instanced draw list, and windowing-agnostic input. Useful without the editor. |
| [`wxsl-editor`](crates/wxsl-editor) | The visual node editor: pan/zoom canvas, node palette, live material preview, and the generated WXSL and WGSL. Draws itself with `wxsl-render`; no GUI toolkit. |
| [`wxsl-stdlib`](crates/wxsl-stdlib) | The base node library: original shader functions written from scratch, plus the WXSL side of the shader ABI. |
| [`wxsl`](crates/wxsl) | The facade crate most consumers depend on; re-exports the above behind Cargo features. |

Full crate graph, data flow diagrams, and the "why" behind this split live
in [`docs/architecture.md`](docs/architecture.md).

## Feature flags

```toml
[dependencies]
# Default: wgpu renderer + base node library, no visual editor.
wxsl = "0"

# Headless: graph model + codegen only, no wgpu, no GUI toolkit, no stdlib.
wxsl = { version = "0", default-features = false }

# With the visual editor (implies `render`):
wxsl = { version = "0", features = ["editor"] }
```

## Licensing

Everything in this workspace, `wxsl-stdlib` included, is dual-licensed
MIT / Apache-2.0 — see [`LICENSE-MIT`](LICENSE-MIT) and
[`LICENSE-APACHE`](LICENSE-APACHE). `wxsl-stdlib` is original code, not a
port of any existing shader library; see
[ADR 0007](docs/adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md)
for why that mattered enough to write down.

## Working on this repo

Start with [`AGENTS.md`](AGENTS.md) — it covers the build/lint/test
commands, the crate-boundary rules, and when to write a new
[ADR](docs/adr/README.md). It applies equally whether you're a human or an
AI coding agent.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md).
