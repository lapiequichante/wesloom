# 0012. Monomorphize templates on the flat module, with shallow inference

Date: 2026-09-08

Status: Accepted

## Context

[ADR 0011](0011-own-the-shading-language.md) decided to own the shading
language, and templates were the reason. `wxsl-lang` has parsed
`fn f<T: f32 | vec2f>(a: T) -> T` and checked its constraints since then,
but nothing instantiated it: the WGSL backend refused any template that
reached it. This ADR decides how instantiation works.

Three questions had to be answered, and each has a wrong answer that looks
reasonable.

**Where in the pipeline.** A template and its call sites can be in different
files — that is the entire point of a shader standard library. So no
per-module pass can see all of the call sites.

**Where the type arguments come from.** The node graph knows every socket's
type exactly, so it can always write them. A hand-written shader would
rather not: `inverse_lerp<vec3f>(a, b, t)` is noise when `a` is visibly a
`vec3f`.

**What a template body can ask about its type parameter.** The original
sketch for this feature (`@@sizeOf(T)`, `@@address(T, index)`) assumed a body
would need to take a type apart. Most do not: `(value - low) / (high - low)`
is already correct for a scalar and for every vector width, and `v[i]`
replaces `@@address` outright. But a body that reduces a vector to a scalar
does need to know the width.

## Decision

**Monomorphization runs on the flattened module**, after import resolution
and conditional translation, before dead-code elimination
([`crate::mono`](../../crates/wxsl-lang/src/mono.rs)). Each template becomes
one concrete declaration per distinct set of type arguments, every reference
is rewritten to name the copy, and the template is deleted. An instantiation
exists only if something asks for it, and dead-code elimination running
afterwards is what drops an instantiation whose only caller was itself
dropped.

Running after resolution means every name is already final, so an instance
name needs no mangling of its own: `remap` imported from `package::math` and
instantiated at `vec3f` is `package_math_remap_vec3f`. That name appears in
shader diagnostics and in captured GPU frames, and being able to read both
the origin and the type off it is worth the length.

**Type arguments are explicit or shallowly inferred.** Explicit always
works and is what the node graph emits. Otherwise a type parameter is bound
from an argument when the parameter's declared type is *exactly* that type
parameter and the argument's own type is evident from syntax alone: a name
with a declared type in scope, a literal with a type suffix, a constructor
call, a call to a function whose return type is known, or an operator
applied to two operands of the same evident type.

Everything else declines, and declining is an error that names the parameter
and asks for the types to be written. There is deliberately no guessing:

* an unsuffixed literal has no type (`1.0` is not `f32` here);
* a comparison is not its operands' type (`x > y` is a `bool`);
* mixed operands are not the left operand's type (`m * v` is a `vec3f`, and
  so is `v * v`, but only one of those has a `vec3f` on the left).

A wrong instantiation is far worse than a diagnostic, because it fails
inside a body the author did not write.

**`components(T)` is the only builtin**, folding to an integer literal — 1
for a scalar, `N` for `vecN`, `C * R` for `matCxR`. Being a literal, it works
as an array size or a loop bound. It folds in concrete code too, and not at
all if the module declares something of its own called `components`.

Two smaller rules fall out:

* **Predeclared type aliases are canonicalized** to their short spelling, so
  `vec3<f32>` and `vec3f` are one type and one instantiation. WGSL says they
  are the same type; `TypeExpr::same_type` is structural and would not have
  noticed.
* **An instance name that collides with an existing declaration is an
  error**, not silently renamed with a counter. A counter would make the
  name depend on compilation order, and that name is in the shader cache key
  and in GPU captures.

## Alternatives considered

* **Instantiate per module, before flattening.** Cheaper, and wrong: a
  template in `package::math` has no way to see the call in `package::main`.
  Only a whole-program pass can enumerate the instantiations.

* **Explicit type arguments only.** Honest and much smaller. Rejected
  because it taxes exactly the code a human writes, while the caller that
  can always supply the types — the node graph — is the one that does not
  mind. The shallow rule costs about eighty lines and covers the common
  shapes.

* **A full type checker, so inference always succeeds.** This is the
  expensive answer: WGSL's overload table, abstract numeric types and their
  conversion ranks, swizzle result types, pointer types. It would make every
  call inferable and every gap in it a wrong instantiation rather than a
  diagnostic. Not worth it until hand-written shaders prove the shallow rule
  is not enough — and it is an additive change when they do.

* **`sizeof(T)` in bytes.** What the crate's own documentation promised
  before this was implemented. Rejected: inside a template over
  `f32 | vec2f | vec3f` the useful question is how many lanes there are, not
  how much memory they occupy, and `vec3f` answers 12 to the second question
  and 3 to the first. Naming it `components` makes the answer unambiguous.

* **`@@address(T, index)`, lowering to `.x` / `.y`.** Obviated: `v[i]`
  already indexes a vector in WGSL. The remaining gap is that a scalar is
  not indexable, which needs a conditional rather than an accessor.

* **Deferring `@if` inside a template so it can test `components(T)`.**
  This would make `@if(components(T) > 1)` work, which is how a body would
  split the scalar case from the vector case. It needs conditional
  translation to run twice — once per module, then again per instantiation
  with the macro values that module resolved — and per-declaration
  provenance for those values. Deferred: no shipped shader needs it, and
  `cond` now says precisely why an `@if` cannot use `components` rather than
  failing with a generic message. Purely additive if it becomes necessary.

## Consequences

Templates work end to end: the pipeline table in
[architecture.md](../architecture.md) gains a stage, and the WGSL backend's
refusal of an uninstantiated template becomes a backstop against a skipped
pass rather than a statement about an unimplemented feature.

`wxsl-lang` now needs a *little* type knowledge, which it did not before:
`evident_type`, `component_count` and the predeclared-alias table. That is a
new thing to keep correct as WGSL grows — `f16` is in there, and a future
scalar type would need adding to `scalar_suffix` and `component_count`.
It is not a type checker and should not be allowed to become one by
accretion; if inference needs to get materially smarter, that is the
alternative above, decided deliberately.

Follow-up this makes possible, and does not itself do:

* **Generic nodes.** The `math.inverse_lerp.f32` / `.vec3f` families in
  `wxsl-stdlib`'s registry exist because a node had to name a concrete type.
  One templated function plus a type-variable socket resolved by connection
  would collapse each family to a single node. That changes node ids and
  generated shader text, so it is its own change.
* **Instantiation is not cached across compilations.** Two variants of one
  material each monomorphize from scratch. Cheap at current sizes — the
  whole shader library is a few thousand lines — and measurable before it is
  worth fixing.
