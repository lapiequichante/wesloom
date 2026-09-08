# 0009. The application supplies the renderer's shader library

Date: 2026-09-08

Status: Accepted

## Context

`wxsl-render` compiles WXSL to WGSL, which means it must resolve import
paths to module sources. The modules it needs are the shader ABI
(ADR 0008) and whatever functions the graph's nodes call — all of which
`wxsl-stdlib` ships.

`wxsl-render` may not depend on `wxsl-stdlib`: ADR 0002 permits
dependency edges only *into* `wxsl-core`, so that a consumer with its own
small node set does not compile the standard library, and so that the
renderer stays usable with a hand-written material system.

Three ways out were available: break the crate boundary, put the WXSL sources
in `wxsl-core`, or have the caller provide them.

## Decision

`wxsl-render` owns a `ShaderLibrary`: a map from module path to WXSL
source, given to it at construction. It ships no shader source of its own and
reads nothing from disk at runtime.

- The application fills the library. With both facade features on,
  `wxsl::stdlib_library()` is the one-liner that puts every
  `wxsl_stdlib::MODULES` entry in it.
- `ShaderLibrary::check_abi` verifies the ABI modules are present, and
  `Renderer::new` calls it, so an incomplete library is an error at startup
  naming the missing module rather than an import failure on the first
  material compiled.
- The generated material module and the generated macro module are added
  per compilation, on top of the library, by the variant cache.

## Alternatives considered

- **Let `wxsl-render` depend on `wxsl-stdlib`.** Rejected: it inverts
  ADR 0002's only rule, and makes the standard library non-optional for every
  wgpu consumer.
- **Move the `.wxsl` ABI sources into `wxsl-core`.** Tempting, since core
  already owns the ABI's *names*. Rejected because it would make the crate
  that is meant to be a dependency-light data model the owner of shader
  source it cannot compile or test, and because it would make substituting
  the ABI implementation harder rather than easier.
- **Read `.wxsl` files from disk at runtime** (the `wxsl-lang` crate's
  `FileResolver`). Rejected as the default: a shipped application would need
  the shader tree next to its executable. `ShaderLibrary` does not preclude a
  consumer inserting sources it read from disk itself.

## Consequences

- Wiring the halves together is the application's one line of boilerplate.
  The facade's `stdlib_library()` and the `pbr_cube` example show it.
- Substituting an ABI module (a different lighting model, a different
  G-buffer packing) is supported by construction: insert your module under
  the same path *after* the stdlib's, and it wins.
- Every WXSL source is embedded in the binary via `include_str!`, so shader
  sources cannot be edited without a rebuild. A hot-reloading consumer would
  insert freshly read sources into a new library and drop the variant cache.
- `wxsl-render`'s error type carries pre-rendered diagnostics rather than
  `wesl::Error`, so the still-0.x compiler's error type is not part of this
  crate's public API.
