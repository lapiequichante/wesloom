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
- A `wgpu` renderer that switches between forward and deferred rendering
  without the graph author maintaining two versions of a material — the
  render path is a property of the active pipeline, and path-specific
  differences are compiled conditionally from one graph.
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
cargo run -p wxsl --example pbr_cube               # windowed; F/D switch render path
cargo run -p wxsl --example pbr_cube -- --headless # one PNG per path, and their difference
cargo run -p wxsl --example pbr_cube -- --dump-wgsl --path deferred
```

A cube whose PBR material — noise-driven roughness, stepped metallic
patches, an sRGB albedo converted to linear light, a pulsing emissive — is
[a node graph in a JSON file](crates/wxsl/assets/pbr_cube.wxsl.json),
not code. Nothing in that graph mentions forward or deferred: the render path
is a property of the pipeline, and the two fragment entry points are selected
by WXSL conditional translation from one generated module. In the window,
`F` and `D` switch path and `N`/`T`/`R`/`Up`/`Down` change macro variables,
each of which compiles a new shader variant once and then hits the cache.

```text
      forward path                          deferred path
  ┌──────────────────────┐        ┌──────────────────┐   ┌────────────────┐
  │ material -> shade    │        │ material ->      │   │ G-buffer ->    │
  │ -> colour            │        │ G-buffer (3 RTs) │──>│ shade -> colour│
  └──────────────────────┘        └──────────────────┘   └────────────────┘
        one graph, one WXSL module, two `@if`-gated fragment entry points
```

## Workspace layout

| Crate | What it is |
|---|---|
| [`wxsl-core`](crates/wxsl-core) | The typed, acyclic node/socket/graph model, its serialized node format, macro variables, the shader ABI, and graph → WXSL codegen. No `wgpu`, no GUI toolkit. |
| [`wxsl-render`](crates/wxsl-render) | The `wgpu` renderer: WXSL → WGSL compilation, the shader variant cache, and the forward and deferred pipelines. |
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
