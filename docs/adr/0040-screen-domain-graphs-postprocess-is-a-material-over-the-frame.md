# 0040. Screen-domain graphs: postprocess is a material over the frame

Date: 2026-09-14

Status: Accepted

## Context

ADR 0034 made an effect data: a descriptor saying what it reads, what it
writes, and where its shader text comes from. It named the end state it was
not yet reaching — *an effect authored as a graph* — and left `EffectShader`
with two variants, `Lighting` (generated from the enabled lighting set) and
`Source` (a `.wxsl` file the effect ships).

`Source` is the one that hurts. Everything a post effect does — sample the
image, threshold it, curve it, mix two things — is what the node library
already does, in the same language, with the same macro chain and the same
variant cache behind it. But a material graph could not be a post effect,
because the vocabulary it compiles into is a *material's*: a
`SurfaceContext` of world positions and tangent frames in, a `Surface` of
seven fields out, one module per `MaterialStage`. None of that means
anything to a fullscreen pass, and the pieces a fullscreen pass does need —
which image, where in it, how big a texel is — had nowhere to come from.

So an effect was either a descriptor plus a file, or it did not exist. That
made "add a post effect" a thing you do in a text editor with a WGSL
reference open, in a repo whose entire premise is that you should not have
to.

There was a second, quieter version of the same gap. Pipeline documents
(ADR 0033) are graphs too, and their nodes — `pass.screen`, `resource.color`
— are meaningless in a material. Nothing said so. A `source.scene` dropped
into a material graph was a codegen crash rather than an error, and a
palette over a pipeline canvas would have had to hardcode which ids to show.

## Decision

### A graph has a domain

`GraphDomain` is `Surface`, `Screen` or `Document`, carried by the graph and
stored in its document (absent means `Surface`, so every file written before
this loads as what it was). A `NodeDefinition` carries the `Domains` it may
be placed in, and `Graph::validate` reports a node outside its graph's
domain by name, beside every other typing error.

Most of the library is in all three — `math.add` does not care what it is
adding for — and the restricted ones mostly restrict *themselves*: a body
that reads a surface context, declares a material's bind group, writes a
surface, or is a pipeline node says which domain it is in, and
`NodeDefinitionBuilder::build` reads the mask off the body. Only the handful
a body cannot decide are written down: a context field two domains share is
in both, one only one of them has is in one.

`NodeRegistry::in_domain` is the filter, and the editor's palette runs it
over the graph being edited, so a canvas never offers a node it would refuse.

### The screen ABI, beside the surface ABI

`package::wxsl::screen` is the screen domain's `surface.wxsl`, and about a
tenth its size:

```wgsl
@group(3) @binding(0) var wxsl_screen_image: texture_2d<f32>;
struct ScreenContext { uv: vec2f, time: f32, pixel: vec2f, texel: vec2f }
fn screen_context(position: vec4f) -> ScreenContext
```

`uv` and `time` are spelled exactly as `SurfaceContext` spells them, so
`input.uv` and `input.time` are *one node each*, usable in both domains —
the same trick `VertexContext` plays on `SurfaceContext`, and the reason
this is a second half of a vocabulary rather than a second vocabulary.
`input.pixel` and `input.texel` are screen-only, and `input.image` hands out
the bound texture as an ordinary `texture_2d<f32>` value, so every node that
already takes a texture reads the frame with nothing new.

`codegen::generate_screen` compiles such a graph into one module: the
graph's own function over a `ScreenContext`, a fullscreen triangle, and a
fragment entry calling one with the other. It shares validation, node
emission, the macro precedence chain and the module header with
`generate` — and shares nothing else, because a material's generation is
stages, interpolants, a discard test and a lighting model, and an effect's
is one function.

### An effect can be a graph

`EffectShader::Graph { graph, wxsl }` joins `Lighting` and `Source`. The
WXSL is generated once, in `Effect::from_graph`, so a graph that does not
compile fails at registration rather than at the first frame that wanted the
pass — and so that nothing downstream of the descriptor needs a node
registry. Past that point a generated module is a module: the pipeline
compiler, the variant cache and the pass encoder were not told.

The shipped graph effects live in the `wxsl` facade crate rather than in
`wxsl-render`, for the reason `stdlib_library()` does: the renderer does not
depend on the node library (ADR 0002), and a graph-authored effect needs
both halves. `wxsl::effects::registry()` is the shipped registry with
`tonemap` **replaced, under its own id**, by a graph — so every stock
pipeline, every preset file and every demo goes on naming `tonemap` and gets
a generated module, with nothing edited to allow it. That the gallery's
luminances do not move is the evidence.

`fxaa` is the first effect that arrives as a graph rather than being
translated into one. The filter itself is a library function over a texture
(`filter/fxaa.wxsl`, an original implementation in that family — ADR 0007),
so it is equally a node a *material* can call; what the screen domain
supplies is only which image, where, and how big a texel is.

## Alternatives considered

**One domain, and let a screen graph reach for a `SurfaceContext` that is
half zeroes.** The half that is zeroes is the geometry, which is most of it,
and a node reading `world_normal` in a fullscreen pass would compile and be
wrong. An error naming the node is worth more than a plausible black image.

**A separate registry per domain.** A registry is global, and a definition
is a definition; two would fork the palette, the serialization and the
"which registry was this validated against" question. The mask is on the
definition, and filtering happens where things are shown.

**Let a screen graph declare its own inputs, so an effect can read two
images.** That is the honest end state and it is N4's (effect parameters)
and N3's (a compute node whose sockets come from its effect's declaration)
— the same unanswered question in both: how a graph declares what the
pipeline must wire into it. Until then the screen ABI binds the one image
every shipped screen effect already reads, and an effect wanting two stays a
descriptor with its own source. Bloom is that effect today.

**Generate the effect's WXSL lazily, at compile time.** The variant cache's
`EffectRequest` has no node registry and should not grow one; generation is
cheap and happens once. The cost is that an effect's graph cannot be edited
in place — it is re-registered — which is the same gesture as replacing any
other effect.

**Ship the graph effects from `wxsl-render`, by hand-writing the node
definitions it needs.** That is a second declaration of signatures the
`.wxsl` files already carry, which is exactly what ADR 0020 exists to
prevent. The crate boundary is doing its job here, not getting in the way.

**Keep `tonemap` a file and ship only `fxaa` as a graph.** Then the claim is
a demonstration rather than a fact: a new effect nobody depends on, proving
nothing about the ones that carry the frame. Replacing the display transform
every stock chain ends in — and having the image not move — is the test.

## Consequences

* A pipeline document is a `Document`-domain graph, so `pipeline::document`
  is the one way to start one and the preset files carry `"domain":
  "document"`. A document without it loads as a material holding pass nodes
  and says so.
* A screen graph declares nothing bindable: `param.value`,
  `texture.texture_2d`, `input.user` and `input.attribute` are surface-domain,
  so validation refuses them and a screen module's interface is empty by
  construction. An effect that wants a knob waits for N4.
* A node handing out a texture binds its name straight through rather than
  through a `let`, because WGSL has no `let` for a handle. Codegen decides
  that from the socket's type, so it holds for any such node.
* `Effect` is `Clone` rather than `Copy`: a graph-authored one owns its
  graph and its text behind an `Arc`.
* A graph effect's variant key folds in a hash of its generated source, not
  just its id — two effects registered under one id at different times are
  two shaders.
* `filter/` stops being an empty directory with a note about M7 in it.
* If this changes, also update `docs/architecture.md`, `AGENTS.md` and
  `crates/wxsl-stdlib/shaders/README.md`.
