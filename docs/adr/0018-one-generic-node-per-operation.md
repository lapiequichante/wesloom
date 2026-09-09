# 0018. One generic node per operation, with WGSL's own operand rules

Date: 2026-09-09

Status: Accepted

## Context

[ADR 0015](0015-generic-sockets-for-arithmetic-nodes.md) gave the arithmetic
families one generic parameter `T` shared by every socket, collapsing
`math.add.f32`/`.vec2f`/`.vec3f`/`.vec4f` into one `math.add`. It stopped
there on purpose, and said so: `clamp`, `mix`, `step`, `smoothstep`,
`inverse_lerp`, `remap`, `wrap`, everything under `vector.`, `logic.select`,
`const.*`, `convert.splat` and every `NodeBody::Call` node stayed one
concrete node per type. [ADR 0017](0017-broadcast-generics-and-disconnect-cleanup.md)
then added a second shape for `math.multiply` alone, whose two operands WGSL
lets differ.

Both left the same thing unfinished. The registry still carried families that
differ only in a type annotation — `math.clamp.f32` and `math.clamp.vec3f`
emit *the same* `clamp({x}, {low}, {high})` — so searching the palette for
"clamp" still found four rows, each usable at exactly one type and unable to
change. And the rule ADR 0017 introduced for `multiply` was, on inspection,
not special to `multiply` at all: `f32 + vec3f`, `vec3f / f32` and
`vec3f % f32` are all valid WGSL, so `add`, `subtract`, `divide` and `modulo`
were forcing their operands to match for no reason but the shape of the node
definition. `multiply` in turn was still missing the half of `*` that does
linear algebra — `mat3x3f * vec3f`, the whole point of having matrices — and
`vector.transform.mat3` existed as a separate node precisely because
`multiply` could not express it.

Three things blocked finishing the job, and each needed a decision:

**Which pairs of types actually combine.** Guessing wrong produces a graph
that validates and a shader the driver rejects, which is the one failure mode
no amount of graph-level testing catches.

**Naming the resolved type inside a template.** `inverse_lerp`'s guard is
`select({$T}(0.0), …)` and `convert.splat`'s body is `{$T}({value})`: both
have to *write* the type, which an expression template with no notion of the
resolved type cannot.

**A default for a socket whose type is not known yet.** `clamp`'s `high`
defaulted to 1, `mix`'s `b` to 1, `step`'s `edge` to 0.5. ADR 0015 gave
generic sockets no default at all (a fixed `Value` is the wrong type for
every resolution but one), which was survivable for `add`'s two
interchangeable operands and would not have been for these.

## Decision

**Two type rules, taken from WGSL and checked against it.**
[`ValueType::componentwise`] is WGSL's typing of `+`, `-`, `/` and `%` — equal
types combine to that type, and an `f32` spreads over a float vector's
components in either order. [`ValueType::product`] is WGSL's typing of `*` —
everything the first allows, plus a scalar against a matrix, a matrix against
another of the same shape, and a matrix against a vector of its own size
(`mat3x3f * vec3f` is `vec3f`), again in either order. `Socket::broadcast`
from ADR 0017 becomes `Socket::combine(rule, a, b)`, naming which rule
derives the socket's type from two parameters, and
`GraphError::IncompatibleGenerics` now carries the rule so its message says
what was wanted.

`crates/wxsl/tests/wgsl_types.rs` compares both rules against `wgpu`'s own
validator for **every** pair of types, and separately checks the claim behind
the families that do *not* combine — `pow`, `min`, `max` and `atan2` have a
single `(T, T) -> T` overload, so those share one parameter. That test is the
authority the rules answer to; the tables were in fact written from its
output.

**One node per operation, everywhere it is possible.** Every family that was
one node per type is now one node: `math.clamp`, `math.mix`, `math.step`,
`math.smoothstep`, `math.inverse_lerp`, `math.remap`, `math.wrap`,
`math.arctangent2`, `math.smootherstep`, `math.safe_normalize`,
`vector.dot`, `vector.length`, `vector.distance`, `vector.normalize`,
`vector.reflect`, `vector.refract`, `vector.transform`, `logic.select`,
`const.value`, `convert.splat`. `math.add`, `math.subtract`, `math.divide`
and `math.modulo` join `math.multiply` in declaring two parameters with a
derived result; `add`, `subtract` and `multiply` accept matrices as well.
The registry went from 139 definitions to 100, and no id names a type any
more except where the type genuinely is the node (see the consequences).

**`{$T}` in an expression template** expands to the resolved WGSL spelling of
generic parameter `T` (`crates/wxsl-core/src/codegen.rs`), which is what lets
`convert.splat` and the range operators' `select` guard be one template. The
node builder checks at construction that every `{$name}` names a declared
parameter, the same way it already checked `Socket::generic`.

**A generic socket's default is a scalar to spread**
(`Socket::splat_default`, read through `Socket::default_for`): "1.0, whatever
width that turns out to be" is well defined at every resolution where a fixed
`Value` is not. `Socket::with_splat_default` stores the scalar on a generic
socket and resolves it to a `Value` immediately on a concrete one, so callers
do not distinguish. `ValueType::splat` extends to matrices as the *diagonal*
— `mat3x3f(v)` is not WGSL and a matrix of all `v` is not a useful value of
anything, whereas `splat(1.0)` being the identity is exactly what a matrix
socket wants as a default.

**A `NodeBody::Call` node can be generic**, calling a WXSL *template*
([ADR 0012](0012-monomorphize-templates-on-the-flat-module.md)) with the type
arguments written out — the case that ADR says never has to be inferred,
because the graph knows every socket's type exactly.
`math/safe_normalize.wxsl` and `math/smootherstep.wxsl` are now one template
each, instead of one body per width and a scalar-only body respectively.

**A resolution is a default until wiring backs it.** Two independent
parameters exist so that `f32 * vec3f` is *expressible*, not because
differing operands are the common case, so resolving one parameter seeds its
companion across a `Socket::combine` socket to the same type
(`Graph::seedable_companions`) — wiring one operand leaves the node complete
rather than half-typed. A node is *placed* resolved too
(`NodeDefinition::default_generics`, `Graph::add_resolved`): the first of
each parameter's allowed types, and a pinned value on every input that has
no default of its own, because an unresolved parameter is an error and a
socket with no known type has nothing to show for a value — a node
complaining before it has been used is worse than a node holding a guess.
The order of every `GenericParam::allowed` is load-bearing for that reason,
and so is choosing sets whose *first* entries combine: `vector.transform`'s
`V` starts at `vec3f` and excludes `vec2f` outright, because `mat3x3f *
vec2f` is nothing at all. A test places one node of every kind and validates
the lot.

Guesses like those are only safe because **connecting retypes rather than
refuses**. `Graph::plan_retype` asks what change to a node's parameters would
make the socket being wired carry the type on offer — set the one parameter
it names, or whichever of the two a combined socket derives from, taking the
smallest change that works — and applies it if it breaks nothing: every
parameter must allow the type, every combined socket must still derive one,
and every edge already touching the node must still join two sockets of the
same type. That last condition is the line between adapting and vandalising.
Plugging an `f32` into a socket showing `vec3f` retypes the node; doing it
where the node's output is already wired to something that really is a
`vec3f` is still `GraphError::TypeMismatch`, not a wire silently dropped.
Retyping also carries the pinned values across (`Value::converted_to`,
`Graph::retype_params`), so picking `vec3f` on a `math.add` whose operands
were `0.5` leaves them at `vec3f(0.5)` instead of reporting a type mismatch
against the value the author typed.

Seeding, defaulting and retyping are three halves of one rule: the most
recent, most specific statement of intent wins, which is the same principle
behind `Graph::set_generic` disconnecting edges that no longer fit rather
than refusing the repin. `Graph::disconnect`/`remove_node` close the loop
(`Graph::refresh_generics`): forget every parameter no remaining edge has a
say in, re-seed the companions of whatever survived, and fall back to the
default for anything left — so unplugging `multiply`'s output does not blank
the operand still wired to it, and unplugging a node completely returns it to
the type it was placed with rather than to none at all.

## Alternatives considered

- **One rule with a matrix flag on it**, instead of `Componentwise` and
  `Product` as separate rules, since `/` and `%` reject matrices where `+`
  and `-` accept two of the same shape. Rejected: that difference is not
  about how two types combine, it is about which types are operands at all,
  which `GenericParam::allowed` already says. `Binary::matrices` in the
  registry table carries it, and the rules stay two functions that each
  describe one of WGSL's two typings.
- **Give `mix`'s `t` its own parameter**, so a component-wise `t` is
  expressible alongside the scalar one WGSL also accepts. Rejected: valid at
  every type the node allows, `mix(vecN, vecN, f32)` is not a per-type split
  and needs no parameter — while the combination it would additionally have
  to reject (`mix(f32, f32, vec3f)`) needs a third, asymmetric rule, and the
  node would carry a second type picker to buy the rarer of the two
  behaviours. A fixed `f32` says the useful thing exactly.
- **Keep `vector.transform` as `mat3` only, or drop it now that
  `math.multiply` subsumes it.** Rejected both ways: a palette entry called
  "Transform by matrix", documented as the way to apply a
  `space.tangent_basis` frame, is worth more than the operator it expands to
  — and as one generic node over `M` and `V` it costs one registry entry, not
  a family. `mat3x3f` with a `vec4f` is reported rather than accepted,
  because `Product` says so.
- **Add `vecN<bool>` to `ValueType`** so the comparison operators could be
  generic too. Rejected as out of scope here: it is a new family of value
  types touching serialization, the editor's value editors and port colours,
  for six nodes that are honestly scalar today. `compare.*` keeps a scalar
  `f32` pair, and loses only the `.f32` suffix that implied a family.
- **Re-derive every resolution from the whole graph after any edge change**,
  rather than the local forget-and-reseed. Rejected for the reason ADR 0017
  already gave: a reachability search on every disconnect, to fix a case
  nobody has reported.

## Consequences

- **Ids changed**, and a graph written before this ADR needs updating:
  `math.remap.f32` → `math.remap` plus `"generics": {"T": "f32"}`,
  `const.vec3f` → `const.value`, `convert.splat.vec3f` → `convert.splat`,
  `compare.less.f32` → `compare.less`, `vector.cross.vec3f` →
  `vector.cross`, and so on. `crates/wxsl/assets/pbr_cube.wxsl.json` is the
  worked example, and its comment block explains the `generics` field. A
  whole-document deserialize never goes through `Graph::connect`, so every
  generic node in a file spells its resolution out.
- **Two families keep a type in their id**, and a test pins that list down:
  `convert.combine.*` and `convert.split.*`, whose *socket count* is part of
  the type (a `vec4f` split has four outputs) — a `GenericParam` resolves a
  socket's type, not a node's socket list. `const.bool` also keeps its name:
  `bool` has no float components, so it is in no parameter's allowed set and
  a splat default has nothing to build there.
- **`math.add` and friends now show two type pickers** in the inspector, one
  per parameter, and up to six candidate types each. The picker wraps onto as
  many rows as the labels need rather than squeezing `mat3x3f` into a sixth
  of a panel.
- **Matrix parameters became editable** (`widgets::matrix_editor`, a grid of
  drag fields, one grid row per matrix row), which `const.value` at
  `mat3x3f`/`mat4x4f` made necessary rather than merely nice. The inspector
  sizes an input's row from what the socket resolved to.
- **Every node shows its id on the canvas**, top-right of the header. Not
  forced by this change, but every diagnostic names a node by id ("node #7
  references …"), and one generic node kind standing in for four makes the
  label alone a weaker identifier than it was.
- `divide` and `modulo` accept a scalar against a vector in *either* order,
  which WGSL does too — the one-directional restriction ADR 0017 recorded as
  a deliberate omission turned out not to exist. `wgsl_types.rs` is where
  that was settled.
- `generative/value_noise3.wxsl` writes `smootherstep<f32>(…)` now that
  `smootherstep` is a template: its arguments are an unsuffixed literal and a
  swizzle, exactly the cases ADR 0012 declines to guess from.
- `math.negate` stays float-only even though the rest of `+`/`-`/`*` accept
  matrices: `-mat3x3f` is not WGSL, which `wgsl_types.rs` checks alongside the two
  binary rules and the registry test pins on the definitions.
- Amends ADR 0015 (which scoped itself to the `Expr`-bodied arithmetic
  families) and ADR 0017 (whose `Socket::broadcast` is this ADR's
  `Socket::combine` with `TypeRule::Product`, and whose `divide`/`modulo`
  consequence is superseded).
