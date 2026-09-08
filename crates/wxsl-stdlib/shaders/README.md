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
  filter/       (empty)
  generative/
  lighting/
  math/
  sample/       (empty)
  sdf/
  space/
```

One function per file, named after the function, so a module path reads
`package::lighting::pbr_direct::pbr_direct`. Where a function has a struct
return or one tightly-coupled variant, both live in that file (see
`lighting/pbr_direct_split.wxsl`).

`filter/` and `sample/` are empty: both are mostly about sampling textures,
which the node model does not carry yet. They stay as directories so the
category layout is not a surprise later.

## `wxsl/` — the shader ABI

These are not library functions; they are the fixed vocabulary a generated
material module is compiled against, and the plumbing of the two render
paths. `wxsl_core::abi` names every item in them, and
[ADR 0008](../../../docs/adr/0008-surface-graphs-and-a-named-shader-abi.md)
explains the split.

| Module | What it holds |
|---|---|
| `bindings.wxsl` | The frame bind group: camera/scene/object uniforms, light sampling. **Host-shared layout**: mirrored by `wxsl-render`'s `scene` module. |
| `surface.wxsl` | `SurfaceContext` and `Surface`, the graph's input and output. |
| `vertex.wxsl` | The vertex stage, shared by both paths, and the context builder. |
| `shading.wxsl` | `shade_surface`: the lighting model. Called by *both* paths. |
| `deferred.wxsl` | The G-buffer struct, and packing/unpacking it. |
| `lighting_pass.wxsl` | The deferred lighting pass, compiled as its own root module. |

Editing any of these means editing `wxsl_core::abi` in the same change:
the struct field tables there are the Rust half of the same contract, in the
same order.

### Bind groups

`maxBindGroups` is 4, and all four are allocated up front by how often their
contents change ([ADR 0010](../../../docs/adr/0010-four-bind-groups-allocated-by-update-frequency.md)).
`wxsl_core::abi::BIND_GROUPS` is the Rust half of this table.

| # | Slot | Declared in | Holds |
|---|---|---|---|
| 0 | `frame` | `bindings.wxsl` | Camera, scene, object transforms |
| 1 | `material` | *generated* | A graph's parameters and textures |
| 2 | `user` | — | Nothing here. The application's slot. |
| 3 | `pass` | `lighting_pass.wxsl` | G-buffer, and future shadow/IBL resources |

Nothing in this crate may bind `@group(2)`, and only pass plumbing may bind
`@group(3)`: a library function must compile in either render path, and group
3 is occupied in deferred. A test in `src/shaders.rs` enforces both.

## Authoring rules

**Every function in this crate is original code.** It's fine — encouraged,
even — to look at how other libraries and engines solve a problem
(LYGIA's category breakdown, [Babylon.js](https://github.com/BabylonJS/Babylon.js)'s
shader techniques, papers, blog posts) for the *idea*: what the function
should do, what a fast approximation looks like, what edge cases matter.
It is not fine to transcribe or lightly rename someone else's
implementation — that's a derivative work regardless of the source
license, and defeats the point of writing this from scratch (see
`docs/adr/0007-original-shader-stdlib-instead-of-a-lygia-port.md`). If a
function was written with a specific external reference in mind for the
*technique* (not the code), a one-line comment naming the reference is
good practice; it is not a substitute for the implementation being your
own.

**A new function needs a node definition.** `src/registry.rs` describes each
function's module, name, parameters and return shape; that descriptor is what
lets a graph type-check a call to it. A function with no descriptor is
unreachable from a graph, and `src/shaders.rs`'s test will fail if the file
is not listed in `MODULES` at all.

**Polymorphism over value types is expanded by `registry.rs`.**
A node definition needs concrete socket types — a graph type-checks against
them — so the `for ty in ValueType::FLOATS` loop is the expansion mechanism
whichever way the function itself is written. Three patterns, and the choice
is not about taste:

* **Expression family** — no `.wxsl` file at all. The loop builds the
  expression per type with `ty.wxsl_type()`, so there is exactly one source
  of truth. Right whenever the body is one expression, which is more often
  than it looks: a scalar guard like `if abs(span) < 1e-8 { … }` does not
  generalize — for the vector types that comparison is a `vec<bool>` and
  `if` rejects it — but rewriting it as `select` both fixes that and
  collapses the body to an expression. `math.remap`, `math.inverse_lerp` and
  `math.wrap` are built this way; `registry.rs`'s `guarded` helper is the
  reshaping.
* **One templated WXSL function.** `fn f<T: f32 | vec2f | vec3f | vec4f>`,
  called as `f<vec3f>(…)` from the loop, instantiated per type by the
  compiler ([ADR 0012](../../../docs/adr/0012-monomorphize-templates-on-the-flat-module.md)).
  Right when the body is more than one expression but is genuinely the same
  code at every width. `components(T)` is available inside it as an integer
  literal — the number of scalar components — for the cases that need the
  width. Note that `@if` cannot test `components(T)`: conditional
  translation runs before instantiation, and `cond` says so if you try.
* **One WXSL function per type**, named `<name>_<wxsl_type>`. Right when the
  bodies genuinely differ — typically a *reduction*, where a scalar guard is
  legitimate because the reduced value is scalar however wide the input is.
  `math/safe_normalize.wxsl` is the example. Write the suffix in exactly one
  place — the loop in `registry.rs` — and the existing test that every
  called function exists will catch a typo.

One node per type is still one node per type in all three: the sockets are
concrete, so the ids stay `math.remap.f32`, `math.remap.vec3f` and so on.
Collapsing a family to a single node with a type-variable socket resolved by
connection is the follow-up ADR 0012 names, not something templates alone
buy.

`select(false_value, true_value, cond)` takes a vector `cond` and chooses
per component, which is usually the semantics you want anyway: one
degenerate component should collapse only itself.

**A function that reads a macro variable must declare it in its own file,
and on its node.** In the file: `@macro const WXSL_FBM_OCTAVES: i32 = 5;`,
which is both the declaration and the default, so the module compiles on its
own. On the node in `registry.rs` (see `generative.fbm3`): that is what puts
the macro in a graph's effective set, so the graph and a single node instance
can override it. A macro used in an `@if` but never declared is an error
naming the file and the line — not a silent `false`.

**Guard the degenerate cases.** A zero-length vector, a zero-width range, a
zero-radius falloff: these arrive from a graph far more often than from
hand-written code, because a socket's default is frequently zero. Returning a
sensible value beats emitting a NaN that shows up as black pixels three nodes
downstream.
