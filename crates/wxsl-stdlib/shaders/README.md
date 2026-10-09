# Layout

Categories mirror the shape common to granular shader libraries (LYGIA
among them) because that shape works, not because this is a port of any of
them:

```
shaders/
  wxsl/      the shader ABI: what a generated material module is written
                against (see below) — not granular functions
  animation/
  color/
  distort/
  filter/
  generative/
  lighting/
  math/
  sample/
  sdf/
  space/
```

One function per file, named after the function, so a module path reads
`package::lighting::pbr_direct::pbr_direct`. Where a function has a struct
return or one tightly-coupled variant, both live in that file (see
`lighting/pbr_direct_split.wxsl`).

That rule is now load-bearing rather than tidy: **the file is the node**
([ADR 0020](../../../docs/adr/0020-a-node-definition-is-derived-from-its-wxsl-source.md)).
`wxsl_lang::node_from_source` reads a file and answers with the node
definition, and `build.rs` runs it over everything under a category
directory, so there is no Rust to write for a new function. See "The shape of
a node source" below for what the derivation reads.

`filter/` holds what samples a *neighbourhood* of a texture rather than a
point: `fxaa` is the first, and it arrived with the screen domain
([ADR 0040](../../../docs/adr/0040-screen-domain-graphs-postprocess-is-a-material-over-the-frame.md)).
These are ordinary functions over a `texture_2d<f32>`, so nothing stops a
material calling one — what the screen domain adds is where the frame's own
image comes from.

`sample/` holds the texture readers. Their signatures name
`texture_2d<f32>` and `sampler`, which the derivation maps to socket types
like any other — a texture is a value in WGSL, so a node can take one
([ADR 0023](../../../docs/adr/0023-a-material-declares-its-resources.md)).
The *texture itself* is declared by a `texture.*` node in the registry, not
by a file here: there is no function to write for "a texture the
application binds".

## `wxsl/` — the shader ABI

These are not library functions; they are the fixed vocabulary a generated
material module is compiled against, and the plumbing of the two render
paths. `wxsl_core::abi` names every item in them, and
[ADR 0008](../../../docs/adr/0008-surface-graphs-and-a-named-shader-abi.md)
explains the split.

| Module | What it holds |
|---|---|
| `bindings.wxsl` | The frame bind group: camera and scene uniforms, the instance transform storage buffer, light sampling. Fixed layouts are generated from `wxsl-core::host` (ADR 0050). |
| `surface.wxsl` | `SurfaceContext` and `Surface`, a material graph's input and output. |
| `screen.wxsl` | `ScreenContext` and the one image a screen effect reads — the screen domain's ABI (ADR 0040). |
| `vertex.wxsl` | The vertex stage, shared by every material stage, and the context builder. |
| `lighting models/` | the lighting-model functions (ADR 0028): shaded through `wxsl_core::lighting`'s registry, not placed as nodes. |

Editing any of these means editing `wxsl_core::abi` in the same change:
the struct field tables there are the Rust half of the same contract, in the
same order.

### Bind groups

`maxBindGroups` is 4, and all four are allocated up front by how often their
contents change ([ADR 0010](../../../docs/adr/0010-four-bind-groups-allocated-by-update-frequency.md)).
`wxsl_core::abi::BIND_GROUPS` is the Rust half of this table.

| # | Slot | Declared in | Holds |
|---|---|---|---|
| 0 | `frame` | `bindings.wxsl`, and *generated* at binding 3 | Camera, scene, the instance transform storage buffer, and a second array of whatever per-instance attributes a material declares |
| 1 | `material` | *generated* | A graph's uniform parameters, textures and samplers |
| 2 | `user` | *generated* | The application's slot. A graph may declare the block it expects there; nothing in this crate binds it |
| 3 | `pass` | `lighting models/` | the lighting-model functions (ADR 0028): shaded through `wxsl_core::lighting`'s registry, not placed as nodes. |

Groups 1 and 2 say *generated* because they have no fixed layout: a
material's graph decides them, and `wxsl_core::resources` computes the
result ([ADR 0023](../../../docs/adr/0023-a-material-declares-its-resources.md)).
There is nothing to write here for either.

Binding 3 of the frame group is the same story in the middle of a group
that is otherwise fixed: the transform array at binding 2 stays exactly
what `Instance` in `bindings.wxsl` says, and a material's own per-instance
attributes are a *separate* array at binding 3, indexed by the same
`@builtin(instance_index)`
([ADR 0024](../../../docs/adr/0024-a-material-declares-the-geometry-it-requires.md)).
Widening `Instance` would give binding 2 two strides, and
`transform_vertex` reads it at this one. Do not add fields to it.

Nothing in this crate may bind `@group(2)`, and only pass plumbing may bind
`@group(3)`: a library function must compile for every material stage, and group
3 is occupied in deferred. A test in `src/shaders.rs` enforces both.

## Authoring rules

**Original code and audited permissive ports are allowed** (ADR 0053).
MIT, Apache-2.0 and BSD-2/3-Clause are the initial approved source licenses.
Check the exact file and its provenance chain, not only a repository badge.
Other licenses require review before copying; unclear/non-permissive sources
remain technique-only. Research checkouts under ignored `refs/` are not shipped.

A port retains upstream copyright/license text and applicable notices. Put
SPDX license, source repository/path, full commit, original symbol and a changes
line after the node's label/documentation paragraph. Add it to root `NOTICE`
and this crate's packaged `NOTICE`, including complete required license text.
`wxsl_stdlib::THIRD_PARTY_NOTICES` exposes the packaged notices to applications;
compiled WGSL alone does not carry them. Review package SPDX metadata whenever
another upstream license is introduced. A technique-only implementation should
say so rather than falsely claiming to be a port.

Worked example: `lighting/sheen_ibl_response.wxsl`. Keep ports granular and
WXSL-native; do not copy foreign bindings, preprocessor stacks or engine state.

**A new function needs no node definition — it *is* one.** Write the file,
add it to `MODULES` in `src/shaders.rs` (a test fails if you forget), and it
appears in the palette. What the derivation reads is below; anything it
cannot read is a diagnostic naming the line, at build time.

**Polymorphism over value types is one generic node, not a family.**
A node definition declares a `GenericParam` and its sockets carry it, so one
node kind covers every type the parameter allows and resolves per instance
from whatever is connected ([ADR 0015](../../../docs/adr/0015-generic-sockets-for-arithmetic-nodes.md),
[ADR 0018](../../../docs/adr/0018-one-generic-node-per-operation.md)). Two
patterns for the body, and the choice is not about taste:

* **An expression** — no `.wxsl` file at all. Right whenever the body is one
  expression, which is more often than it looks: a scalar guard like
  `if abs(span) < 1e-8 { … }` does not generalize — for the vector types
  that comparison is a `vec<bool>` and `if` rejects it — but rewriting it as
  `select` both fixes that and collapses the body to an expression.
  `math.remap`, `math.inverse_lerp` and `math.wrap` are built this way;
  `registry.rs`'s `guarded` helper is the reshaping. Where the expression has
  to *name* its type — a `select` fallback of the right width, a constructor
  — `{$T}` expands to the resolved WGSL spelling.
* **A templated WXSL function.** `fn f<T: f32 | vec2f | vec3f | vec4f>`,
  which codegen calls as `f<vec3f>(…)` with the type argument written out,
  instantiated per type by the compiler
  ([ADR 0012](../../../docs/adr/0012-monomorphize-templates-on-the-flat-module.md)).
  Right when the body is more than one expression but is genuinely the same
  code at every width — `math/safe_normalize.wxsl` and
  `math/smootherstep.wxsl` are the two. `components(T)` is available inside
  it as an integer literal — the number of scalar components — for the cases
  that need the width. Note that `@if` cannot test `components(T)`:
  conditional translation runs before instantiation, and `cond` says so if
  you try.

There is no third pattern. One WXSL function per type, named
`<name>_<wxsl_type>`, is what `safe_normalize` used to be, and a template
replaced it: a reduction's scalar guard (`dot(v, v)` is a scalar however wide
`v` is) generalizes as unchanged inside a template as it did inside four
copies. If a body ever genuinely differs by width, `@if` on a macro is the
tool, not a copy per type.

`select(false_value, true_value, cond)` takes a vector `cond` and chooses
per component, which is usually the semantics you want anyway: one
degenerate component should collapse only itself.

**A function that reads a macro variable declares it in its own file.**
`@macro const WXSL_FBM_OCTAVES: i32 = 5;` is both the declaration and the
default, so the module compiles on its own, and the derivation carries it
onto the node — which is what puts the macro in a graph's effective set, so
the graph and a single node instance can override it. Its trailing comment is
the description the editor shows next to the control. A macro used in an
`@if` but never declared is an error naming the file and the line — not a
silent `false`.

**Guard the degenerate cases.** A zero-length vector, a zero-width range, a
zero-radius falloff: these arrive from a graph far more often than from
hand-written code, because a socket's default is frequently zero. Returning a
sensible value beats emitting a NaN that shows up as black pixels three nodes
downstream.

## The shape of a node source

Everything a node needs is in the file. `math/smootherstep.wxsl`, in full
except for the body:

```wxsl
// Smootherstep
//
// Quintic ramp between two edges, with a continuous second derivative — no
// crease where the ramp meets the flat parts.
//
// Worth the extra multiply wherever the first derivative's discontinuity
// shows up as a visible crease — noise interpolation, or a value driving a
// normal.

fn smootherstep<T: f32 | vec2f | vec3f | vec4f>(
    edge0: T, // @default 0.0
    edge1: T, // @default 1.0
    x: T,     // @default 0.5
) -> T {
```

| What | Where it comes from |
|---|---|
| the id `math.smootherstep` and its category | the file's place in the tree |
| the label `Smootherstep` | the **first line** of the leading comment block |
| the documentation | the **paragraph after it**, and only that paragraph |
| a type parameter's allowed set | its bound list, verbatim |
| input sockets | the parameters, in order |
| output sockets | the return type — one named `out`, or one per field of a struct declared in the same file |
| socket docs and defaults | each parameter's own trailing comment |
| macro declarations | `@macro const`, with its trailing comment as the doc |

Four things to get right:

* **A label line, then a blank comment line, then one paragraph.** Anything
  after that paragraph is for whoever is reading the source — how the body
  works, which paper the technique is from, why a constant is what it is —
  and never reaches the editor. Put implementation notes there, not in the
  first paragraph.
* **One parameter per line, each with its own `@default`.** A comment after
  the second parameter on a shared line has nothing to say which parameter it
  belongs to, so the derivation ignores it and the socket comes out
  mandatory. Write the doc first and the annotation last:
  `lacunarity: f32, // Frequency multiplier per octave. @default 2.0`.
* **`@default` takes one number or the type's components.** One number
  spreads (`@default 0.5` on a `vec3f` is `vec3f(0.5)`, on a `mat3x3f` its
  diagonal), which is the only form a socket carrying a type parameter can
  have — its type is not known until an instance resolves it. Several are
  components in order: `@default 0.0, 1.0, 0.0`. No annotation at all means
  the input is mandatory, which is right for a socket with no sensible
  fallback and wrong for most.
* **A misspelled annotation is an error, not a shrug.** `@defualt` fails the
  build naming the line, because a silently-ignored default leaves a socket
  mandatory for no visible reason.

The ABI files under `wxsl/` are exempt from all of this: they have entry
points and several functions each, and none of them is a node.
