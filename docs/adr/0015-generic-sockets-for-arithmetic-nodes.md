# 0015. Generic sockets, resolved per node instance, for arithmetic nodes

Date: 2026-09-09

Status: Accepted

## Context

Before this change, `wxsl-stdlib`'s arithmetic operators (`add`, `subtract`,
`multiply`, `divide`, `modulo`, `power`, `minimum`, `maximum`, and eighteen
unary functions — `negate`, `absolute`, `sqrt`, the trig functions, and so on)
were generated once per concrete float type: `math.add.f32`,
`math.add.vec2f`, `math.add.vec3f`, `math.add.vec4f`, and the same four-way
split for every other family. That was already factored at the Rust level
(one table, one generation loop over `ValueType::FLOATS`), so there was no
copy-pasted *code* — but the *registry* still carried four separate node
kinds per family, and so did the palette a user searches: typing "add" found
four rows, each usable for exactly one type, none of them able to change.

The underlying operation never needed that split. `{a} + {b}` is exactly the
same WXSL for `f32` and `vec4f` — WGSL's arithmetic operators are already
defined component-wise for every numeric vector — so the only thing that
differed between the four `math.add.*` node kinds was the *type annotation*
on otherwise identical sockets. Splitting on it was paying a registry entry,
a palette row and a maintenance burden for a distinction the emitted code
never needed.

## Decision

`wxsl-core::node` gains **generic sockets**: a [`NodeDefinition`] can declare
one or more [`GenericParam`]s (a name plus a fixed set of allowed
[`ValueType`]s), and a [`Socket`] can reference one via `Socket::generic`
instead of carrying a single fixed type. `wxsl-stdlib`'s arithmetic
families now register *one* node per family (`math.add`, not
`math.add.f32`/`.vec2f`/`.vec3f`/`.vec4f`), each declaring one parameter `T:
f32 | vec2f | vec3f | vec4f` shared by every one of its sockets.

Resolution is per graph *node instance*, stored in
[`Graph`]'s `Node::generics: BTreeMap<String, ValueType>` — never on the
shared `NodeDefinition`, which every instance of a kind refers to. The
graph's two invariants (exact-type matching, no implicit conversion) are
unchanged; they now apply to a socket's *resolved* type rather than always
to a type fixed at definition time:

- **`Graph::connect` adopts a type the first time it can.** If one side of a
  new edge is an unresolved generic socket and the other is concretely typed
  (or an already-resolved generic), the connection succeeds and resolves the
  unresolved side to match — this is what makes a socket "adapt to whatever
  it receives." If both sides already disagree on a concrete type, the
  connection is rejected exactly as a mismatch between two ordinary
  concretely-typed sockets always was. If *neither* side is resolved yet
  (two fresh generic nodes wired together), the edge is still valid and both
  stay polymorphic until something anchors the chain — connecting a third,
  concretely-typed node to either one propagates through the existing edge
  and resolves both, not just the one directly touched.
- **An unresolved generic parameter makes its node invalid**, reported by
  `Graph::validate` as `GraphError::UnresolvedGeneric` — the graph-level
  analogue of `GraphError::MissingInput` for a mandatory socket nothing
  feeds. Generic sockets have no static default (`Socket::default` stays
  `None`; a fixed `Value` would be the wrong type for every resolution but
  one), so a fresh, disconnected generic node needs its type picked, either
  by wiring or explicitly, before it means anything.
- **`Graph::set_generic`** resolves a parameter without a wire — for a
  node nothing is connected to yet, or to change what an already-resolved
  one is. Repinning **disconnects** any edge that no longer fits, rather
  than rejecting the repin: changing what a node *is* has always
  invalidated wiring that no longer matches (switching between
  `math.add.f32` and `math.add.vec3f` was never otherwise possible), and
  this is the same behaviour surfacing through one node kind instead of
  several.

Every place that used to read a socket's declared `ty` unconditionally now
has to ask instead "is this socket generic, and if so, what has this
instance resolved it to": `wxsl-core::codegen` (the `let` binding's type
annotation), the editor's canvas (a port's colour) and inspector (the
socket's displayed type, and a new "pick a type" row per declared
parameter, with buttons for each allowed type). `wxsl-core::graph`'s
`Graph::effective_type` (`pub(crate)`) and `Graph::generic_type` (public,
for the editor) are the two functions that answer it.

## Alternatives considered

- **Keep the per-type split, and merge it only at the editor/palette
  level** (present `math.add.f32`/`.vec2f`/`.vec3f`/`.vec4f` as one
  searchable row that places whichever concrete kind fits on drop, and lets
  a node's underlying kind be switched later). Rejected: it would have
  delivered the same *visible* behaviour with far less change to
  `wxsl-core`, but it does not touch the actual thing asked for — "sockets
  that adapt to the input they receive and become invalid if not the right
  combination" describes graph-level typing, not a palette convenience. It
  would also have left the four-entries-per-family duplication in place,
  merely hidden behind an editor-side lookup table.
- **Auto-infer transitively through every existing edge on every
  resolution**, not just the one node whose parameter just got fixed
  (a full constraint-propagation pass over the whole graph). Implemented in
  bounded form instead — `resolve_generic` walks the connected component of
  edges that share a generic parameter from the node that was just resolved,
  which covers "wire a chain first, anchor it after" — but a change to one
  end of an *already fully resolved* subgraph does not re-walk anything
  beyond what actually shares the changed parameter. A full solver was not
  worth building for eight binary and eighteen unary node families whose
  sockets all share a single parameter each.
- **Give a generic socket a default anyway**, splatting the family's
  identity value (0 or 1) into whatever type ends up resolved. Rejected:
  nothing on `wxsl-core`'s side knows the resolved type at the point a
  `Socket::default` would need to be fixed (build time, on a shared
  definition), and teaching the core graph model about "arithmetic
  identities" to make this work would be exactly the kind of
  domain-specific metadata ADR 0004 keeps off the core node model. Requiring
  an explicit connection or pin for the first release accepts a real,
  documented loss of convenience (a fresh `math.add` needs both operands fed
  before it validates, where `math.add.f32` used to default both to 0)
  in exchange for a core model that stays general.

## Consequences

- Only the `Expr`-bodied arithmetic families (`BINARY`/`UNARY` in
  `wxsl-stdlib::registry`) are genericized in this pass. Everything else —
  `clamp`, `mix`, `step`, `smoothstep`, `inverse_lerp`, `remap`, `wrap`,
  `vector.*`, and every `NodeBody::Call` node — is unchanged, still one
  concrete node per float type. Some of those (`clamp`, `mix`, …) have
  sockets that are not interchangeable operands, so a single shared
  parameter would not mean the same thing on every socket; genericizing them
  is a separate, unstarted piece of work, not a rejected one.
- `wxsl-stdlib`'s registry shrank from 226 node definitions to 139: the
  eight binary and eighteen eligible unary families went from four registry
  entries each to one, net of the (unchanged) `clamp`/`mix`/etc. families.
- The node format gained one optional field, `Node.generics: BTreeMap<String,
  ValueType>`, serialized only when non-empty. A document written before
  this ADR deserializes unchanged (no generic nodes existed to reference
  it); a document that names a now-generic id (`math.add.f32`) needs
  updating to the generic id plus an explicit `generics` entry, since a
  whole-document deserialize never goes through `Graph::connect`'s
  adoption logic — see `crates/wxsl/assets/pbr_cube.wxsl.json`'s comment
  block for a worked example.
- `crates/wxsl/tests/graph_to_wgsl.rs`'s "every node in the library
  compiles" coverage test now resolves a generic node to every type its
  parameter allows (not just its placeholder), so genericizing a family
  does not quietly lose per-type coverage.
