# 24. A material declares the vertex and instance attributes it requires

Date: 2026-09-10

Status: Accepted, amended by [0027](0027-a-graph-computes-its-own-interpolants.md)

Amends [0008](0008-surface-graphs-and-a-named-shader-abi.md),
[0010](0010-four-bind-groups-allocated-by-update-frequency.md)

## Context

ADR 0023 gave a graph three ways to say what it needs from outside itself:
uniform parameters it owns, textures and samplers the host binds, and a
block the application fills. All three come in through a bind group. The
fourth thing a shader reads from is the *geometry*, and there a graph
could say nothing at all.

Both ends were fixed. `VertexIn` in `shaders/wxsl/vertex.wxsl` declares
four attributes — position, normal, tangent, uv — mirrored by
`mesh::Vertex::ATTRIBUTES`. Per-instance data is a model matrix and its
inverse transpose, and nothing else. A material that wants a second UV
set, a vertex colour, a per-vertex wind weight, or a per-instance tint,
age or animation offset has nowhere to put any of it, and a glTF file's
`COLOR_0` accessor was read and discarded.

This is the same shape as ADR 0023's third case and it wants the same
treatment. A declared attribute is a **requirement on the geometry**,
exactly as a group-2 block is a requirement on the application: the
material states what it needs, hands out the shape, and somebody else
supplies it — with a named error when they cannot, rather than a frame
that merely looks wrong.

Two things make it harder than the bind-group cases. Inter-stage
`@location`s are a budget of sixteen that four different things now want
to spend from. And WGSL offers `@builtin(instance_index)` in the vertex
stage only, while a surface graph is evaluated in the *fragment* stage —
so a fragment that wants per-instance data has to be told which instance
it is.

## Decision

**One declaration, two backings, and the frequency decides which.**
`Graph::attributes` is a list of `AttributeDecl { name, ty, frequency }`,
graph-level like `Graph::user_block` and for the same reason: a
requirement on somebody else has to be readable without walking the
nodes, and has to stay stated while the branch that reads it is half
wired. `AttributeFrequency::Vertex` gets a vertex buffer;
`AttributeFrequency::Instance` gets a row of a storage buffer.

**Reading one is `input.attribute` with the name as a setting**, checked
against the declared set — not a generated node kind per attribute, which
would make `NodeRegistry` per-graph when it is global and shared. The
node does *not* say which frequency, because the declaration does, so
moving an attribute from per-vertex to per-instance rewires nothing;
`moving_an_attribute_between_frequencies_rewires_nothing` asserts the node
set is untouched. The type comes from the document, so this is one of the
few sockets where a resolved generic can be *wrong* rather than merely
missing, and `AttributeTypeMismatch` says so.

**Per-vertex: one vertex buffer per attribute, at slot 1 and up.** Not a
widened interleaved struct. glTF hands its accessors over per accessor
anyway, a mesh can serve a material that wants colours and one that does
not without re-uploading its positions, and the base stream stays a
`#[repr(C)]` struct with a mirror to check. It costs vertex buffer slots,
of which WebGPU guarantees eight, so `abi::MAX_VERTEX_ATTRIBUTES` is
**four** — written down rather than discovered when somebody declares a
fifth. `MeshData::attributes` names the streams; `Mesh::check_attributes`
is what turns a mesh that lacks one into an error naming the material, the
attribute and the mesh.

**Per-instance: a second storage array beside the transforms, not a
widened one.** This is the decision that changed under test, so it is
worth stating why. The obvious shape — appending the declared fields to
the row at `abi::BINDING_INSTANCES` — puts *two strides* on one array: the
hand-written `transform_vertex` reads that array through the ABI's
`Instance` struct at 128 bytes a row, and a row that grew to 144 is read
at the wrong offsets by code that cannot know it grew. On hardware that
looks like a thousand quads in a thousand wrong places. So the declared
attributes are their own array at `abi::BINDING_INSTANCE_ATTRIBUTES`,
indexed by the same `@builtin(instance_index)`, and neither array has to
know the other's width. The transform array stays exactly what it was:
ABI, one shape, one upload for the whole frame.

Its three original reasons all survive: it is one binding however many
attributes are declared, it puts no pressure on the vertex slots the
per-vertex half is spending, and it survives indirect draws, where a GPU
culling pass emits instance *indices* and an instance-step vertex stream
would have to be compacted to match.

**The row's layout is computed by `BufferLayout::storage`**, the second
customer of ADR 0023's layout computer and the reason that code is a
table rather than a struct. The only difference from the uniform address
space is the struct alignment, which is not rounded up to 16 — one branch,
one constructor.

**The index travels, not the values.** One `@interpolate(flat) u32`
varying carries `@builtin(instance_index)` to the fragment stage, which
re-indexes the array itself. One location covers *every* declared instance
attribute however many there are, which is strictly better than passing
each value down, and it is the only reason this touches varyings at all.

**The extended IO structs sit beside the ABI's, never instead of them.**
WGSL lets an entry point take several IO parameters but return only one,
which decides the shape exactly:

- The vertex entry takes `(input: VertexIn, extra: MaterialVertexIn)`, so
  `VertexIn` is untouched hand-written ABI and what the material adds sits
  next to it.
- It returns one generated `MaterialVertexOut`, which repeats the base
  varyings at the same locations and adds the extras above them. Its base
  half is written from `abi::VERTEX_OUT_FIELDS`, which is also what
  `vertex.wxsl` declares, and `wxsl-stdlib` has the test that holds the
  two together.
- The fragment entry takes `(vertex: VertexOut, extra: MaterialVaryings)`,
  so `surface_context` receives the ABI's own struct and needs no
  widening.

A material that declares nothing emits exactly the two entry points it
always emitted. Not "almost": no `.wxsl` file changed in this milestone,
and `declaring_nothing_generates_what_it_always_did` asserts that no
generated module mentions any of the invented names.

**The location budget gets one accountant.** `Graph::validate` counts the
base varyings, the instance index if it travels, and one per declared
per-vertex attribute against `abi::MAX_VARYING_LOCATIONS`, and reports
which line item overran it. Narrowing that per *stage* — a depth-only pass
needs none of them — is M5's partitioning with a general mechanism, not a
special case here.

**What the geometry wants is part of the shape a pipeline is built for.**
`MaterialInterface::signature` gained the vertex layout and the instance
stride, so two materials differing only there do not share a pipeline. The
frame group's bind group *layout* is unchanged in shape — the attribute
binding is always in it, whether or not the material being drawn declares
anything — because a layout that changed per material would invalidate
every pipeline layout built against it. Only the buffer behind it differs,
one per row shape in the frame.

## Alternatives considered

**Widening the instance row at `BINDING_INSTANCES`.** What the plan said,
and what this was built as first. It fails for a concrete reason found on
hardware: two views of one array at two strides, one of them hand-written
ABI that cannot know the other exists. Making it work means the vertex
stage must also read the wide view, which means `transform_vertex` stops
taking a `VertexIn` — an ABI change, to save a binding index there is no
shortage of.

**An instance-step vertex buffer** (`VertexStepMode::Instance`). The
obvious route, and it loses to the decision already settled for instance
transforms in ADR 0021, for its three reasons above.

**A generated node kind per declared attribute.** Reads better on the
canvas — `input.color` rather than `input.attribute` named `color` — and
makes `NodeRegistry` per-graph, which is a much larger thing to own than a
validated string. It would also make the frequency part of the node kind,
so moving an attribute between backings would rewire the graph.

**Inferring the declarations from the nodes that read them.** Symmetrical
with how `param.value` works, and wrong here for the reason
`Graph::user_block` is declared rather than inferred: the *mesh* has the
streams it has, and a requirement that appeared and vanished as a branch
was wired up is not a requirement. It would also make the instance stride
depend on which nodes a stage happens to reach, and the instance buffer is
uploaded once for the whole frame.

**Per-stage narrowing now.** A depth-only pass reads none of the declared
attributes and should bind none of their buffers. It falls out of
reachability, which is exactly M5's partitioning; doing it here would be a
special case that M5 then has to unpick.

**Dense per-shape packing of the attribute rows.** Every shape's buffer is
as long as the whole draw list, so a frame mixing two shapes allocates
rows nothing writes. Packing densely means a draw's row index is no longer
its index in the draw list — and that index is what
`@builtin(instance_index)` is, and what an indirect buffer the application
wrote holds. One shape, which is every frame this repo draws, wastes
nothing.

**Nested structs for the extended varyings.** Would keep the base half in
one place. WGSL does not allow a struct member of struct type in an
entry-point IO type, which is what forces `MaterialVertexOut` to repeat
the base locations — and therefore what forces `abi::VERTEX_OUT_FIELDS` to
be a table.

## Consequences

A glTF file's `COLOR_0` and `TEXCOORD_1` now reach a shader, under the
names `gltf::COLOR_ATTRIBUTE` and `gltf::UV1_ATTRIBUTE`. Nothing else does:
joints and weights want a skinning stage that does not exist, and
inventing a name for every semantic a file might carry is a vocabulary
nobody agreed to.

`DrawItem` gained a third field, `attributes`, so every caller building a
draw list by hand is affected again. A draw that forgets one its material
declares is `MissingInstanceAttribute`, reported while the frame is
compiled and before a pass is opened.

`MeshData::extend` now drops any stream the two sides do not both carry.
There is no value that would be right to invent for the vertices of a
primitive with no colours, and a stream half-filled with an invented one
is worse than no stream: the material would draw, and half of it would be
wrong.

The editor can read an attribute a document declares and cannot yet
declare one: the preview invents a positional gradient for every
per-vertex stream and a one for every per-instance field. That is the same
state textures are in after ADR 0023, and for the same reason — the editor
is not the application. One rather than zero, unlike the magenta checker's
"make the placeholder visible" rule: the preview draws a single object, so
there is no per-instance variation to show, and zero multiplied into a
base colour is a preview that went dark for a reason the author cannot
see.

The layout computer now has both its customers, so
`crates/wxsl/tests/probe/mod.rs` holds one probe graph that both
`material_resources.rs` and `material_geometry.rs` run: every value type
written through the computed layout and read back on a GPU against a
literal the shader compiler inlined, once in the uniform address space and
once in the storage one. Those two files are what notice if the alignment
rules are ever got wrong.

If this changes, also update `wxsl_core::abi`'s vertex and instance
tables, `docs/architecture.md`'s bind-group table, and
`crates/wxsl-stdlib/shaders/README.md`'s.

## Amendment (ADR 0027)

A third frequency: `AttributeFrequency::Computed`, a value the graph's own
vertex stage produces rather than one the geometry supplies. The reading
node is unchanged — this ADR's claim that which backing a name has is the
declaration's business and not the node's is what makes that possible —
and the writing side is a terminal, `output.varying`.

The accountant here now counts three kinds of spender rather than two, and
interpolants are numbered *after* everything the geometry brings so that
declaring one never moves a vertex attribute's location. See
[0027](0027-a-graph-computes-its-own-interpolants.md).
