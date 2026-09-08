# Layout

Categories mirror the shape common to granular shader libraries (LYGIA
among them) because that shape works, not because this is a port of any of
them:

```
shaders/
  wesloom/      the shader ABI: what a generated material module is written
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
`lighting/pbr_direct_split.wesl`).

`filter/` and `sample/` are empty: both are mostly about sampling textures,
which the node model does not carry yet. They stay as directories so the
category layout is not a surprise later.

## `wesloom/` — the shader ABI

These are not library functions; they are the fixed vocabulary a generated
material module is compiled against, and the plumbing of the two render
paths. `wesloom_core::abi` names every item in them, and
[ADR 0008](../../../docs/adr/0008-surface-graphs-and-a-named-shader-abi.md)
explains the split.

| Module | What it holds |
|---|---|
| `bindings.wesl` | The frame bind group: camera/scene/object uniforms, light sampling. **Host-shared layout**: mirrored by `wesloom-render`'s `scene` module. |
| `surface.wesl` | `SurfaceContext` and `Surface`, the graph's input and output. |
| `vertex.wesl` | The vertex stage, shared by both paths, and the context builder. |
| `shading.wesl` | `shade_surface`: the lighting model. Called by *both* paths. |
| `deferred.wesl` | The G-buffer struct, and packing/unpacking it. |
| `lighting_pass.wesl` | The deferred lighting pass, compiled as its own root module. |

Editing any of these means editing `wesloom_core::abi` in the same change:
the struct field tables there are the Rust half of the same contract, in the
same order.

### Bind groups

`maxBindGroups` is 4, and all four are allocated up front by how often their
contents change ([ADR 0010](../../../docs/adr/0010-four-bind-groups-allocated-by-update-frequency.md)).
`wesloom_core::abi::BIND_GROUPS` is the Rust half of this table.

| # | Slot | Declared in | Holds |
|---|---|---|---|
| 0 | `frame` | `bindings.wesl` | Camera, scene, object transforms |
| 1 | `material` | *generated* | A graph's parameters and textures |
| 2 | `user` | — | Nothing here. The application's slot. |
| 3 | `pass` | `lighting_pass.wesl` | G-buffer, and future shadow/IBL resources |

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

**A function that reads a macro variable must declare it on its node.** The
macro's `const` declaration is generated per compilation into
`package::wesloom::macros`, and only for macros in the effective set — which
is built from the declarations of the nodes a graph uses. Declare it in
`registry.rs` (see `generative.fbm3`) or the import will not resolve.

**Guard the degenerate cases.** A zero-length vector, a zero-width range, a
zero-radius falloff: these arrive from a graph far more often than from
hand-written code, because a socket's default is frequently zero. Returning a
sensible value beats emitting a NaN that shows up as black pixels three nodes
downstream.
