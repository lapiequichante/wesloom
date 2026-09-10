# 23. A material declares its resources: uniforms, textures, and the application's slot

Date: 2026-09-10

Status: Accepted

Amends [0008](0008-surface-graphs-and-a-named-shader-abi.md),
[0010](0010-four-bind-groups-allocated-by-update-frequency.md),
[0019](0019-node-colour-and-name-are-instance-metadata.md)

## Context

A material graph could compute and nothing else. Every value it used came
from the surface context, from a literal inlined into the generated WXSL,
or from a macro variable — and each of those is *baked*, so moving a
slider recompiled a shader. `abi::GROUP_MATERIAL` was allocated in ADR
0010 and had stayed empty ever since, because nothing had a reason to bind
into it. Nothing could sample a texture at all: `ValueType` carried nine
WGSL value types and none of them was a handle.

That is the gap under every remaining milestone. Shadows need a texture
array, postprocess needs the frame it is filtering, bakes need somewhere
to put the bake and somewhere to read it back from, and an application
embedding this needs a way to hand a material its own data without the
renderer having to know what that data *is*.

So the milestone is one idea in three parts: **a graph does not only
compute, it declares what it needs from outside itself.**

1. Values the host changes without a recompile — a slider.
2. Textures and samplers the host binds.
3. A block the *application* fills, which the material only describes.

The third is different in kind from the first two and worth naming: the
material does not own that resource. It states a shape and hands out a
layout; somebody else provides the data.

One problem runs through all of it. A uniform buffer's field offsets are
decided by the graph, so there is no `#[repr(C)]` struct to mirror them —
which is how every other host-shared layout in this repo is kept honest
(ADR 0008: two halves, written twice, checked by a test on the sizes).
And the offsets are not obvious: WGSL aligns a `vec3f` to 16 bytes while
it occupies 12, so `f32` then `vec3f` puts the vector at byte 16 and not
at byte 4, and a `mat3x3f` is three columns each padded to 16 rather than
nine contiguous floats. A hand-written mirror gets those wrong silently,
every frame.

## Decision

**`wxsl_core::resources` computes the layout, and it is the only thing
that knows it.** `BufferLayout::uniform` takes names and types and answers
with offsets; the shader's struct is *generated from it* and the host
writes *through it*. There is no second half to disagree, which is the
deliberate exception to ADR 0008's mirroring rule rather than an oversight
of it. Fields are ordered by alignment, widest first, then by name — so
the order is a property of the declarations rather than of the walk that
found them, and the `f32`-then-`vec3f` case costs nothing instead of
wasting twelve bytes. `bool` has no representation in WGSL's uniform
address space at all, so a boolean parameter is stored as a `u32` and
compared against zero where it is read; `BufferLayout::read_expr` is the
one place that knows.

**`MaterialInterface` is what a graph declares**, computed over the same
reachable set codegen emits from — a parked branch declares nothing,
exactly as it emits nothing. It holds the parameter layout, the textures
and samplers with their binding indices, and the application's block. It
is carried on `GeneratedShader` rather than re-derived by the renderer,
because two derivations are two things to disagree.

**Three node bodies declare, and each carries a `SettingDef`.** A setting
is a string-valued property of a node *instance* that changes what it
compiles to: `param.value`'s `name` is the uniform's identity, and
`texture.texture_2d`'s is what the host binds it by. This is a new kind of
per-instance data, distinct from ADR 0019's colour and label, which are
metadata a reader chooses and codegen never sees. A `param.value`'s
default sits on a socket marked `Socket::constant` — a value the instance
pins and no edge may reach, because the default is read on the CPU before
any shader runs.

**The resource types live in `ValueType`, and not in `ValueType::ALL`.** A
texture and a sampler are values in WGSL — handles, passed to
`textureSample` and to functions — so an edge can carry one and
`sample.texture_2d` is an ordinary derived node whose first two parameters
happen to be a texture and a sampler. But nobody types one in: there is no
`Value` for a texture, no zero, no splat, no arithmetic. Keeping them in
the enum means a socket's type stays one enum everywhere — the editor, the
graph typing, the serialized format — and keeping them out of `ALL` means
nothing that iterates "every type a value can have" ever offers one.

**Group 1's contents are per material, so its layout is too.** Binding 0
is always the parameter buffer, reserved even when a graph declares none,
so that adding a first parameter cannot renumber the textures above it;
resources take the indices upward in name order.
`bindings::BindingLayouts` caches a `BindGroupLayout` per interface
*signature*, and the pipeline and pipeline-layout caches key on that same
signature — a **shape**, never a value. Two materials with the same
parameters at different settings share one pipeline, which is what makes a
slider free.

**Group 2 stays the application's; the material only describes it.** A
graph declares `Graph::user_block` — a named block with typed fields, in
full — and codegen emits the struct and the `@group(2) @binding(0)`
declaration. `Renderer::user_layout` hands out the `BindGroupLayout`, the
application hands back a `BindGroup` on the draw, and `wgpu` validates that
the two agree. There is no checking code of ours to keep in sync, and a
mismatch is a `wgpu` error naming the binding rather than a wrong picture.
This amends ADR 0010's "nothing of ours in group 2": the slot's *ownership*
is unchanged; what is new is that a material can say what it expects there.

**Both groups arrive on the draw.** `DrawItem::bindings` carries group 1
and `DrawItem::user` carries group 2, because the resources behind them
belong to the application — the same rule that keeps the renderer
scene-graph-free, and the shape that lets two objects share a material with
different textures. A draw whose material declares a group it did not carry
is an error naming the material and the group.

## Alternatives considered

**A `#[repr(C)]` mirror, as everywhere else.** There is nothing to write:
the fields are whatever the graph declares. A generic "parameter block" of
sixteen `vec4f`s with the graph indexing into it would restore the mirror,
at the cost of wasting most of the buffer and making the generated WGSL
unreadable.

**`@align`/`@size` attributes on the generated struct.** Would state the
offsets a second time, in a second notation, and give them somewhere to
disagree. Instead the field *order* is chosen so that WGSL's own layout
rules put every field exactly where the computed layout says, and a GPU
test at every `ValueType` checks that they do.

**Declaration order, or graph order, for the fields.** Both are stable
enough, and both waste space on the exact case that motivated this
(`vec3f` next to a scalar). Alignment-descending costs nothing and reads
no worse in a struct nobody writes by hand.

**A parallel `SocketType` enum for textures and samplers.** Honest about
what they are, and it forks the editor's port colours, the graph's typing
and the serialized node format — three places — to express a distinction
that `ValueType::is_resource` expresses in one line.

**Inferring the application's block from the fields a graph reads.** The
application already has a struct with a layout of its own. A block
inferred from the two fields this graph happens to read would put them at
the wrong offsets, silently. The graph states the block in full or it does
not state it at all.

**`MaterialBindings` owned by the `Material`.** Tempting, and wrong for the
same reason meshes are not: `Material` is pure data with no device in it,
and two objects sharing a material with different textures is ordinary.

**A texture named by the sampling node, with no texture socket.** Smaller —
no resource types, no edges carrying handles — and it loses one texture
feeding two samples at different UVs, and the resource-typed edges the
pipeline graph will want.

## Consequences

`GROUP_MATERIAL` is finally bound, and everything downstream can now ask
for a texture: shadows, screen effects, bakes, peel buffers. The layout
computer has a second customer waiting in M4's widened instance buffer,
which is why the ordering rule and the write path are a table rather than a
struct.

A parameter is now the cheap knob and a `const` is the expensive one, and
the difference is visible: dragging a parameter through eight values leaves
`cache_stats()` and `pipeline_count()` untouched, which
`changing_a_parameter_costs_a_buffer_write_and_not_a_variant` asserts.

`DrawItem` gained two fields, so every caller building a draw list by hand
is affected — the demo, the tests, `wxsl::scene::SceneResources`, the
editor's preview. `SceneResources` gained `create_bindings`/`upload`
around the gap where the application supplies a texture, because a scene
document still has no way to name an image; that is M9's business, and
until then a scene material declaring a texture says so by name.

The editor can select a texture node and rename it, and cannot yet give it
an image: the preview binds a magenta checker to anything a graph declares.
That is a visible placeholder rather than a silent black surface, and it is
the honest state of things until a texture is something a document can
name.

The offsets are checked the only way that is really convincing — on a GPU,
against literals the shader compiler inlined, at every `ValueType`
including `mat3x3f` and a `bool` — in
`crates/wxsl/tests/material_resources.rs`. If the layout rules ever change,
that file is the one that notices.

If this changes, also update `wxsl_core::abi`'s binding constants,
`docs/architecture.md`'s bind-group table, and
`crates/wxsl-stdlib/shaders/README.md`'s.
