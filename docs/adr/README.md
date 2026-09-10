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
| [0004](0004-node-editor-is-an-optional-additive-ui-layer.md) | Visual node editor is an optional, additive UI layer | Accepted, amended by [0013](0013-the-editor-draws-itself-with-wxsl-render.md), [0019](0019-node-colour-and-name-are-instance-metadata.md) |
| [0005](0005-render-pipeline-abstraction-and-shader-switching.md) | Render pipeline abstraction with automatic forward/deferred shader switching | Accepted, amended by [0021](0021-a-declarative-render-graph-and-a-scene-document.md), [0022](0022-material-stages-replace-the-render-path-enum.md) |
| [0006](0006-lygia-port-licensing-and-isolation.md) | LYGIA port: licensing and crate isolation | Superseded by [0007](0007-original-shader-stdlib-instead-of-a-lygia-port.md) |
| [0007](0007-original-shader-stdlib-instead-of-a-lygia-port.md) | Original shader standard library instead of a LYGIA port | Accepted |
| [0008](0008-surface-graphs-and-a-named-shader-abi.md) | A material graph describes a surface, against a named shader ABI | Accepted, amended by [0020](0020-a-node-definition-is-derived-from-its-wxsl-source.md), [0021](0021-a-declarative-render-graph-and-a-scene-document.md) |
| [0009](0009-the-application-supplies-the-shader-library.md) | The application supplies the renderer's shader library | Accepted |
| [0010](0010-four-bind-groups-allocated-by-update-frequency.md) | Four bind groups, allocated by update frequency | Accepted, amended by [0021](0021-a-declarative-render-graph-and-a-scene-document.md) |
| [0011](0011-own-the-shading-language.md) | Own the shading language: WXSL replaces WESL | Accepted |
| [0012](0012-monomorphize-templates-on-the-flat-module.md) | Monomorphize templates on the flat module, with shallow inference | Accepted |
| [0013](0013-the-editor-draws-itself-with-wxsl-render.md) | The editor draws itself with `wxsl-render` | Accepted |
| [0014](0014-msdf-text-with-an-own-generator-and-app-supplied-fonts.md) | MSDF text, generated in-tree, from fonts the application supplies | Accepted |
| [0015](0015-generic-sockets-for-arithmetic-nodes.md) | Generic sockets, resolved per node instance, for arithmetic nodes | Accepted, amended by [0018](0018-one-generic-node-per-operation.md) |
| [0016](0016-syntax-highlighting-reuses-wxsl-langs-lexer.md) | The editor's code-panel syntax highlighting reuses `wxsl-lang`'s lexer | Accepted |
| [0017](0017-broadcast-generics-and-disconnect-cleanup.md) | Broadcast generic parameters, and forgetting a resolution on disconnect | Accepted, amended by [0018](0018-one-generic-node-per-operation.md) |
| [0018](0018-one-generic-node-per-operation.md) | One generic node per operation, with WGSL's own operand rules | Accepted |
| [0019](0019-node-colour-and-name-are-instance-metadata.md) | A node's colour and name belong to the node, not to its kind | Accepted |
| [0020](0020-a-node-definition-is-derived-from-its-wxsl-source.md) | A node definition is derived from its WXSL source | Accepted |
| [0021](0021-a-declarative-render-graph-and-a-scene-document.md) | A declarative render graph, and a scene document the pipeline is not in | Accepted, amended by [0022](0022-material-stages-replace-the-render-path-enum.md) |
| [0022](0022-material-stages-replace-the-render-path-enum.md) | Material stages replace the render-path enum | Accepted |

To add one: copy `template.md` to `NNNN-short-title.md` (next number), fill
it in, add a row here. See `AGENTS.md` at the repo root for when an ADR is
warranted.
