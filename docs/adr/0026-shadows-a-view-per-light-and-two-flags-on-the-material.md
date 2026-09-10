# 26. Shadows: a view per light, a lookup in the shading function, and two flags on the material

Date: 2026-09-10

Status: Accepted

Amends [0021](0021-a-declarative-render-graph-and-a-scene-document.md),
[0025](0025-a-material-graph-spans-shader-stages.md)

## Context

ADR 0025 partitioned a material graph so that a pass writing no colour
could still compile the displacement and the alpha test. It was built for
a consumer that did not exist yet: `MaterialStage::SHADOW` was a row in
the stage table, compiled correctly, and nothing ever asked for it. The
milestone's two acceptance tests — a perforated material casting a
perforated shadow, a displacing one casting a displaced shadow — could not
be written at all.

Making them writable turned out to need one thing the pipeline had never
had, and it is not the texture or the filter.

**A pass could not change its point of view.** The camera is one uniform
in the frame group, filled once per frame from `Environment::camera`.
Every pass in every pipeline rendered from it. A shadow pass renders the
same objects from a light, which means the vertex stage needs a different
view-projection — and the vertex stage is hand-written ABI shared by every
material.

Around that sit the ordinary parts: somewhere to render into, a way to
find it again while shading, and a filter that does not produce acne on
exactly the two things this milestone introduces.

And one question the ABI could not answer at all: whether a given material
takes part. "Transparent" is a tag; "casts a shadow" and "receives one"
are the same kind of fact about a material, and nothing carried them.

## Decision

**A pass renders from a named view, and the frame supplies the list.**
`PassDesc::view` is a `PassView` — `Camera`, or `Light { index }` — and
`FrameBindings` uploads one `CameraUniform` per view into a single buffer,
addressed by a **dynamic offset** on the frame group's camera binding.
Nothing in any shader changes: `camera.view_proj` is whichever view the
pass named, so `transform_vertex` is the same function in a shadow pass as
in a forward one, and a pipeline with no shadow passes binds offset zero
and pays nothing.

**One shadow pass per light *slot*, always — not per casting light.** A
pass list is built once and scheduled against every frame after it, and
which lights cast is the environment's business and changes whenever the
application says so. So both stock pipelines declare `abi::MAX_LIGHTS`
shadow passes up front. A slot whose light is not casting is cleared to
the far plane and drawn into by nothing, which reads as "fully lit" — the
same answer, for the cost of a clear.

**The maps are a depth 2D texture array in the frame group**, at
`abi::BINDING_SHADOW_MAPS`, beside the lights they come from and with a
comparison sampler at `abi::BINDING_SHADOW_SAMPLER`. The frame group and
not the pass group, because the forward stage and the deferred lighting
pass must read them the same way: `shading.wxsl` gets one `shadow_factor`
and both paths inherit it, exactly as they already inherit the light loop.
The pass group could not do that — it holds the G-buffer in the deferred
path, and its bindings are numbered per pass.

**A pass that writes the maps binds a placeholder instead.** `wgpu`
tracks usage per bind group, not per shader, so a shadow pass with the
frame group bound would be sampling and writing one texture in one pass —
a validation error, even though its shader compiles no shading at all. So
`FrameBindings` keeps two bind groups per instance-row shape,
`ShadowMaps::Bound` and `ShadowMaps::Detached`, and the renderer picks by
whether the pass writes the resource.

**PCF over a comparison sampler, biased by normal offset.** A 3x3 kernel
of hardware-filtered comparisons — nine fetches, each already bilinear.
The bias moves the *sample point* along the surface normal rather than
moving the comparison depth, scaled by how grazing the light is. A
constant depth bias has to be large enough for the worst slope in the
scene and then peels every contact shadow off its object; it also behaves
worst on precisely the two things this milestone exists for, a displaced
vertex whose normal no longer matches its position and a perforated
surface that is depth discontinuities all the way down.

**Front faces are not culled.** The usual trick for hiding acne, and it
only works on closed geometry — which an alpha-tested leaf and a displaced
plane are not.

**A material declares `cast_shadow` and `receive_shadow`**, on
`MaterialEntry` in the scene document and on `MaterialOptions` in the
renderer, both defaulting to on. They are as much a property of a material
as its tags are. They land in different places because they are different
kinds of thing:

* `cast_shadow` is a **selection**. The shadow passes skip a material that
  does not cast, filtered where the frame is compiled so it never even
  compiles a variant it will not draw. Two materials differing only here
  generate byte-identical WGSL, and a test says so.
* `receive_shadow` is **code**. It is `abi::FEATURE_RECEIVE_SHADOWS`, a
  macro, so a material that does not receive one does not compile the
  lookup at all and the variant cache keeps the two apart on their macro
  sets.

**A directional light gets an orthographic box centred on the origin.**
Author-set extent and depth. A point light asking for a shadow gets none
rather than a wrong one: one slice is one point of view, and a point light
needs six.

## Alternatives considered

**A per-pass camera *buffer* instead of a dynamic offset.** One uniform
per view, rebound per pass. It is more bind group churn for the same
result, and it makes the frame group's contents depend on which pass is
recording — which is the thing ADR 0010's group ordering exists to avoid.

**Putting the light's matrix in the scene uniform and selecting it in the
vertex shader.** No dynamic offset, no new pass field. It also means
`transform_vertex` branching on which pass it is in, so the one function
every material shares would have to know that shadows exist.

**Generating the pass list from the environment each frame.** Honest —
one pass per casting light, no wasted clears — and it throws away the
schedule, the resource allocation and every pipeline keyed on the pass
list every time the application moves a light. A fixed shape with cheap
empty slots is the same picture for a clear per unused light.

**A shadow atlas with a rect per light** rather than a fixed-resolution
array slice. The right answer eventually — a shadow needs more texels the
closer its light is to what it falls on — and a contained change when it
comes: one differently-shaped binding, plus a rect in the light.

**Depth bias rather than normal offset.** Simpler, one number, and
supported by `wgpu::DepthBiasState` for free. It is also the version that
fails on this milestone's own acceptance cases.

**`receive_shadow` as a uniform rather than a macro.** No second variant.
It also means every material pays for the PCF loop and a branch, to
express something that never changes while the material exists.

**Both flags as tags.** Tempting, since `transparent` is one and these sit
beside it. Tags are an open set the application invents members of, and a
pass selects on a tag *expression*; these two are closed, and one of them
has to reach codegen, which a tag cannot.

## Consequences

Both stock pipelines are four passes longer. `forward_graph` is now four
shadow passes, a depth prepass and a shading pass; the tests that assert
their shape say so in terms of `abi::MAX_LIGHTS` rather than a literal.

The scene uniform grew: `Light` gained a `mat4x4f`, a slice index and a
bias, taking one light from 32 bytes to 112. Host-shared, so
`bindings.wxsl` and `wxsl-render`'s `LightUniform` are edited together and
a size assertion is what catches drift.

`depth_only` and `shadow` no longer generate identical source for every
material — ADR 0025 predicted they would diverge "once the shadow stage
renders from a light's point of view", and they have, though only through
the view they bind rather than through the code.

**In the deferred path, `receive_shadow` is a property of the frame rather
than of each material.** The lighting pass is one fullscreen draw compiled
against one macro set — the first draw's — so a scene mixing materials
that receive and materials that do not will shade them all the same way
there. This is not new: `wxsl_tonemap` and `wxsl_debug_normals` have
always behaved this way. Making it per material needs a bit in the
G-buffer, and there is no ninth channel going spare.

A shadow pass draws every material that casts, including ones whose graph
does nothing in a shadow stage. That is one draw call per object per
casting light, with no culling: the frustum is the light's, and nothing
computes it.

If this changes, also update `crates/wxsl-stdlib/shaders/wxsl/shadow.wxsl`
and the `Light` struct in `bindings.wxsl`, which are the other half of
`wxsl_render::environment`'s `LightUniform`.
