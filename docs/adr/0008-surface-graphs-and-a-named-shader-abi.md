# 0008. A material graph describes a surface, against a named shader ABI

Date: 2026-09-08

Status: Accepted

## Context

ADR 0003 settled that graphs compile to WXSL and ADR 0005 that one WXSL
module serves both render paths. Neither says *what* a graph is responsible
for. Two readings were available once implementation started:

1. A graph produces the whole shader: its own entry points, its own uniform
   bindings, its own light loop.
2. A graph produces one function — the surface — and something else provides
   the entry points, bindings and lighting around it.

Reading (1) makes every graph carry the renderer's plumbing, which means the
plumbing is duplicated per graph and can disagree with the renderer's bind
group layouts. It also makes ADR 0005's promise hard to keep: a graph author
would be the one writing the two path-specific entry points.

Reading (2) needs a *contract*: names and types that the generated module and
the hand-written WXSL both agree on. Somebody has to own that contract, and
`wxsl-core` is the only crate both halves depend on.

A second question arrived with it. Some node behaviour cannot be a socket
value: the octave count of an FBM loop is a loop bound, and a lighting model
switch removes code rather than selecting between two results. WXSL has
conditional translation and `const` declarations for exactly this, but a WXSL
module cannot see declarations in the root module that imports it, so
"the host sets a constant" needs somewhere to put it.

## Decision

- **A material graph describes a surface.** It compiles to a function
  `wxsl_material(ctx: SurfaceContext) -> Surface`: per-fragment inputs in,
  material properties out. Codegen wraps it in the imports and the two
  `@if`-gated fragment entry points; the light loop, the vertex stage and the
  G-buffer packing are hand-written WXSL.
- **`wxsl_core::abi` is the contract.** Module paths, struct names,
  function names, the `SurfaceContext` and `Surface` field tables, the
  G-buffer layout, and the feature flags the ABI honours all live there as
  data. The input nodes and the output node are *generated from those tables*
  (`abi::context_node_defs`, `abi::surface_output_def`) rather than declared
  a second time, so a field cannot exist in the struct but not as a node.
- **`wxsl-stdlib` ships the implementation** of that ABI as
  `package::wxsl::{bindings, surface, vertex, shading, deferred,
  lighting_pass}`. An application may substitute its own: the renderer
  resolves imports against whatever shader library it is handed (ADR 0009).
- **Both render paths call the same shading function.** `shade_surface` is
  called by the forward fragment entry and by the deferred lighting pass, so
  the two paths cannot drift apart in what lighting means — a property worth
  having as a test, and `tests/render_cube.rs` asserts the two paths produce
  the same image.
- **Macro variables** (`wxsl_core::macros`) are the graph-level knobs that
  change the shape of the shader rather than a value in it. A node definition
  declares one with a default; a graph pins values (in the node format, so
  they are editable); an application can override on top.
  - A `Flag` becomes a WXSL conditional-translation feature (`@if(name)`).
  - An `Int`/`Float` becomes a `const` declaration in a *generated module*,
    `package::wxsl::macros`, which both the generated material module and
    hand-written stdlib functions import from. That module exists precisely
    because a WXSL module cannot reach into the root module importing it.
  - The whole macro set is part of the shader variant cache key, since
    neither kind of macro shows up in the root module's own declarations.
- **The render path is not a macro variable.** `wxsl_deferred` is bound by
  the renderer from the active pipeline's `RenderPath` and overwrites anything
  a graph pinned under that name: a graph that could choose its own entry
  points would be a graph that can be used in the wrong pass.

## Alternatives considered

- **Graphs emit whole shaders (reading 1).** Rejected: it duplicates the
  renderer's binding layout into every graph and puts the two path-specific
  entry points in the graph author's hands, which is what ADR 0005 exists to
  avoid.
- **The ABI lives in `wxsl-stdlib`, with core knowing nothing.** Rejected:
  `wxsl-render` needs the entry point and G-buffer names to build
  pipelines, and it must not depend on `wxsl-stdlib` (ADR 0002). Core is
  the only crate both can see.
- **Numeric macros as pipeline-overridable constants (`override`).** WGSL's
  `override` is set at pipeline creation with no recompilation, which is
  attractive — but an `override` cannot be a loop bound or an array size,
  which is most of what these macros are for. Kept in mind for values that
  are merely tunable; the graph model has sockets for those anyway.
- **String-substituting macro values into node WXSL before compiling.**
  Rejected for the reason ADR 0003 rejected string templating: it bypasses
  the parser, and `const` plus imports already expresses it.

## Consequences

- Changing a `SurfaceContext` or `Surface` field is a two-file change:
  `abi.rs`'s table and `shaders/wxsl/surface.wxsl`. The tables are
  documented as being in declaration order for that reason, and
  `tests/graph_to_wgsl.rs` compiles every node so a mismatch fails loudly.
- The same applies to the uniform structs: `wxsl-render`'s `scene` module
  mirrors `shaders/wxsl/bindings.wxsl` field for field, with a test on the
  struct sizes, because a layout mismatch corrupts every frame silently.
- A stdlib function that reads a macro variable must have that macro declared
  on its node definition, or the generated macro module will not declare it
  and the import will fail. `shaders/README.md` states this as an authoring
  rule.
- Adding a render path (ADR 0005 anticipates a visibility-buffer path) means
  a new arm in the generated entry points and a new flag name here, not a new
  compiler.
- Graphs that want to output something other than a surface (a post-process
  effect, a compute kernel) are not covered by this ADR and would need a
  second ABI shape alongside this one.
