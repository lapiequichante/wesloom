# 0013. The editor draws itself with `wxsl-render`

Date: 2026-09-09

Status: Accepted

Amends [0004](0004-node-editor-is-an-optional-additive-ui-layer.md), which
stands except for its claim that `wxsl-editor` depends only on
`wxsl-core` and needs no wgpu.

## Context

ADR 0004 settled *that* the editor is an optional additive layer, and left
*how it draws* open with a one-line note: "compile with `wxsl-render` at
the application level; this crate stays wgpu-free". Written out, that meant
the editor would describe its UI to some third-party GUI toolkit (egui was
the placeholder in `Cargo.toml`), and the material preview would be a
texture the toolkit was handed from outside.

Taking that path now costs more than it saves:

* **Two renderers in one window.** The editor's chrome would be drawn by a
  toolkit with its own pipelines, atlas, font stack and event model, while
  the thing being edited is drawn by ours. Every pixel-level concern — colour
  space, premultiplication, DPI scale, the preview's sRGB encoding — has to
  be reconciled across a boundary we do not own.
* **We already own the harder half.** A node canvas is rounded rectangles,
  capsules, bezier links, clipped scroll regions and a lot of text. Given a
  texture atlas and MSDF glyphs, that is a 2D draw list — and a 2D draw list
  is what `wxsl-render` is one shader away from being able to submit.
* **The UI is the best available test of the compiler.** If the editor's own
  chrome is drawn by a WXSL shader that goes through `wxsl-lang` and the
  variant cache like any material, then every editor frame exercises the
  compiler, the shader library seam (ADR 0009) and the bind-group budget
  (ADR 0010).
* **A GUI toolkit is a large, opinionated dependency** for a project whose
  reason to exist is owning this stack (ADR 0011 made the same call about
  the shading language).

## Decision

`wxsl-editor` depends on `wxsl-render` and draws itself through it.
Concretely:

* **`wxsl-render` grows a `ui` module**: a texture atlas, an MSDF font and
  text layout stack (ADR 0014), a 2D draw list of rounded boxes, capsules,
  textured quads and glyph runs, windowing-agnostic input event types, and a
  `UiRenderer` that submits one pass per frame. This is renderer
  functionality, usable without the editor.
* **The UI shader is WXSL in `wxsl-stdlib`**, named by `wxsl_core::abi`
  (`package::wxsl::ui`) and compiled through the same
  `variants::compile` path as a material. `wxsl-render` still ships no
  shaders (ADR 0009), and the UI pass binds only the pass group
  (`abi::GROUP_PASS`), which is what that slot is for (ADR 0010).
* **`wxsl-editor` owns interaction, not drawing**: an immediate-mode widget
  layer, the node canvas, the palette, the panels, and the offscreen material
  preview. It consumes `wxsl_render::ui` for output and
  `wxsl_render::Renderer` for the preview.
* **`wxsl-editor` stays windowing-agnostic.** It defines its own input
  event types (`wxsl_render::ui::input`) and never depends on winit; the
  application translates its own events and calls `Editor::handle_event`.
  The winit glue lives in `crates/wxsl/examples/editor.rs`, so the editor
  crate is embeddable in an application that already has an event loop.
* **`wxsl-core` is untouched.** Everything ADR 0004 says about the core
  graph model carrying no notion of how to draw itself still holds: node
  positions were already in the node format, and the editor derives every
  widget from a socket's *type*, never from editor metadata on a definition.

The dependency arrow directions ADR 0002 fixes are unchanged: the new edge is
`wxsl-editor → wxsl-render`, which points away from the editor, not
into it. Nothing may make `wxsl-core` or `wxsl-render` depend on
`wxsl-editor`.

## Alternatives considered

- **egui, as ADR 0004's `Cargo.toml` comment anticipated.** Rejected on the
  reconciliation cost above, plus the specific one that a node canvas in an
  immediate-mode toolkit means fighting its layout model for the one screen
  that matters most. The dependency is also not small: it and its atlas,
  font and glyph-cache stack would be the largest thing in an editor build.
- **Editor emits an abstract UI description; the application renders it.**
  Keeps `wxsl-editor` wgpu-free as ADR 0004 wanted, but only by moving the
  problem: every consumer then has to implement rounded boxes, clipping and
  text before seeing a single node, and the editor could not be tested
  end to end without a fake backend. The abstraction earns nothing while
  there is exactly one renderer.
- **Grow the UI layer inside `wxsl-editor` instead of `wxsl-render`.**
  Rejected because the layer is not editor-specific: an application
  embedding the renderer wants an atlas, text and a debug overlay whether or
  not it ships the editor, and putting it in the editor would deny it to
  them (or duplicate it). It also puts the wgpu code in the crate with the
  least reason to hold it.

## Consequences

- The facade's `editor` feature already implied `render`, so the feature
  matrix in `docs/architecture.md` needs no change — but the crate table's
  "Needs wgpu?" column for `wxsl-editor` becomes **yes**, and ADR 0004's
  claim to the contrary is superseded by this ADR. CI's headless check is
  unaffected: it asserts the *facade with no features* pulls in neither wgpu
  nor a GUI crate, which is still true.
- `wxsl-render` now has a public surface that has nothing to do with
  materials. It lives under one `ui` module so the split is legible, and the
  crate's doc comment says which half is which.
- The UI vertex layout is a host-shared contract, exactly like the mesh
  vertex format and the scene uniforms: `wxsl_core::abi`'s `UI_*` tables,
  `wxsl_render::ui`'s `#[repr(C)]` vertex, and
  `wxsl-stdlib/shaders/wxsl/ui.wxsl` are three halves of one thing and
  must be edited together. A test in `wxsl-stdlib` asserts the shader
  declares the bindings the ABI numbers, as it already does for the frame
  group.
- A GPU-less CI machine cannot test the editor's rendering. Everything that
  can be tested without a device is factored to be: MSDF generation, text
  layout, atlas packing, draw-list batching, canvas hit-testing and the
  interaction state machine are all pure functions over data.
