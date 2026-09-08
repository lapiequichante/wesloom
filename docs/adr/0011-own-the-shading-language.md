# 0011. Own the shading language: WXSL replaces WESL

Date: 2026-09-08

Status: Accepted

Supersedes [0003](0003-wesl-as-the-shading-language.md) in its choice of
compiler. The reasons WESL was chosen over raw WGSL — imports, conditional
translation, one composition mechanism shared by generated and hand-written
code — all still stand, and `wxsl-lang` provides them.

## Context

ADR 0003 adopted WESL and the `wesl` crate so the project would not have to
build its own shader composition layer. Two things have changed since.

The first is that the node system needs *templates*: one authored function
usable at `f32`, `vec2f`, `vec3f` and `vec4f`, resolved from the graph's
types. WESL has a generics feature for exactly this, and it does not work.
Tested against `wesl` 0.4.4 with the `generics` feature enabled
(recorded in ADR 0003): a generic in an imported module cannot be resolved at
all, and the only useful case — two instantiations of one function in one
shader — panics inside the compiler.

The second is that the workarounds all cost something we would rather not
pay. Generating the per-type code from Rust templates moves shader logic out
of the shader files, which is the thing ADR 0007 says those files exist to
prevent. Instantiating a module per type with a generated `alias T` prologue
works (also tested) but cannot express code whose *validity* depends on the
type, only code whose values do. A `@@`-directive superset of WESL gets that
back, but at the price of files that are no longer WESL — losing the
interoperability that motivated ADR 0003 in the first place, while still
depending on `wesl` for everything else.

If the files are going to stop being WESL, the dependency has stopped paying
for itself.

## Decision

`wxsl-lang`: a new crate holding a lexer, a LALRPOP grammar, a resolver
and a WGSL backend for **WXSL**, the shading language this project owns.
Sources are `.wxsl`. The `wesl` and `wgsl-parse` dependencies are removed.

The language is WGSL plus four things, in decreasing order of how much they
justify the crate:

* **Templates.** `fn f<T: f32 | vec2f>(a: T) -> T`, instantiated per concrete
  type, inferred at call sites, with `sizeof(T)` available as a
  compile-time constant. This is the feature that forced the decision.
* **Imports.** `import package::math::remap;`, resolved against a module map
  supplied by the application, with mangling — the same shape as WESL's, so
  the existing shader tree ports unchanged.
* **Conditional translation.** `@if(feature)` on declarations and statements,
  driven by the graph's macro variables.
* **Macro constants.** Host-set `const` and `alias` declarations injected per
  compilation.

The backend emits WGSL, because that is what `wgpu` consumes. Nothing else
about the render path changes.

Syntax stays a superset of WGSL wherever there is a choice, so that a
`.wxsl` file is readable by anyone who knows WGSL, and so that the 34
files in `wxsl-stdlib` port with near-zero edits.

## Alternatives considered

- **Keep `wesl` and generate per-type code from Rust templates.** Shipped
  briefly and reverted by this ADR. Works, but shader bodies live in Rust
  `format!` strings, which is unreadable as shader source and unusable by
  anyone writing shaders by hand.
- **Keep `wesl` and instantiate a module per type with a generated
  `alias T` prologue.** Verified working, and the cheapest option by far
  (~130 lines). Rejected because an alias can vary the types and values in a
  body but not its *validity*: a body using `v[i]` cannot instantiate at
  `f32`, and no amount of const-folding removes the offending expression.
- **A `@@` preprocessor over WESL.** Rejected: roughly 1,100 lines of
  scanner and source-mapping — more than the 921-line shader library it
  would serve — and it forfeits WESL tooling anyway, so it pays the cost of
  owning a language without getting to design one.
- **Fork `wgsl-parse`** (4,258 lines of Rust, 1,884 of LALRPOP). Rejected as
  a starting point: tracking someone else's 0.x grammar is a permanent tax,
  and the grammar is the part we most want to control.
- **Wait for WESL to fix generics.** Rejected on timing, not on merit. The
  feature is upstream and clearly intended; if it lands and is good, the
  question of dropping `wxsl-lang`'s front end for it can be reopened —
  WXSL is a superset of WGSL, so the shader tree is not trapped.

## Consequences

- This is the largest single piece of infrastructure in the project, and it
  is on the critical path for every shader that compiles. It is also the
  piece most able to make the node system good, because the graph's type
  information can now reach the shader compiler instead of being flattened
  away first.
- Diagnostics are now ours to produce. A shader error must point at a line in
  a `.wxsl` file, through import mangling and template instantiation. This
  is the part of the work most likely to be underestimated, and the tests
  treat it as a feature rather than an afterthought.
- We lose WESL's tooling — its language server, and the ability to hand a
  library file to another WESL project unmodified. Staying a WGSL superset
  keeps the loss to the extensions actually used.
- `wxsl-render`'s `ShaderLibrary` (ADR 0009) is unaffected in shape: it
  is still a map from module path to source that the application fills. Only
  the compiler behind it changes.
- The per-type node id suffixes (`math.add.vec3f`) exist because the node
  layer had to do the monomorphization the shader language could not. With
  templates they can collapse to one generic node whose type is resolved by
  the graph, which is a separate change this ADR enables but does not make.
