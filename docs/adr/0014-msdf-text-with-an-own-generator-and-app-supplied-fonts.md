# 0014. MSDF text, generated in-tree, from fonts the application supplies

Date: 2026-09-09

Status: Accepted

## Context

The editor is drawn by `wxsl-render` (ADR 0013), so the renderer needs
text — a lot of it, at two very different scales: node titles and socket
labels that zoom with the canvas, and two monospaced code panels (the WXSL a
graph became, and the WGSL that compiled to) that scroll thousands of lines.

Grayscale glyph rasterization would mean re-rasterizing every glyph whenever
the canvas zoom changes, and re-uploading the atlas with it. A multi-channel
signed distance field is rasterized once per glyph and stays sharp at any
scale, including under a canvas transform, which is exactly the shape of this
problem. The cost is a generator: turning a glyph outline into an MSDF is
more than a rasterization.

Two questions followed: where the distance fields come from, and where the
font bytes come from.

## Decision

**MSDF generation lives in `wxsl-render::ui::msdf`, over
`ttf-parser`.** `ttf-parser` (pure Rust, no transitive dependencies,
`no_std`-capable) reads the font tables and hands us glyph outlines; we do
contour extraction, edge colouring and the per-channel pseudo-distance
ourselves. That is the only new third-party dependency the editor
introduces.

**The application supplies the fonts**, as bytes, exactly as it supplies the
shader library (ADR 0009). `wxsl-render` embeds no font and reads no file:
`ui::Font::from_bytes` takes what the caller loaded. The editor takes a UI
font and a monospaced font at construction; `crates/wxsl/examples/editor.rs`
finds platform defaults or takes `--font`/`--mono`.

Glyphs are rasterized on demand into a shared texture atlas at one fixed EM
size with a fixed distance range, and scaled in the shader — a distance field
is resolution-independent, so a second size is a second draw, not a second
atlas entry.

## Alternatives considered

- **The `fdsm` crate** (pure-Rust MSDF generation). A working implementation
  we would not have to own, but it pulls `nalgebra`, `image` and their
  trees into every editor build, for one function's worth of work whose
  algorithm is small and well documented. Owning ~400 testable lines is the
  better trade here, and it keeps the new-dependency count at one crate with
  zero transitive dependencies.
- **`msdfgen` (bindings to the original C++ library).** The reference
  implementation, but it makes a C++ toolchain a build requirement of the
  editor. Not acceptable for a Rust workspace that currently builds with
  nothing but cargo.
- **A pre-baked atlas embedded in the crate** (PNG + JSON from
  `msdf-atlas-gen`). Zero dependencies and zero runtime cost, but it freezes
  the font and the charset, needs an external C++ tool to regenerate, and
  puts binary blobs in the repo. A shader-graph editor whose labels come from
  user-authored node names cannot have a frozen charset.
- **Grayscale rasterization (`fontdue`, `ab_glyph`, `swash`).** Simpler and
  well supported, but re-rasterizes per size: the canvas zoom would
  invalidate the atlas continuously, which is the one thing this UI does
  constantly. It also loses the crisp corners MSDF exists to keep, most
  visibly on the code panels' box-drawing and punctuation.
- **Embedding an OFL font in the repo.** Would make the editor work with no
  configuration, at the cost of ~600KB of vendored binaries and a
  third-party licence file to carry in a workspace that is currently plain
  MIT/Apache-2.0 with no third-party assets at all. Deferred, not refused:
  if the default-font search proves annoying in practice, an
  `embedded-font` Cargo feature is the obvious follow-up, and nothing in the
  API needs to change for it.

## Consequences

- `wxsl-render` gains one dependency, `ttf-parser`. It is not optional:
  the crate's `wgpu` dependency is not optional either, and the `ui` module
  is not feature-gated (see ADR 0013 — the facade's features are the
  granularity that matters).
- The generator is ours to keep correct. It is tested without a GPU:
  generated fields are checked against a brute-force true signed distance at
  sample points, the median reconstruction is checked at corners, and a
  degenerate outline (empty glyph, single point, self-touching contour) must
  produce a field rather than a panic.
- An application that supplies no font gets an error at construction, not a
  window with invisible labels. The editor example's font search is
  platform-specific and deliberately dumb (a short list of well-known paths);
  when it fails, the error names `--font`.
- Complex text shaping is explicitly out of scope: layout is left-to-right,
  advance plus kerning pairs, with no bidi, no ligatures and no script
  itemization. Node labels, WGSL and file paths do not need it. If that
  changes, the seam is `ui::text`, which is the only module that turns a
  string into positioned glyphs.
