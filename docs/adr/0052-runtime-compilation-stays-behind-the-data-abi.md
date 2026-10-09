# 0052. Runtime compilation stays behind the data ABI

Date: 2026-10-09

Status: Accepted

Implements plan4 B4; extends [0051](0051-dawn-implements-only-the-device-half.md).

## Context

The offline device backend now agrees with Rust. C++ applications need to
compile edited materials without acquiring another compiler or variant-key rule.

## Decision

An optional C++ `Compiler` dynamically loads the generated C ABI, checks its
version and copies result views before freeing them. It caches successful,
identical JSON requests (bounded to 64 entries); variant keys remain Rust data.
The offline renderer does not require or link the Rust library.

Runtime bundles carry the original pipeline/material/effect requests. Dawn can
recompile their stock plans and shaders and replace a material transactionally.
An edit changing the material layout signature, draw-selection flags, pipeline
configuration or host geometry is refused: those require a new host bundle.
Compile failures leave the previous material and pipelines usable. Parameters
can be uploaded separately without shader compilation or pipeline invalidation.
This synchronous headless API does not introduce an editor, worker queue or
surface integration.

## Alternatives considered

- Duplicate Rust variant hashes/layouts in C++: breaks the shared-core rule.
- Require Rust for all device applications: loses offline deployment.

## Consequences

Runtime acceptance must render without reading offline WGSL, agree with the
offline/native reference, exercise edits and cache hits, and retain the last
valid rendering after a named compiler/layout failure. B5 uses the same fixed
comparator, a curated scene list and one rendered sample per stdlib category.
