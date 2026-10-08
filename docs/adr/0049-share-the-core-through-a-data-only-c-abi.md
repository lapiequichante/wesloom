# 0049. Share the core through a data-only C ABI

Date: 2026-10-08

Status: Accepted

Implements plan4 B2; extends [0048](0048-the-frame-plan-is-device-free.md).
Amends [0002](0002-cargo-workspace-crate-boundaries.md)'s crate map.

## Context

Dawn must consume Rust's pipeline compiler, scheduler, shader compiler and
computed material layouts without linking a renderer or duplicating rules.
The boundary also needs explicit ownership, version checks and diagnostics.

## Decision

`wxsl-ffi` builds a cdylib with four operations: compile/schedule a pipeline,
compile WXSL, compile one material stage and its interface, and check a
scene against a setup. Inputs are UTF-8 JSON requests containing the existing
versioned documents. Output plans and interface metadata remain JSON; WGSL,
parameter defaults and computed field tables are borrowed byte/table views.
No device handles, callbacks, renderer state or C++ planner cross this seam.

Each operation takes the C ABI version explicitly and rejects any mismatch
before reading input. This transport version is distinct from the documents'
shader ABI revision. An opaque, immutable result owns all its views until
`wxsl_result_free`; input memory is borrowed only during the call. Results
include status and named diagnostics. Null/oversized inputs are refused and
Rust panics are caught at the operation boundary; `panic=unwind` is required
at build time (allocation failure and an
invalid foreign pointer cannot be made recoverable by a C ABI).

The first cut includes the shipped node/source vocabulary and lighting
models. Applications may overlay source modules and derive additional
function nodes from designated WXSL modules. Custom effect registrations and
custom lighting-model descriptors are deferred until they have a consumer.
Shader compilation and macro binding share `wxsl-lang`'s entry point with
`wxsl-render::variants`; parity tests compare exact text and computed layouts.

The C header is generated from the exported Rust declarations with a small
`syn`-based generator. Its checked-in copy is test-pinned. Unsafe code is
confined to foreign memory access and result ownership in `wxsl-ffi::ffi`.

For N8's format decision, authoring/runtime compilation embeds versioned JSON
and WXSL sources; an offline device-only application embeds exported plan
JSON, WGSL and layout data, without the Rust compiler. There is no new IR or
second document format. Plan/schema changes require an ABI version decision.

## Alternatives considered

- Linking `wxsl-render`: imports a GPU implementation for pure operations.
- Serializing WGSL and parameter bytes in JSON: unnecessary copying/escaping.
- C structs mirroring shader buffers: breaks when a computed layout changes.
- A hand-written header: creates another declaration that can drift.

## Consequences

- `wxsl-frame` serializes its actual descriptions and schedule, not a second
  planner's approximation. Resource ids index the descriptions; schedule
  entries index passes; allocations retain their ring/aliasing information.
- C callers own no Rust allocation directly. Views are read-only and must
  not outlive their result; free is called exactly once. Calls are independent.
- The C harness lives in `dawn/tests` and needs no Dawn SDK or GPU. B3 still
  owns the renderer and B6 still owns fixed frame-buffer generation.
- CI checks the FFI's no-GPU dependency tree, generated header, Rust parity
  tests and a dynamically loaded C/C++ header smoke test.
