# Architecture map

This is the map; the ADRs in `docs/adr/` are the territory's legal record.
When the two disagree, trust the ADRs and fix this file.

## Vision, one paragraph

`wxsl` lets you build a material/shader as a node graph (visually, via
`wxsl-editor`, or programmatically against `wxsl-core` directly),
compiles that graph to [WXSL](https://wesl-lang.dev), and runs it through a
`wgpu` renderer (`wxsl-render`) that can switch between forward and
deferred rendering without you maintaining two graphs. An original,
from-scratch library of granular base nodes (`wxsl-stdlib`) — math,
color, lighting, SDFs, noise, and so on, in the spirit of libraries like
[LYGIA](https://lygia.xyz) but not derived from one — ships as part of the
default build (ADR 0007).

## Crate graph

```mermaid
graph LR
    core["wxsl-core<br/>(graph model + WXSL codegen)<br/>no wgpu, no GUI"]
    lang["wxsl-lang<br/>(WXSL compiler:<br/>parser + WGSL backend)"]
    render["wxsl-render<br/>(wgpu pipelines,<br/>forward/deferred switching)"]
    editor["wxsl-editor<br/>(visual node editor,<br/>GUI toolkit)"]
    stdlib["wxsl-stdlib<br/>(original base nodes)<br/>MIT/Apache-2.0"]
    facade["wxsl<br/>(facade crate, feature-gated re-exports)"]

    render --> core
    render --> lang
    editor --> core
    stdlib --> core
    facade -. "render feature (default)" .-> render
    facade -. "editor feature" .-> editor
    facade -. "stdlib feature (default)" .-> stdlib
    facade --> core
```

Arrows point from dependent to dependency. The only crate every build
includes is `wxsl-core`. See
[ADR 0002](adr/0002-cargo-workspace-crate-boundaries.md) for why the split
exists and which edges must never appear.

## Module map

What lives where, now that the crates have contents. Each module's own doc
comment is the detailed version.

**`wxsl-core`** — `node` (value types, sockets, `WeslFunction`
descriptors, `NodeDefinition`, the registry), `graph` (nodes, edges,
validation, traversal, the serialized node format), `codegen` (graph → WXSL,
plus the generated macro module), `macros` (macro variables), `abi` (the
shader ABI's names and field tables), `wesl` (identifier/float/hash helpers),
`error`.

**`wxsl-stdlib`** — `shaders` (the embedded `.wxsl` sources, keyed by
module path), `registry` (every function and operator as a node definition).

**`wxsl-render`** — `path` (`RenderPath`), `library` (`ShaderLibrary`),
`material` (a graph compiled to WXSL), `variants` (WXSL → WGSL and the
variant cache), `pipeline` (the `Pipeline` trait, forward and deferred),
`renderer` (the front end that hides the path switch), `scene` (camera,
lights, uniform layouts), `mesh` (vertex format, cube), `gpu` (device setup,
offscreen rendering and readback), `error`.

**`wxsl`** — feature-gated re-exports, plus `stdlib_library()`, the one
line that hands the node library's WXSL to the renderer.

## Feature flags (`wxsl` facade crate)

| Feature | Default | Adds | Implies |
|---|---|---|---|
| `render` | **on** | `wxsl-render` (wgpu pipelines) | — |
| `stdlib` | **on** | `wxsl-stdlib` (original base nodes) | — |
| `editor` | off | `wxsl-editor` (visual node editor) | `render` |

A headless runtime that just loads and runs a pre-authored graph can use
`default-features = false, features = ["render"]` — no GUI toolkit anywhere
in its dependency tree. A build with nothing but `wxsl-core` (e.g. an
offline graph validator/exporter) uses `default-features = false` with no
features at all.

## Data flow: authoring to pixels

```mermaid
graph TD
    author["Graph authored<br/>(editor UI, or built programmatically,<br/>or loaded from the node format)"] --> model
    model["wxsl_core::graph::Graph<br/>(typed, acyclic, validated)"] --> codegen["wxsl_core::codegen<br/>(graph -> WXSL source)"]
    codegen --> weslsrc["one WXSL module:<br/>imports + wxsl_material()<br/>+ @if-gated entry points"]
    codegen --> macromod["generated macro module<br/>(numeric macro variables<br/>as const declarations)"]
    library["wxsl_render::ShaderLibrary<br/>(the ABI + node functions,<br/>supplied by the application)"] --> compiler
    weslsrc --> compiler["wesl<br/>(WXSL -> WGSL: resolves imports,<br/>evaluates @if/@elif/@else)"]
    macromod --> compiler
    compiler --> variants["wxsl_render::variants<br/>cache: (source+macros hash, RenderPath)<br/>-> wgpu shader module"]
    path["Active RenderPath<br/>(Forward | Deferred)<br/>chosen by the application"] --> variants
    variants --> pipeline["wxsl_render::pipeline<br/>(forward: 1 pass;<br/>deferred: G-buffer + lighting pass)"]
    pipeline --> gpu["wgpu render passes"]
```

The `RenderPath` is a property of the *pipeline*, never of the *graph* — a
material graph is written once and works under either path because the
path-specific differences are expressed as conditional compilation inside
one WXSL module, not as two separate graphs. See
[ADR 0005](adr/0005-render-pipeline-abstraction-and-shader-switching.md).

## What a graph is responsible for

A material graph describes a *surface*, not a whole shader: it compiles to
one function taking the per-fragment `SurfaceContext` and returning a
`Surface` (base colour, metallic, roughness, normal, emissive, occlusion,
alpha). Codegen wraps that in the entry points, and the vertex stage, light
loop and G-buffer packing are hand-written WXSL in `wxsl-stdlib`.

`wxsl_core::abi` is where the two halves agree on names and field
layouts, and where the render-path flag and the macro variables the ABI
honours are declared. See
[ADR 0008](adr/0008-surface-graphs-and-a-named-shader-abi.md).

Both paths call the *same* `shade_surface` function — the forward fragment
entry directly, the deferred lighting pass after unpacking the G-buffer — so
the two cannot drift apart in what lighting means. `crates/wxsl/tests/`
asserts they render the same image.

## Bind groups

WebGPU guarantees only four bind groups, so all four are allocated up front,
ordered by how often their contents change — a backend may disturb
higher-numbered groups when a lower one is rebound.

| # | Slot | Rebound | Holds |
|---|---|---|---|
| 0 | `frame` | per frame | Camera, scene lighting, object transforms |
| 1 | `material` | per material | A graph's parameters, textures, samplers |
| 2 | `user` | whenever | Nothing wxsl binds — the application's slot |
| 3 | `pass` | per pass | The G-buffer, and future shadow/IBL resources |

Object transforms share the frame group despite changing per draw: a
dynamic offset addresses them for free, where a group of their own would
cost a quarter of the budget. That leaves the application one free slot, not
two — the honest cost of the deferred path needing somewhere to put the
G-buffer while a graph still has to compile for either path.
`wxsl_core::abi::BIND_GROUPS` is the single declaration, and a test in
`wxsl-stdlib` asserts the shipped `.wxsl` binds the groups it names. See
[ADR 0010](adr/0010-four-bind-groups-allocated-by-update-frequency.md).

## Macro variables

Not everything a node needs can be a socket value: a loop bound has to be a
compile-time constant, and a lighting-model switch should remove code rather
than pick between two results. Those are *macro variables*
(`wxsl_core::macros`), declared by node definitions and pinned per graph
in the node format:

| Kind | Becomes | Example |
|---|---|---|
| `Flag(bool)` | a WXSL conditional-translation feature (`@if(name)`) | `wxsl_fbm_ridged`, `wxsl_tonemap` |
| `Int` / `Float` | a `const` the declaring module carries | `WXSL_FBM_OCTAVES` |

A macro is declared, with a default, by the WXSL module that uses it —
`@macro const OCTAVES: i32 = 5;` — and is an ordinary module-scope `const`
after binding, which is why it can be a loop bound or an array size.

Precedence, weakest first: the declaring file's default, each node's declared
default, what the graph pins, then what the application overrides. The whole
set is part of the variant cache key, because the bindings also reach
*imported* modules, whose own defaults are nowhere in the root source.

## The renderer takes its shaders from the application

`wxsl-render` compiles WXSL but ships none — it must not depend on
`wxsl-stdlib` (ADR 0002), so the application hands it a `ShaderLibrary`.
With both facade features on that is `wxsl::stdlib_library()`. This is
also the seam for substituting an ABI module or adding hand-written WXSL. See
[ADR 0009](adr/0009-the-application-supplies-the-shader-library.md).

## Why WXSL and not raw WGSL

WGSL alone has no imports and no conditional compilation, both of which
this project needs structurally: composing node functions, and branching
shader output per render path or feature. It also has no generics, which
the node system needs so that one authored function can serve `f32`,
`vec2f`, `vec3f` and `vec4f`.

WXSL is a superset of WGSL adding four things — templates, imports,
conditional translation and macro constants — compiled by `wxsl-lang` and
lowered back to WGSL, because WGSL is what `wgpu` consumes.

This started as a dependency on [WESL](https://wesl-lang.dev), which
provides imports and conditional translation. That was
[ADR 0003](adr/0003-wesl-as-the-shading-language.md), now superseded: WESL's
generics do not work (tested, with the evidence recorded in that ADR), and
templates were the feature the node system could not do without.
[ADR 0011](adr/0011-own-the-shading-language.md) records the replacement and
the four cheaper alternatives it rejected first.

## The compiler

`wxsl-lang` is a source-to-WGSL compiler, in pipeline order:

| Stage | Job |
|---|---|
| `lexer` | tokens, plus WGSL's template disambiguation (`a < b` vs `vec3<f32>`) |
| `grammar` | LALRPOP, over the token stream rather than raw text |
| `cond` | evaluate `@if`, bind `@macro const` values — per module, before renaming |
| `resolve` | inline imports, mangle by origin, rewrite references respecting shadowing |
| `mono` | instantiate templates, one copy per set of type arguments |
| — | dead-code elimination from the root's declarations |
| `emit` | WGSL, refusing anything still WXSL-only |

The pass order is an invariant, not a preference:

* `cond` must run before renaming, because an `@if` names macros in its own
  module's vocabulary and a dropped branch should never have its references
  resolved.
* `mono` must run after flattening, because a template and its call sites
  can be in different files, so no per-module pass sees all of them.
* dead-code elimination must run after `mono`, so an instantiation whose
  only caller was itself dropped goes with it.

`resolve` takes the bindings and sequences all of this itself rather than
trusting callers to get it right.

Templates carry one builtin, `components(T)` — the number of scalar
components in a type, folded to an integer literal, so it works as an array
size or a loop bound. Type arguments are written explicitly (which is what
the node graph emits, since a graph knows every socket's type) or inferred
from the arguments by a deliberately shallow rule that errors rather than
guesses. [ADR 0012](adr/0012-monomorphize-templates-on-the-flat-module.md)
records why, and what a full type checker would have cost.

The backend's refusal is deliberate. If an unresolved import, an
uninstantiated template or a surviving `@if` reaches it, the error names the
construct and the pass that should have removed it — instead of `wgpu`
rejecting syntax it has never heard of.

## The base node library

`wxsl-stdlib` mirrors the category layout common to granular shader
libraries under `crates/wxsl-stdlib/shaders/` (`math/`, `color/`,
`space/`, `lighting/`, `generative/`, `sdf/`, `sample/`, `animation/`,
`filter/`, `distort/`), one `.wxsl` file per function — but every function
is original code, not a port. A `wxsl/` directory alongside them holds the
shader ABI (ADR 0008), which is plumbing rather than granular functions.

Two kinds of node come out of it. Arithmetic is an inline WXSL expression
generated per value type (`math.add.vec3f` is `{a} + {b}`), because wrapping
an addition in a function call buys nothing. Everything with a body — PBR
shading, noise, tonemapping, colour spaces — is a real WXSL function
described by a `WeslFunction` giving its module, name, parameters and return
shape, so the WXSL source stays the single definition of the behaviour and
hand-written WXSL can call the same function a graph does. An earlier plan to rewrite
[LYGIA](https://lygia.xyz) into WXSL was scrapped once its non-permissive
license ([ADR 0006](adr/0006-lygia-port-licensing-and-isolation.md)) turned
out to be a real adoption cost even fully isolated behind an opt-in
feature; ADR 0007 replaced it with this from-scratch library, which is why
`stdlib` needs no special licensing treatment and defaults on. See
[ADR 0007](adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md) and
`crates/wxsl-stdlib/shaders/README.md`'s authoring rule before adding a
function.

## Current status

Implemented and tested end to end, except the editor:

| Area | State |
|---|---|
| `wxsl-core`: node/socket model, typed acyclic graph, validation, WXSL codegen, macro variables, node format (serde) | done |
| `wxsl-stdlib`: shader ABI, 215 node definitions over arithmetic, vectors, conversions, logic, colour, space, noise, SDFs, animation, PBR lighting | done |
| `wxsl-render`: WXSL→WGSL compilation, variant cache, forward and deferred pipelines, cube mesh, scene uniforms, offscreen rendering | done |
| `wxsl`: facade, `stdlib_library()`, the `pbr_cube` demo | done |
| `wxsl-editor` | **scaffolding** — module stubs only, no UI (ADR 0004) |

The demo is the thing to run first:

```sh
cargo run -p wxsl --example pbr_cube              # windowed; F/D switch path
cargo run -p wxsl --example pbr_cube -- --headless  # both paths to PNG
cargo run -p wxsl --example pbr_cube -- --dump-wgsl # what the graph became
```

Test coverage worth knowing about, since it is what keeps the two halves of
the shader ABI honest:

- `crates/wxsl/tests/graph_to_wgsl.rs` compiles *every node in the
  library* to WGSL on both render paths, plus the demo graph, macro
  switching, and the node format's round trip. No GPU needed.
- `crates/wxsl/tests/render_cube.rs` renders the cube through both paths
  on a real device and asserts the images match. Skips when no adapter is
  available.

## See also

- `docs/adr/` — the decision log this map summarizes.
- `docs/glossary.md` — terminology used above without re-explanation.
- `AGENTS.md` (repo root) — process and conventions for making changes here.
