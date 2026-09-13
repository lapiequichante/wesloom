# 0036. Buffers are graph resources

Date: 2026-09-13

Status: Accepted

Implements plan2's P11. The scheduler reasons about everything a frame
touches; until now the buffers were invisible to it.

## Context

`ResourceDesc` was texture-shaped, and the buffers a frame uses — the
draw queue, the frame group's uniforms — were implicit ABI infrastructure
the scheduler could not see. request.md's resource list (storage buffer,
storage texture, uniform data) wants buffers *declared*, so that
dependency reasoning covers them the way it covers attachments: a pass
that writes a buffer orders the passes that read it, and the pool
allocates it. P11 was deliberately ordered after P3 so the document
vocabulary would model buffers once rather than fork them in later.

## Decision

* **A shape on the descriptor**: `ResourceDesc { label, shape,
  persistence, imported }`, with `ResourceShape::{Texture, Buffer}` —
  the texture fields move inside the first arm; a buffer is `size` bytes
  plus extra usages. Every existing constructor and builder
  (`ResourceDesc::color`, `with_extent`, `with_usage`, `persistent`, …)
  keeps its signature, so a graph that compiles today compiles
  unchanged; `ResourceDesc::buffer(label, size)` is the new row.
* **The scheduler treats a buffer as a resource whose aliasing rule is
  "never"**: it participates in ordering (a read of a buffer is an edge,
  exactly like a texture read), it gets a slot with `STORAGE` usage
  inferred, and it never shares that slot — even with a same-sized
  buffer whose lifetime does not overlap. A buffer aliasing bug corrupts
  a whole block rather than a frame region, and no workload has asked
  for the memory back yet; the rule is refinable later, as the plan
  says.
* **The pass group binds them**: `PassBinding` became an enum —
  `Texture`, `StorageTexture` (ADR 0035's write bindings), `Buffer {
  read_only, size }`. Reads bind read-only storage (visible to
  vertex/fragment/compute), writes read-write (compute only). The
  binding order is unchanged — reads, then writes — so the effect
  contract covers buffer-consuming effects with no new rule: a buffer
  input is `EffectInputKind::Buffer`, the shader declares
  `var<storage>` at the matching binding, and the pool's
  `ResourcePool::buffer(slot)` is the read-back escape hatch beside
  `texture()`.
* **Attachments stay textures**: a buffer in a colour or depth slot is
  the named error `GraphError::AttachmentNotATexture`, not a bind-group
  complaint three layers down.
* **The proof ships beside the LUT's**: `RAMP_FILL` (compute, fills a
  256-entry storage buffer with an eased ramp) and `RAMP_VIEW` (screen,
  reads it as storage and draws one value per column) — the smallest
  complete example of compute → buffer → fragment with no texture in
  between, registered by an application with `add_effect` like any
  effect.

## Alternatives

* **A sibling `BufferDesc` and a second graph list** — two resource
  spaces that must agree about ids, ordering and lifetimes. Rejected:
  the shape enum keeps one id space and one scheduler.
* **Transient buffer aliasing** (reuse a dead buffer's allocation) —
  the memory win is real but the debugging cost is block-shaped; "never"
  is the honest first rule and revisiting it is a measured change, not
  an urgent one.
* **Uniform buffers as a distinct kind** — a uniform is a *binding
  kind*, and the pass group binds storage; a uniform-consuming effect
  wants effect parameters (ADR 0034's deferred piece), not a second
  buffer flavour. The frame group's own uniforms stay ABI infrastructure
  — declaring them in the graph would let a document reshape the ABI,
  which the pipeline vocabulary is deliberately not allowed to do (see
  `wxsl_core::pipeline`'s module notes).
* **`pass.compute` document node now** — the vocabulary cannot type a
  compute effect's output socket without choosing buffer-or-texture per
  node, and the two proof effects are hand-built-graph demos anyway.
  The engine side is complete; the document node waits for its first
  document-shaped consumer, the same rule P12 and P7 state.

## Consequences

* Ordering, allocation and binding for buffers are the scheduler's, not
  the application's: a compute pass that fills a buffer and a screen
  pass that reads it schedule from their declared reads, and device-free
  tests pin the edge, the never-alias rule and the named attachment
  error.
* `ResourceDesc`/`SlotDesc` field accesses became shape matches — about
  ten sites, all compiler-checked, which is the cost of the enum being
  honest.
* Buffer read-back (a test asserting the ramp's values) wants
  `COPY_SRC` via `with_buffer_usage` plus `ResourcePool::buffer`; the
  GPU proof instead *draws* the buffer and asserts the picture's
  direction, which needs no new API.
* The editor's pipeline canvas shows buffers as resources for free once
  it renders this vocabulary; nothing in the document compiler had to
  change to keep that possible.
