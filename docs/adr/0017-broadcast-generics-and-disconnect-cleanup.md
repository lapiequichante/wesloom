# 0017. Broadcast generic parameters, and forgetting a resolution on disconnect

Date: 2026-09-09

Status: Accepted

## Context

ADR 0015 gave every genericized arithmetic family (`math.add`, `math.multiply`,
…) one generic parameter `T`, shared by every one of its sockets. That is
correct for `add`, `subtract`, `divide`, `modulo`, `power`, `minimum` and
`maximum` — WGSL requires both operands of those to already be the same type —
but it is *wrong* for `multiply`: WGSL's `*` alone among them also accepts a
scalar against a vector, in either order (`f32 * vec3f` and `vec3f * f32` both
give `vec3f`). A single shared `T` cannot express that `math.multiply`'s two
operands might legitimately be different types — connecting a `vec3f` to `a`
forced `T` to `vec3f`, so `b` (sharing the same `T`) then refused an `f32`
that WGSL itself would have accepted just fine.

Separately, resolving a generic parameter had no way back out. Nothing ever
cleared a node's `Node::generics` entry once set, including when every edge
that had ever touched it was disconnected — so a node that had resolved `T`
to `f32` (say, by an edge later removed) stayed pinned to `f32` forever,
refusing a `vec3f` on the next connection even though nothing was connected
to justify that refusal. Both problems were reported together against
`math.multiply` specifically, but the second is a defect in the mechanism
ADR 0015 built, not in that one family's node definition.

## Decision

**Broadcast sockets.** [`Socket`] gains `broadcast: Option<Box<(WxslIdent,
WxslIdent)>>` (via `Socket::broadcast(a, b)`), mutually exclusive with
`Socket::generic`. Where a generic socket's effective type *is* one resolved
parameter, a broadcast socket's effective type is
[`ValueType::broadcast`] of two — WGSL's own rule for `*`: equal types
combine to that type, `f32` paired with any other float type combines to
that other type, and two different non-scalar types do not combine at all.
`math.multiply` now declares two independent parameters, `A` and `B` (one per
operand, each still allowing every float type), and its output broadcasts
them, instead of sharing one `T` across `a`, `b` and `out`. `Graph::validate`
gains `GraphError::IncompatibleGenerics` for a broadcast socket whose two
parameters are both resolved but do not combine (`vec2f` and `vec3f`, say) —
the derived-type analogue of `TypeMismatch`, checked once both parameters
have something to compare rather than at every socket that happens to
reference them. `Graph::effective_type` (now `pub`, not `pub(crate)` —
canvas port colours and inspector labels need the same answer for a
broadcast socket that codegen does) and `Graph::set_generic`'s
mismatched-edge scan both learned about `.broadcast` alongside `.generic`,
so repinning one of `multiply`'s two operands disconnects a downstream wire
the new combination no longer fits, exactly as repinning an ordinary shared
`T` already did.

`Graph::connect` needed one robustness fix to make room for this: its
adoption logic previously assumed an *unresolved* socket always names a
single generic parameter to adopt into (`.expect("an unresolved type implies
a generic socket")`). A broadcast socket can be unresolved with no such
parameter at all — its type is derived, not stored — so that assumption no
longer holds; connecting a not-yet-resolved broadcast socket to a concrete
one now falls back to the same "nothing to adopt, but the edge is still
valid" behaviour already used when *neither* side is resolved. Nothing
retroactively re-checks that edge if the broadcast later resolves to
something incompatible — but `Graph::validate`'s existing `check_edge` pass
re-derives every edge's types fresh on every call, so a graph that becomes
inconsistent this way is still caught, just by the ordinary `TypeMismatch`/
`IncompatibleGenerics` checks rather than a special case in `connect`.

**Forgetting a stale resolution.** `Graph::disconnect` and `Graph::remove_node`
now take a `&NodeRegistry` (previously they needed none) and, after removing
an edge, check whether either node it touched still has *any* remaining
edge touching a socket that shares the same generic parameter; if not, that
parameter's resolution is removed from `Node::generics`, exactly undoing
what `Graph::connect`'s adoption would do on the way back in. This is
deliberately **local, not transitive**: two generic nodes wired together and
anchored through a third, concrete one keep whatever they resolved to if the
edge removed is between the two generic nodes themselves (each still touches
the other) — even though neither is connected to the concrete anchor once
that specific edge is gone. A full transitive re-derivation would need a
reachability search over the remaining graph on every disconnect; the local
rule fixes the reported case (a node with no connections left at all,
stuck on a stale type) without paying for that.

## Alternatives considered

- **Give `multiply` its old per-type registry entries back**
  (`math.multiply.f32`/`.vec2f`/`.vec3f`/`.vec4f`), keeping ADR 0015's
  single-`T` shape for everything that *is* generic. Rejected: it solves the
  reported bug by undoing the feature that motivated ADR 0015 for exactly the
  one family where the flexibility matters most — `f32 * vec3f` (tinting a
  colour by a scalar, scaling a vector by a factor) is one of the most common
  things a multiply node is *for*.
- **One generic parameter, with `allowed` widened to also permit `f32`
  against a specific vector width.** Rejected: `GenericParam::allowed` is a
  flat list of types a single parameter may become; there is no way to say
  "this parameter is `f32` *or* whatever the other socket's parameter is,"
  which is the actual relationship. Two independent parameters plus a
  derived combine rule says exactly that; a single parameter with a special
  case bolted on says it awkwardly, if at all.
- **Re-derive every generic resolution from scratch after every edge
  change**, rather than incrementally forgetting only what a specific
  removal invalidates. Rejected as the transitive case above already
  explains: correct in the fullest sense, but a strictly bigger algorithm
  for a case nobody reported, paid on every disconnect rather than only when
  it matters.

## Consequences

- `divide` and `modulo` also broadcast in WGSL, but **one-directionally**
  (`vecN / f32` is valid; `f32 / vecN` is not) — a different rule from
  `multiply`'s symmetric one. Not implemented here: `ValueType::broadcast`
  is deliberately the one symmetric rule `multiply` needs, not a
  general-purpose "every way WGSL ever broadcasts" mechanism speculatively
  built ahead of a second caller. A one-directional variant is a small,
  separate addition whenever `divide`/`modulo` actually need it.
- `Graph::disconnect` and `Graph::remove_node` both changed signature (a new
  `&NodeRegistry` parameter) — every caller in the editor and the test suite
  needed updating. Both were already the only two ways an edge disappears,
  so this is the only place the forgetting rule needed to be taught.
- `NodeBody::Call`'s `WxslFunction` is now boxed (`Call(Box<WxslFunction>)`):
  `Socket` grew (the new `broadcast` field) enough to trip clippy's
  `large_enum_variant` lint on `NodeBody` by way of `WxslFunction`'s
  `FunctionReturn::Value(Socket)`. Boxing the already-larger-than-everything-
  else `Call` payload was the smaller, more local fix than boxing
  `broadcast` itself, which would have pushed `Box`/`.as_deref()` through
  every place `Socket::broadcast` is read.
