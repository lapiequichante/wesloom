# 0041. Compute and buffers join the document vocabulary

Date: 2026-09-20

Status: Accepted

## Context

ADR 0035 gave the engine compute passes and ADR 0036 gave the scheduler
buffers — and both stopped at the engine's edge. A compute pass could not
be authored as data: the pipeline vocabulary had no `pass.compute`, and a
screen effect whose inputs included a buffer was a *named compile error*
("buffer inputs have no document socket yet; wire this effect into a
hand-built pass list"). The two proofs of the buffer machinery — the LUT
bake and the buffer ramp — lived in the gallery as hand-built pass lists,
the one corner of a frame that was still written in Rust.

ADR 0036 named the reason and plan3's N3 queued it: the document
vocabulary's socket sets are fixed, and a fixed socket set is the wrong
shape for a compute effect. `pass.screen` gets away with three sockets
because a screen effect's wiring *is* the frame — one G-buffer, one image,
one write, mapped onto them by kind. A compute effect's wiring is whatever
its work needs; an effect with two writes wants two sockets, and there is
no honest spelling of that on a three-socket node.

## Decision

### `resource.buffer`

A storage buffer is a document resource, beside `resource.color` and
`resource.depth`: a `bytes` setting (the size, positive or a named
`BadBufferSize` error), a `history` setting (transient, or a ring — the
same spelling `resource.color` uses), and a `buffer` output of the new
`ValueType::StorageBuffer` handle type, which joins the pipeline-resource
family (no zero, no splat, never a shader socket). It compiles to
`ResourceDesc::buffer`, and the scheduler's existing rules apply
unchanged: reads order passes, writes are compute-only bindings, and the
slot is never aliased.

### `pass.compute.<effect>`, sockets from the declaration

A compute effect joins the vocabulary as its own node definition,
`pass.compute.<effect id>`, derived from the effect's declaration —
[`EffectRegistry::node_defs`] — and registered beside the static table by
[`document_registry`], the registry documents are validated and compiled
against.

The derivation rule, and the reason it is safe:

* Each declared **input** becomes a mandatory input socket, named and
  typed by the declaration (`gbuffer` → the G-buffer handle, `image` → a
  colour target, a buffer → the storage-buffer handle). The graph model's
  typing is therefore the contract, enforced before the compiler runs —
  the mirror-direction checks `pass.screen` needs do not exist here,
  because a socket the effect did not declare is not on the node at all.
* Each declared **output** becomes a mandatory *input* socket too — the
  wire names the storage being written, exactly as `into` does on a
  screen pass. The resource is the thing other passes read: compute to
  screen through a buffer is one `resource.buffer` feeding two passes,
  the same shape as a colour chain through `resource.color`. A declared
  output never produces an edge, so no pass output is ever a virtual
  handle the way a screen pass's `color` output is — storage has a real
  resource by construction.
* The node carries the `policy` setting every pass node has; a
  non-default policy promotes its written resources to stable storage,
  the compiler derivation `pass.screen` already uses.

The sockets cannot be drawn any other way: a fixed socket set is precisely
what broke down. Changing a node's effect is changing the node — same as
every registry entry — and the palette shows the derived rows under the
`pass` category their id gives them.

### Screen effects gain one socket

A screen effect that reads storage gets its socket: `pass.screen` grows an
optional `buffer` input, and the compiler maps the effect's declared
buffer inputs onto it in declaration order, as it already maps G-buffer
and image inputs onto `gbuffer` and `image`. The socket set stays fixed,
so an effect declaring two buffer inputs is the named mistake — the error
says a screen pass has one `buffer` socket — and such an effect stays a
hand-built pass list until it wants N4's honest answer. Every shipped
screen effect fits.

### A size setting on `resource.color`

The LUT bake's target is 64×64, and no fraction of the window says that.
`resource.color` gains a `size` setting: `viewport` (the default — today's
behaviour, scaled by the existing `scale` setting) or fixed pixels as
`64x64`, compiled to `Extent::Fixed`. Anything else is the named error.

[`EffectRegistry::node_defs`]: ../../../crates/wxsl-render/src/effect.rs
[`document_registry`]: ../../../crates/wxsl-render/src/pipeline_doc.rs

## Alternatives considered

**A fixed socket set for `pass.compute`, mapped by kind like
`pass.screen`.** One `data`-ish input and one output cannot say whether
they are a buffer or a storage texture, and the second write breaks it —
the exact failure the plan named as the design question. The honest fix
was deriving the sockets, not abbreviating them.

**Instance-dependent sockets on one static `pass.compute` node, resolved
from the node's `effect` setting.** That is a graph-model feature —
per-node socket sets — bought for one node kind, rippling through
validation, serialization and the canvas. The registry is already the
mechanism for "nodes that differ": definitions are data, ids are strings,
and `pipeline::node_defs`'s own documentation invites applications to
register more. Deriving one definition per effect costs no new machinery
and keeps the graph model's "a definition's sockets are its sockets"
invariant intact.

**Outputs as producing sockets** (wire the compute pass's `ramp` output
into the reader's `buffer` input). Then the compute node's output is a
virtual stand-in for a resource, as a screen pass's `color` output is —
but a screen pass's output must be virtual, because its target may be the
frame's, which no document node declared. Storage is always declared. Two
spellings of one wire — through the pass or through the resource — would
be two spellings of one fact, so the producing socket loses.

**Leave the proofs hand-built and ship only the vocabulary.** The tests
would then assert that documents compile to hand-built lists that no
document needs — and the gallery's comment ("hand-built, because a compute
pass has no document node yet") would be a lie. Both proofs are documents,
and the parity tests compare them against the hand-built spelling kept as
the reference.

## Consequences

* The buffer-ramp and brdf-lut gallery demos are documents; every demo in
  the gallery compiles through `compile_pipeline`, and nothing in a frame
  is hand-built Rust any more. The GPU test
  (`the_buffer_ramp_compiles_from_a_document_and_reaches_the_screen`)
  renders the ramp from its document.
* ADR 0036's "the vocabulary cannot type a compute effect's output socket"
  alternative is answered, not just revisited: the typing comes from the
  declaration, one derived definition at a time.
* The compile-time gates — `--no-default-features`, `--features editor`,
  the `document_registry` superset rule — cover the new rows
  automatically, because the derivation lives behind the
  [`EffectRegistry`] the compiler already takes.
* An effect registered after a document was written is an `UnknownEffect`
  naming the effect id at compile — the document outlived the
  registration that made it, and the error says so by name.
* N4 (effect parameters) and the two-image question are now the *only*
  things standing between every shipped effect and a document spelling:
  bloom stays a descriptor until an effect can declare what the pipeline
  wires into it, which is the same socket-derivation question this ADR
  answered for compute, one level up.
* If this changes, also update `docs/architecture.md`'s pipeline
  vocabulary table and `plan3.md`'s N3 entry.
