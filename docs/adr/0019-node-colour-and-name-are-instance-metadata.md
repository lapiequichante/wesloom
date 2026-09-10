# 0019. A node's colour and name belong to the node, not to its kind

Date: 2026-09-09

Status: Accepted, amended by [0023](0023-a-material-declares-its-resources.md)

## Context

The canvas coloured a node's title strip by its *category*, from a table in
`wxsl-editor`'s `Theme` — `math` blue, `color` purple, `lighting` amber, and
so on. [ADR 0004](0004-node-editor-is-an-optional-additive-ui-layer.md)'s
rule is that the editor derives what it draws from the interface a node
already describes and stores no display metadata on the core model, and
colouring by category obeyed that rule to the letter: the category is a
string the definition already carries.

It also spent the canvas's strongest visual channel on the one thing a reader
least needs it for. The category is written in the inspector and legible in
the id (`math.remap`, `color.luminance`); what a reader of a graph actually
wants marked is which part of it is the roughness and which part is the
emissive glow, and that is a property of *these* nodes, which nothing in the
node model knows. [ADR 0018](0018-one-generic-node-per-operation.md) made
this worse in passing: one generic node kind now stands in where four used
to, so a title reading "Add" says less than it did, and there are fewer
distinct categories on a canvas than before.

`Node::position` and `Node::label` were already on the core model for
exactly this kind of reason — a layout that does not survive a save is not a
layout — and ADR 0004 calls position "the one exception". So the question was
not whether display metadata may live on `Node` at all, but whether colour
earns the same exception, and what the default should be if it does.

## Decision

**`Node::color: Option<[f32; 3]>`**, serialized in the node format beside
`position` and `label`, and meaningless to codegen. It is per node
*instance*: two `math.add` nodes in one graph can be coloured differently,
which is the whole point — a colour marks what a part of a graph is *for*,
and only its author knows that.

**Every node is the same colour until one says otherwise.** `Theme::node_color`
replaces `Theme::category_color`, and the category table is gone. A default
that already varies leaves an author nothing to say with: if `math` nodes are
blue and `color` nodes purple, painting three nodes to group them competes
with a colouring that means something else. Uniform by default is what makes
a set colour read as deliberate.

**The name is editable in the inspector**, writing `Node::label`, with the
definition's own label as the starting text; clearing it or typing that same
label back drops the override. Renaming has always been possible in the
format and was not reachable from the editor.

**A colour is picked, not typed.** `widgets::color_picker` is a
saturation/value square over a hue strip (plus an alpha strip for a `vec4f`),
drawn as a grid of flat rectangles because that is what the interface
renderer has — no gradient primitive, and none added for this. Any swatch
opens it: the node-colour row in the inspector, and the swatch already shown
next to a `vec3f`/`vec4f` parameter that reads as a colour
(`widgets::looks_like_color`). It opens *underneath* the row rather than in a
popup, so it fits the inspector's scroll area, which computes its content
height up front; one is open at a time, since a picker is several rows tall.
The component fields stay: numbers are a poor way to pick a colour and the
only way to check one, and both edit the same value.

**A node's id is drawn top-right of its header.** Every diagnostic names a
node by id ("node #7 references …"), so being able to read it off the canvas
is what makes those messages actionable — and it is the one label a rename
cannot hide.

## Alternatives considered

- **Keep colouring by category, and add a per-node override on top.**
  Rejected: an override is only legible against a neutral default. The
  category colours would still be the loudest thing on the canvas, and an
  author's grouping would read as "these nodes are a category" rather than
  "these nodes belong together".
- **Store the colour in the editor, keyed by node id, outside the graph
  file.** Rejected for the reason `position` is in the file: a colouring that
  does not survive a save is not worth setting. Keeping it out of the format
  would also mean a second sidecar document to keep in step with the first.
- **A colour per *definition*, in the node library.** Rejected: it is the
  same information as the category with more places to set it, and it would
  put display metadata on `NodeDefinition`, which ADR 0004 keeps clean for a
  better reason than habit — an application supplying its own nodes should
  not have to have opinions about colour.
- **A popup colour picker over the canvas.** Rejected as the first thing to
  build: the immediate-mode layer has no overlay pass, so a popup would need
  a second draw phase and its own hit-test priority. Expanding the row costs
  nothing and reuses the scroll area that already exists.

## Consequences

- The node format gained one optional field. A document written before this
  ADR loads unchanged, and one written after it loads in an older build as
  an unknown field — `serde` ignores it by default, so nothing breaks either
  way.
- `crates/wxsl/assets/pbr_cube.wxsl.json` colours the two nodes that make
  the ember glow, as the worked example of what colour is *for*.
- `Theme::category_color` is gone. Nothing outside the canvas used it.
- `widgets::value_editor` no longer fits in a single row: its height depends
  on the socket's type (a matrix is a grid of fields) and on whether its
  picker is open, so callers ask `widgets::value_editor_height` first. The
  inspector's height pre-pass and its drawing pass therefore have to agree on
  the widget id — `app::param_widget_id` exists so that they cannot drift.
- The editor gained a "changed but not compiled" request flag: renaming a
  node or recolouring it marks the document modified without triggering a
  shader rebuild.


## Amendment (ADR 0023)

There is now a *third* kind of per-instance data, and it is the one this
ADR's rule does not cover: a **setting**
(`wxsl_core::node::SettingDef`) is a string a node instance carries that
changes what it compiles to. A `param.value`'s `name` is the uniform's
identity — rename it and the host writes a different field — so unlike a
label it is emphatically not metadata, and unlike a colour it is not free
to change.

The distinction this ADR draws still holds and is why the new thing needed
its own home rather than reusing `Node::label`: a label is what a *reader*
calls a node and codegen never sees it. See [0023](0023-a-material-declares-its-resources.md).
