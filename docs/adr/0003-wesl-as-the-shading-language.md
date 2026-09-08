# 0003. WESL as the shading language, `wesl`/`wesl-cli` as the compiler

Date: 2026-09-08

Status: Superseded by [0011](0011-own-the-shading-language.md)

The reasoning for wanting imports, conditional translation and one shared
composition mechanism still holds, and WXSL provides all three. What did
not survive is the choice of *compiler*: WESL's generics do not work (the
evidence is in "Alternatives considered" below), and the node system needs
templates. ADR 0011 records the replacement.

## Context

The project needs a shading language that:

- targets `wgpu`, i.e. ultimately lowers to WGSL,
- supports composing a shader out of reusable pieces — imports/modules —
  since both the node editor and the base node library (`wxsl-stdlib`)
  fundamentally work by wiring together small shader functions, and
- has conditional compilation, since the same node graph needs to emit a
  different shader body depending on the active render path (ADR 0005) and
  active feature set.

Plain WGSL has none of the above: no imports, no modules, no conditional
compilation. Every project that needs this today either hand-rolls string
concatenation/preprocessing (fragile, no real parsing) or builds something
`naga_oil`-shaped, tied to a specific engine (Bevy, in `naga_oil`'s case).

[WESL](https://wesl-lang.dev) ("WGSL Extended") is a community effort
(the same `wgsl-tooling-wg` group behind `wgsl-parse` and
`wgsl-analyzer`) building exactly this as a portable superset of WGSL:
`@if`/`@elif`/`@else` conditional compilation, import statements, and
package-style shader distribution via Cargo, with a Rust implementation
(the `wesl` crate, plus a `wesl-cli` binary) that compiles WESL down to
WGSL. It's young (0.x) but framework-agnostic by design, which matches this
project's goal of being a standalone library rather than tied to one engine.

## Decision

- Node graphs compile to **WESL source**, not directly to WGSL. A node's
  implementation is a WESL function (or import of one); the graph compiler
  in `wxsl-core::codegen` emits a `.wxsl` module that wires those
  functions together, using WESL's import syntax rather than string-pasting
  function bodies together.
- We depend on the `wesl` crate (compiler) and `wesl-cli` (tooling) rather
  than writing our own WESL→WGSL lowering. `wxsl-render` calls into
  `wesl` at the point where a compiled graph is about to become a `wgpu`
  shader module.
- Render-path-specific and feature-specific variation (ADR 0005) is
  expressed with WESL's `@if`/`@elif`/`@else` conditional compilation
  rather than by generating entirely separate WESL source per variant by
  hand — one WESL module, compiled multiple times with different
  conditions bound.
- Hand-written WESL and node-graph-generated WESL are meant to interoperate:
  a user can write a WESL function by hand and use it as a node, or drop
  down to hand-written WESL that imports node-graph-generated modules. This
  is a direct consequence of both going through the same import mechanism
  rather than the graph compiler having its own bespoke composition format.

## Alternatives considered

- **Compile the graph straight to WGSL by string templating.** Rejected:
  reinvents (poorly) the module/import and conditional-compilation features
  WESL already provides, and produces output that can't interoperate with
  any hand-written WESL a user brings.
- **WESL generics (`@type(T, f32 | vec2f | vec3f | vec4f)`) to write a
  type-polymorphic function once.** Rejected: not functional in `wesl`
  0.4.4. Tested on 2026-09-08, with the `generics` crate feature enabled, in
  every configuration we could form:

  | Setup | Result |
  |---|---|
  | generic in an imported module, called as `f<f32>(…)` and `f<vec3f>(…)` | panic inside the compiler |
  | generic in an imported module, called without template arguments | `cannot find declaration of package::lib::f` |
  | generic declared in the root module, single instantiation | `cannot find declaration of f` |

  Import resolution runs before monomorphization, so a generic name never
  resolves, and the two-instantiation case — the only one that would be
  useful — panics rather than erroring. The implementation matches:
  `generics::replace_calls` `.unwrap()`s its mangled-name lookup, literal
  generic arguments hit a `todo!()`, and a `// TODO recursive` marks where
  nested calls are not visited. Upstream's own doc comment reads "Generics
  are super experimental, don't expect anything from it."

  Note that even a working implementation would not remove the per-type
  duplication that matters, because `ValueType` has no type variables: each
  type needs its own node definition with concrete sockets regardless. The
  polymorphism therefore lives in `wxsl-stdlib`'s registry, which
  generates a node *and its expression* per type from one template — see
  `shaders/README.md`. Revisit if a future `wesl` release fixes this **and**
  per-type multi-statement WESL functions become common enough to be worth
  the second mechanism.

- **Build our own composition layer (à la `naga_oil`) instead of adopting
  WESL.** Rejected: `naga_oil` itself exists because WGSL lacks these
  features, and WESL is that same idea developed as a language spec with
  multi-tool buy-in (a language server, a parser crate, a compiler) rather
  than one engine's internal preprocessor. Riding that ecosystem costs one
  external, still-0.x dependency; not riding it means re-solving the same
  problem alone with no ecosystem interoperability.

## Consequences

- The project is exposed to WESL's own churn (0.x, per the search that
  informed this ADR: import syntax and `@if`/`@elif`/`@else` landed as
  recently as WESL 0.2). Pin the `wesl`/`wesl-cli` version explicitly in
  `Cargo.toml` once real code depends on it, and expect to revisit this ADR
  if a breaking WESL release requires a nontrivial migration.
- `wxsl-core`'s codegen module only needs to *emit* WESL text and hand it
  to `wesl`; it does not need its own WGSL lowering logic.
- The base node library (`wxsl-stdlib`, ADR 0007) is written *in WESL*
  from the start, specifically so its functions can be imported the same
  way as any other node's WESL, rather than needing special-cased handling.
