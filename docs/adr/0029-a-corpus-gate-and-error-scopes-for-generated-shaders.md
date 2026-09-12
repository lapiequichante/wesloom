# 0029. A corpus gate for generated shaders, error scopes in GPU tests, and the budget numbers from the spec table

Date: 2026-09-12

Status: Accepted

## Context

M6 generated most of the ABI's shader text in Rust, and every generator
bug — a macro declared twice, an unresolved name, a wrong accessor —
surfaced the same way: as a `wgpu` validation panic during a GPU test,
minutes after the change that caused it, with the message half-swallowed
by the uncaptured-error handler. The information needed to fix such a bug
was in the WXSL compiler's diagnostics the whole time; nothing ran the
generated text through the compiler until a device was involved.

The same milestone mis-guessed the attachment-budget arithmetic once
("4 bytes for Rgba8Unorm"), and the number that would have corrected it —
the spec's cost table — was not pinned anywhere.

## Decision

Three pieces of developer-experience floor, all test-side; no runtime
behaviour changes:

* **The corpus gate** (`wxsl-stdlib/tests/lighting_models.rs`) is *the*
  place every generated shader is checked, with no device: every shipped
  model compiled standalone and as a direct dispatch, every telling
  lighting set as a switch, the lighting pass for every telling set under
  every binding of the ABI's macro flags, and the generated material
  module for every stage of every telling set. It fails with the
  compiler's rendered diagnostic and names the combination. The flag
  cross-product is derived from `abi::abi_macros`, so a flag added there
  joins the gate instead of escaping it. A generator or template change
  that breaks a shader now fails here in about a second.
* **Error scopes in the GPU harness** (`tests/probe/mod.rs`): every frame
  a test renders runs inside a validation error scope. A pipeline or
  shader module that fails validation surfaces as a panic carrying the
  driver's full text, attributed to the frame that caused it — instead of
  an uncaptured-error abort with the message truncated. Tests that expect
  a *named* renderer error are unaffected: those return before any GPU
  work.
* **The budget numbers are pinned by a test**
  (`gbuffer_bytes_per_sample`): golden totals per telling set, each
  number annotated with the spec's cost table (four-channel targets cost
  8 bytes per sample whatever their bit depth; a pair 4; a scalar 1;
  alignment rounds the running total up before each add). The test
  documents that the shipped full set costs 30, not the naive sum 29 —
  the round-up is the part that was once guessed wrong.

The acceptance-test sampling vocabulary (world point → pixel, colour at
a point, worst-channel gap over a patch) moved into the shared
`tests/probe` harness, so the next milestone's acceptance test is a
scene description plus assertions rather than new image arithmetic.

## Alternatives considered

* **Validate inside `wxsl-render` at generation time** — have the
  renderer compile its generated text with `naga` before handing it to
  `wgpu`. Rejected: it couples the renderer to a second parser's opinion,
  and the corpus gate already catches the same errors in a fraction of
  the time, where the *generators* live.
* **A dedicated test binary for the gate.** Kept inside
  `wxsl-stdlib`'s tests instead: the gate needs the shipped sources, the
  registry and `wxsl-lang`, and one more binary buys nothing.

## Consequences

* A new generator, a new lighting model, a new material stage or a new
  ABI macro should extend the gate's tables — the material-module test
  enumerates stages and sets, so most additions are covered by adding a
  telling set or letting `abi_macros` grow.
* GPU test failures now name validation errors inline; when a test
  *wants* to exercise invalid shaders it should push its own scope (as
  `wgsl_types.rs` does) rather than fighting the harness's.
* If `gbuffer_bytes_per_sample`'s cost table ever changes, this test's
  golden numbers change with it, and the ADR that changed the table says
  why the spec's numbers changed.
