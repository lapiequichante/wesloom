# wesloom

WESL shader node graphs on `wgpu`, with an optional visual node editor and
an original, from-scratch library of base shader nodes.

> **Status: scaffolding.** This repository currently contains the crate
> layout, feature flags, and design documentation for the project — no
> rendering, no `wesl` compiler integration, and no editor UI yet. See
> [Current status](docs/architecture.md#current-status).

## What this is

- A Rust library, built around [`wgpu`](https://wgpu.rs), for authoring
  shaders as node graphs compiled to [WESL](https://wesl-lang.dev) (WGSL
  Extended) rather than raw WGSL — so graphs, hand-written shaders, and the
  bundled node library all compose through the same import mechanism.
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
  headless/runtime consumer never compiles a GUI toolkit just to use the
  graph model or renderer.

## Workspace layout

| Crate | What it is |
|---|---|
| [`wesloom-core`](crates/wesloom-core) | The node/socket/graph data model and graph → WESL codegen. No `wgpu`, no GUI toolkit. |
| [`wesloom-render`](crates/wesloom-render) | The `wgpu` renderer: pipeline abstraction, forward/deferred shader-variant switching. |
| [`wesloom-editor`](crates/wesloom-editor) | The visual node editor UI. |
| [`wesloom-stdlib`](crates/wesloom-stdlib) | The base node library: original shader functions, written from scratch. |
| [`wesloom`](crates/wesloom) | The facade crate most consumers depend on; re-exports the above behind Cargo features. |

Full crate graph, data flow diagrams, and the "why" behind this split live
in [`docs/architecture.md`](docs/architecture.md).

## Feature flags

```toml
[dependencies]
# Default: wgpu renderer + base node library, no visual editor.
wesloom = "0"

# Headless: graph model + codegen only, no wgpu, no GUI toolkit, no stdlib.
wesloom = { version = "0", default-features = false }

# With the visual editor (implies `render`):
wesloom = { version = "0", features = ["editor"] }
```

## Licensing

Everything in this workspace, `wesloom-stdlib` included, is dual-licensed
MIT / Apache-2.0 — see [`LICENSE-MIT`](LICENSE-MIT) and
[`LICENSE-APACHE`](LICENSE-APACHE). `wesloom-stdlib` is original code, not a
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
