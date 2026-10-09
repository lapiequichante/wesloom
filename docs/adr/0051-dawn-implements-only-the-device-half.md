# 0051. Dawn implements only the device half, offline first

Date: 2026-10-09

Status: Accepted

Implements plan4 B3; builds on [0049](0049-share-the-core-through-a-data-only-c-abi.md)
and [0050](0050-host-shared-layouts-are-generated.md).

## Context

A second backend must render the same documents without implementing another
scheduler, shader compiler, layout computer or capability handshake.

## Decision

`dawn/` is a CMake C++ library and headless demo. Conan 2 manages C++
dependencies; the official Dawn release is pinned by commit and archive
SHA-256 in a local recipe. The cache/build outputs live under ignored target/.

A Rust offline exporter emits the shared plan/schedule, computed binding and
material layouts, WGSL, draw selections and CPU upload data. C++ consumes
these artifacts and owns device setup, GPU resources, pipelines, recording
and readback. It never parses a material graph or re-derives its rules. The
device binary links no Rust library. Runtime compiler integration is B4.

The first acceptance is both stock presets and scene_check headless. Device
features/limits are queried, unsupported requirements fail by name, and
unknown descriptor cases fail rather than silently substituting defaults.
The editor and surface/window integration are not part of this headless cut.
The offline demo supplies procedural meshes, default parameter/instance
values and a checker texture. File meshes, application uniform blocks,
additional vertex streams and external imports need host support; they are
refused, not silently ignored. The exporter exposes the two stock documents
and resource probes, not yet arbitrary application pipeline documents.

The acceptance comparator uses a mean absolute RGBA8 difference of at most
0.005 and at most 1% of pixels differing by more than 0.05 in a colour
channel. These thresholds allow G-buffer/native arithmetic rounding, not
missing geometry or a wrong binding. They are fixed before the first Dawn
render, not tuned until green. B5 widens the corpus and CI's GPU coverage.

## Alternatives considered

- Build a new C++ planner: prohibited by the shared-core decision.
- Link Rust into the renderer now: skips the offline-first milestone.
- Fetch latest Dawn during CMake configuration: unreviewed API/binary drift.

## Consequences

Conan is required only for renderer builds; the existing ABI harness still
runs without Dawn. Fixed host structs come from the generated header. Offline
assets are versioned and hardware checks precede GPU recording. Every new
descriptor case needs a shared-plan test and a backend agreement test.
Device probes compare stable compute buffers, history rings and indirect
geometry against Rust. A deliberately wrong instance upload must fail the
same image comparator; a mismatched layout fingerprint must fail by name.
