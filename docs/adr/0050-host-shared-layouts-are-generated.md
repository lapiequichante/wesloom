# 0050. Host-shared layouts are computed or generated, never mirrored

Date: 2026-10-09

Status: Accepted

Amends [0008](0008-surface-graphs-and-a-named-shader-abi.md) and
[0021](0021-a-declarative-render-graph-and-a-scene-document.md). Implements plan4 B6.

## Context

Dawn adds a C++ host to the camera/light/scene/instance layout mirrors.
Keeping another struct in sync would multiply a silent corruption risk.

## Decision

`wxsl-core::host` declares the fixed frame-buffer fields once and generates
the Rust definitions, C/C++ header and WXSL declarations. Build scripts
consume that same generator. Existing names, offsets and buffer sizes remain
unchanged; parameter/user/instance-attribute buffers still use BufferLayout.
The base vertex and UI instance definitions are generated from the existing
ABI attribute tables, using tight vertex packing rather than uniform rules.
Backend implementations keep uploads and native vertex-layout adapters.
MSDF edge/job storage buffers and the UI viewport use the same schema;
storage alignment is distinct from uniform alignment and packed vertices.

Generation checks every host offset/size against the table. Generated Rust
also derives Pod, which rejects implicit padding. The generated C header
asserts sizes and offsets when compiled in either C or C++. Shader templates
carry explicit generation markers, not a second field declaration.

## Alternatives considered

- Another hand mirror, even test-pinned: adds an authoring site instead of
  removing one.
- Parse Rust structs to recover WGSL layout: conflates natural host alignment
  with WGSL's uniform and storage rules.

## Consequences

Changing a table field regenerates all three consumers. GPU probes and the
backend parity tests remain required: layout generation alone cannot prove
the buffers were bound or uploaded correctly. UI remains wgpu-only; sharing
its schema does not authorize porting the editor.
The checked-in C header is test-pinned to the generator, and offline bundles
carry a layout fingerprint: a stale header fails before device creation.
