# 0039. Tonemap is an effect, and ambient reads the LUT

Date: 2026-09-14

Status: Accepted

## Context

Two things ADRs 0034 and 0035 wrote down as future work had been sitting
there since, and they turn out to be the same sentence from two ends:
*what is in a colour buffer, and when does it stop being light?*

**The display transform was inside the shading function.** The generated
`shade_surface` ended in

```wgsl
var color = lit * scene.exposure;
@if(wxsl_tonemap)
color = tonemap_filmic(color);
return vec4f(linear_to_srgb(color), saturate(surface.alpha));
```

so every image any pass could read was already curved and encoded. Three
consequences, all of them paid:

* **Bloom thresholded the wrong numbers.** `bloom.wxsl` said so in its own
  doc comment — "the physically correct place to threshold would be before
  tonemapping … the effect is one `const` edit from it" — and thresholded
  at 0.72 of the encoded range, a number tuned by eye rather than a
  statement about light.
* **`wxsl_tonemap` was a macro on a *material*.** Whether the frame is
  curved for a display is not a property of a surface, and pinning it per
  material was a way to get two materials in one frame disagreeing about
  what the image is.
* **The forward path and the deferred lighting pass each had to be trusted
  to encode identically**, because each encoded for itself. They did — the
  corpus gate and the two-path image test are why — but "two
  implementations agree" is a thing to check, and one implementation is
  not.

**The BRDF LUT had no reader.** The `brdf_lut` compute effect (ADR 0035's
execution-policy proof) bakes the split-sum environment-BRDF table with
policy `once`, and nothing but a viewer effect ever looked at it: it was a
demonstration that `once` works, not a thing the renderer used.
Meanwhile `ambient_environment` — the specular half of every material's
ambient — faded its Fresnel term by `1.0 - roughness` and called it a
day, which is a hand-tuned stand-in for exactly the function that table
holds.

## Decision

### The display transform is one pass at the end of the chain

`shade_surface` returns `lit * scene.exposure` — linear radiance, exposed
but neither curved nor encoded — and the shipped `tonemap` effect
(`wxsl-render/shaders/tonemap.wxsl`) applies `tonemap_filmic` and
`linear_to_srgb` over the whole image, into the frame's own target. Every
stock pipeline ends in it, in all three of the spellings that have to
agree: the preset documents, the hand-built reference graphs, and
`stock_document`.

`wxsl_tonemap` is gone from `abi::abi_macros`. The curve and the encode
are still the library's `package::color` modules — the tonemap effect
imports them — so there remains exactly one filmic curve in this repo.

Consequences of the move, taken deliberately:

* **Bloom thresholds at 1.0 linear**, with a wider knee. Diffuse white is
  the line: what glows is what is brighter than a white surface fully lit,
  which is what "highlight" means. That is the `const` edit the old
  comment promised.
* **A material pass can start a chain.** `pass.geometry` gains an optional
  `into`, the same socket `pass.screen` has, and the compiler resolves
  both through one `write_target`. A forward pipeline could previously
  only *end* a chain; the rule "a chain passes along a resource, and
  another pass's output is nobody's to write" is now one rule rather than
  one rule and one special case.
* **The clear colour is linear radiance, and the head of the chain clears
  to it.** An intermediate cleared to transparent would make every chained
  pipeline's background black regardless of the configuration, because
  what the viewer sees as background is whatever the head left where
  nothing was drawn — the lighting pass `discard`s there, and a forward
  pass never covers the screen. The shipped default is a much smaller
  number than before for the same reason: it goes through the curve now.
* **An effect that ships its own source no longer inherits the materials'
  macro set.** It never read it; keying its cached variant on it meant a
  material macro recompiled every effect in the chain. Only
  `EffectShader::Lighting` inherits, because its text comes from the same
  ABI templates a material's does.
* **The debug-normal view goes through the transform too.** It is shaded
  like everything else, so it is curved and encoded like everything else.
  A test that wants the raw number presents through a chain with no
  display transform, which is what `probe::linear_document` is.

### Ambient reads the baked table

`abi::BINDING_ENVIRONMENT_LUT` and `abi::BINDING_ENVIRONMENT_SAMPLER` join
the frame group, and `wxsl/bindings.wxsl` declares them beside a lookup:

```wgsl
fn environment_brdf(n_dot_v: f32, roughness: f32) -> vec2f
```

`ambient_environment` takes that `vec2f` as its last parameter and its
specular term becomes the split-sum form, `irradiance * (F0 * scale +
bias)`. The default, `(1, 0)`, is a caller with no table — F0 straight
through — so a graph wiring the node by hand is unchanged and valid.

In the frame group for the shadow maps' reason: the forward stage and the
deferred lighting pass shade through the same function, so one lookup
serves both paths and there is nothing to keep in agreement.

The table is baked by the `brdf_lut` effect's shader, run once, before the
first pass of the first frame, into a texture the frame bindings own. The
`once` policy's argument applies unchanged — the shader is a pure function
of its own coordinates — but the bake is not a pass, because a pipeline
*document* cannot express a compute pass yet (plan3's N3) and the table
has to be there for every pipeline, including a hand-built one that never
heard of it, since every material's ambient term reads it. When documents
grow `pass.compute`, the bake moves into the stock documents and this
becomes the default rather than a fixture.

## Alternatives considered

**Keep `wxsl_tonemap` and let the effect read it.** A macro with two
readers and no owner, and the per-material disagreement stays expressible.
The flag was never about a material.

**Tonemap in the `present` step rather than as an effect.** `present` is a
terminal marker in a document, not a pass; making it run a shader would
give it an identity it deliberately does not have, and would put the
transform somewhere a document cannot take it out of. The demo that takes
it out — `T` in `pbr_cube` — is the argument: removing a pass is document
surgery, removing a hidden step is a renderer feature.

**Leave intermediates cleared to transparent and let the tonemap pass
clear the frame's target.** The tonemap covers every pixel, so its clear
is never seen; the background would come from the intermediate and be
black everywhere. The clear colour belongs to the frame, not to a
resource.

**Bake the LUT at renderer construction.** `Renderer::new` has a device
but no queue, and a lazy bake on the first frame is the same number of
dispatches with no API change.

**Keep the LUT in a pass list and bind it like the shadow maps.** That is
the right end state and is what `declare_shadow_maps` is a template for —
but it needs `source.environment` and `pass.compute` node definitions,
which is N3's item. Doing half of it now would mean a document node whose
pass could not be written.

**Sample the table inside `ambient_environment` rather than passing the
value in.** A stdlib node function that reached for a `@group(0)` global
would stop being a function of its arguments, and the node could no longer
be wired into a graph that does not go through `shade_surface`. The value
is computed at the one call site that has the frame group in scope.

## Consequences

* The `once` bake is infrastructure rather than a demonstration, and the
  gallery's `brdf-lut` demo still shows the pass form of it.
* A pipeline that does not end in the `tonemap` effect presents linear
  radiance. Correct for a chain that goes on to another effect, wrong on a
  screen — which is why the stock pipelines carry it and why the probe
  harness deliberately does not.
* A chain's intermediates are HDR (`rgba16float`) and linear, so anything
  reading one — a bloom threshold, and a history buffer when N2's velocity
  stage arrives — reads light.
* The gallery gains an `ibl` demo: no lamps at all, a sky and a bounce, so
  what is in the image is `ambient_environment` and therefore the table.
* Committed gallery images and the demo luminances move, because the whole
  frame goes through the curve now rather than the shading function.
* If this changes, also update `docs/architecture.md` and `AGENTS.md`.
