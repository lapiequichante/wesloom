# Architecture Decision Records

An ADR captures one architecturally significant decision: the context that
forced it, what we decided, and what it costs. We keep them because this
project is built collaboratively with AI agents across many sessions with
no shared memory between them — the ADR log is how a decision made in one
session survives into the next one, instead of getting silently re-litigated
or reversed.

Numbers are sequential and never reused. Superseded ADRs stay in the repo
with an updated `Status` line pointing at whatever replaced them — history
is signal, don't delete it.

## Index

| # | Title | Status |
|---|---|---|
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-cargo-workspace-crate-boundaries.md) | Cargo workspace layout and crate boundaries | Accepted |
| [0003](0003-wesl-as-the-shading-language.md) | WESL as the shading language, `wesl`/`wesl-cli` as the compiler | Superseded by [0011](0011-own-the-shading-language.md) |
| [0004](0004-node-editor-is-an-optional-additive-ui-layer.md) | Visual node editor is an optional, additive UI layer | Accepted |
| [0005](0005-render-pipeline-abstraction-and-shader-switching.md) | Render pipeline abstraction with automatic forward/deferred shader switching | Accepted |
| [0006](0006-lygia-port-licensing-and-isolation.md) | LYGIA port: licensing and crate isolation | Superseded by [0007](0007-original-shader-stdlib-instead-of-a-lygia-port.md) |
| [0007](0007-original-shader-stdlib-instead-of-a-lygia-port.md) | Original shader standard library instead of a LYGIA port | Accepted |
| [0008](0008-surface-graphs-and-a-named-shader-abi.md) | A material graph describes a surface, against a named shader ABI | Accepted |
| [0009](0009-the-application-supplies-the-shader-library.md) | The application supplies the renderer's shader library | Accepted |
| [0010](0010-four-bind-groups-allocated-by-update-frequency.md) | Four bind groups, allocated by update frequency | Accepted |

To add one: copy `template.md` to `NNNN-short-title.md` (next number), fill
it in, add a row here. See `AGENTS.md` at the repo root for when an ADR is
warranted.
