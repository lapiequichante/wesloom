# 0043. The pipeline canvas is a mode, and it compiles on every edit

Date: 2026-09-27

Status: Accepted

Implements plan3's P5 — the editor's second canvas, deferred by plan2
until "P3/P4 give it something honest to show". The honest thing now
includes policies, feature channels, compute (ADR
[0041](0041-compute-and-buffers-join-the-document-vocabulary.md)) and
effect parameters (ADR
[0042](0042-effect-parameters-are-uniforms-the-descriptor-declares-them.md)).
Plan2's own guard rail named the shape: "a pipeline graph is a second
canvas over the same model, not a second editor." Plan3 kept one ADR
question open — whether the two-canvas state model wants one. It does,
and this is it.

## Context

The material preview has run the real
[`Renderer`](../../../crates/wxsl-render/src/renderer.rs) since ADR
[0013](0013-the-editor-draws-itself-with-wxsl-render.md) — the same
variant cache, the same pipelines a shipped application uses. A pipeline
document, meanwhile, compiles to a pass list through a pure, device-free
function (ADR [0033](0033-pipelines-are-documents.md)), and the renderer
accepts a pass list through `set_graph`. Put those three facts together
and the canvas's edit loop is already determined: validate, compile,
`set_graph` — "a pipeline edit is live by construction". What remained to
decide was the state around that loop: who owns the renderer, what a
compile failure does to the frame, what the palette's effect rows *are*,
and what the panels show instead of generated code.

## Decision

* **A mode, not an editor.** `Editor` holds both documents — the material
  graph and the pipeline document — and `CanvasMode` says which one the
  panels and shortcuts point at. The canvas component, the palette panel,
  the widgets and the immediate-mode layer are the material editor's own,
  reused as-is; the pipeline canvas has its own view, selection and
  picker state so switching does not lose any of them. The toolbar's
  canvas toggle is the biggest fact on screen; the stock-pipeline buttons
  disappear while the pipeline canvas is up, because there the document
  *is* the pipeline and a "forward" button over it would lie.
* **The compile target is the renderer the preview already runs.** An
  edit marks the document dirty; the next frame compiles it against
  `document_registry(renderer.effects())` and — deliberately — the
  renderer's *own* `PipelineConfig` (new accessor
  `Renderer::pipeline_config`), then installs the pass list with
  `set_graph`. Compiling against the config the renderer actually runs is
  what makes "what the preview shows" equal "what an application built
  from this document would run", rather than a second copy of the config
  drifting from the first. The same draw list renders through whatever
  pass list is installed, which is the whole point of a pipeline being
  data.
* **A failed compile keeps the last good pass list running.** The errors
  go to the problems tab, named by the document node at fault — the
  compiler's contract, and the material preview's rule (its last good
  material keeps rendering mid-edit) restated at pass level. The status
  bar colours on the active canvas's own compile state.
* **Leaving puts back what was there.** `Preview::restore_stock` restores
  the stock pipeline the material canvas chose. The two canvases share
  one renderer, so each puts back what it expects before the other's
  next frame; the material's compiled variants stay warm across the
  round trip.
* **The palette is the document registry plus one row per screen
  effect.** Compute effects are already registry rows — the derived
  `pass.compute.<effect>` definitions ADR 0041 added. A screen effect
  maps onto the fixed `pass.screen`, so its palette row is *palette-level
  sugar*: a `Match` carrying a preset setting, which places a
  `pass.screen` with `effect` already named. It is not a derived node
  definition, because a screen effect's wiring is the frame — the fixed
  socket set ADR 0034 kept — and a per-effect definition would either
  duplicate the pass or imply sockets the pass does not have. Searching
  and ranking are the palette's own `score`, so both sources order the
  same way.
* **The bottom panel lists the compiled passes.** A document's analogue
  of the material editor's generated text is a pass list, so the tabs are
  `passes | problems`, and the pass rows are read off the *running*
  renderer — label, kind, policy — not off the last successful compile.
  The inspector keeps the material inspector's settings rows (a document
  node's knobs are settings, and a setting edit is a recompile), adds the
  selected effect's declared parameters as live sliders — the face ADR
  [0042](0042-effect-parameters-are-uniforms-the-descriptor-declares-them.md)
  queued for this canvas, where a move is a buffer write and compiles
  nothing — and the one section that is about the pipeline rather than
  the selection: the G-buffer channel plan with each channel's source,
  and the attachment budget as a bar — plan2 P12's data, finally with a
  face.
* **The proof**: the GPU test drives the done-when end to end — the
  deferred preset opened in the canvas, the bloom edit made through the
  editor's own `pipeline_graph_mut` (a `pass.screen`, a resource for it
  to write, the display transform's input wire re-dropped), and the
  running pass list grows by exactly one pass labelled `bloom`; the frame
  still paints, and returning to the material canvas brings the stock
  pass list back. Device-free tests pin the same edit through the public
  compiler, the palette's rows, and the flattening of a broken edit's
  errors.

## Alternatives considered

* **A second `Renderer` for the pipeline preview.** Rejected: two variant
  caches, two resource pools, every pipeline compiled twice — and the
  property being sold would quietly become "live in a parallel universe".
  The shared renderer is also the cheaper one at swap time: the
  material's variants are the document's, so the first frame after an
  edit compiles only what the new pass list added.
* **A second editor instance** — two `Editor`s, one window. This is the
  "second editor" plan2's guard rail forbids, and mechanically worse: two
  UI passes and two glyph atlases for one screen, with the mode toggle
  rebuilt as instance switching.
* **Derived node definitions for screen effects** (the ADR 0041
  mechanism, applied again). Rejected for the reason above: compute
  effects needed derived sockets because their wiring is *their own*;
  screen effects' wiring is the frame's, and `pass.screen` already spells
  it. The preset-carrying palette row delivers the same convenience
  without a second spelling of the pass.
* **A "document" tab showing serialized JSON** beside the passes. The
  save flow already writes the node format; a JSON view would be a third
  rendering of data the file holds. The pass list is the one thing the
  bottom panel can show that nothing else does.
* **Background compilation**, the stock-pipeline swap machinery applied
  to document edits. Rejected at this size: the document compile is
  device-free and sub-millisecond at document scale, and it triggers no
  shader compilation until the first frame runs the new pass list — which
  is exactly what the existing swap exists to hide, for the cases that
  need it.

## Consequences

* `Renderer::pipeline_config()` is new public surface — the one value the
  canvas compiles against and the plan inspector reads. `Preview` grew
  the compile loop's renderer half (`install_document`, `restore_stock`)
  and the read side (`effects`, `render_graph`, `pipeline_config`);
  `pipeline_graph`/`pipeline_graph_mut`/`pipeline_status` are how an
  application — or a test — drives the document programmatically.
* The editor crate touches no facade code: the graph-authored effects
  (`wxsl::effects`) are the *application's* to register, exactly as the
  gallery's is — the example does it, and an editor built without them
  shows only the shipped descriptors under the same ids.
* Editing a document whose passes carry policies resets the runs —
  `set_graph` semantics, and the honest cost of a structural edit. N9's
  policy inspector is where that becomes visible; nothing today lies
  about it.
* The canvas has no way to author a *feature channel* into the config —
  features stay the application's `set_features` call until N9's features
  panel. The plan inspector shows what is set, which is most of what the
  panel would do anyway.
* The palette's `Match` grew the preset field; the editor's `Requests`
  grew a mode and a preset-carrying add. Neither is public API.
* If this changes — a second renderer, per-effect screen sockets, a
  third canvas — this ADR and plan2's guard rail are the two texts to
  supersede. `AGENTS.md`'s editor bullets and `docs/architecture.md`'s
  editor section say what is where today.
