# 0010. Four bind groups, allocated by update frequency

Date: 2026-09-08

Status: Accepted

## Context

WebGPU's `maxBindGroups` limit defaults to 4, and that is also `wgpu`'s
default, so four is the budget every wxsl shader has to fit in. Some
backends disturb the bindings of higher-numbered groups when a lower one is
rebound with an incompatible layout, which is why the near-universal
convention is to order groups by update frequency, least frequent first.

Five things want a slot, and they update at five different rates:

| Frequency | What |
|---|---|
| per frame | camera, lights, fog, ambient, time, mouse, resolution |
| per pass | G-buffer inputs, shadow atlas, environment maps |
| per material | the graph's parameters, and its textures |
| per draw | object model and normal matrices |
| whenever | whatever the application wants to bind itself |

Four slots, five frequencies. Something must share, and until now the choice
was implicit: `wxsl/bindings.wxsl` put camera, scene *and* object in group
0, and `wxsl/lighting_pass.wxsl` put the G-buffer in group 1 because
nothing else was using it. That works only for as long as group 1 has no
other claimant, and the next thing we build — material parameter uniforms —
is exactly such a claimant.

## Decision

Four named slots, fixed for the life of the ABI, declared once in
`wxsl_core::abi` as [`GROUP_FRAME`], [`GROUP_MATERIAL`], [`GROUP_USER`]
and [`GROUP_PASS`]:

| # | Name | Contents | Owner |
|---|---|---|---|
| 0 | `FRAME` | camera, scene (lights, fog, ambient, time), object transforms | ABI |
| 1 | `MATERIAL` | generated parameter struct, textures, samplers | ABI |
| 2 | `USER` | anything the application binds | application |
| 3 | `PASS` | G-buffer, shadow atlas, IBL — resources a pipeline *shape* needs | ABI |

Per-draw object data shares group 0 with the per-frame data, as a
dynamic-offset binding: one buffer holds every draw's transform and each draw
selects its own with an offset, so the group is built once per frame and
never rebound. `min_uniform_buffer_offset_alignment` is 256 by default, so
entries are padded to 256 bytes even though the object uniform is 128.

The pass group goes at 3 rather than 2 because rebinding the highest-numbered
group disturbs nothing above it, and because a pass group is bound once per
pass, where its index costs nothing either way.

Group 2 is never touched by wxsl. Node definitions may declare bindings in
it; codegen emits the declarations, rejects two nodes claiming the same slot
with different types, and reports the set on the generated shader so an
application can check its layout before `wgpu` validation does.

## Alternatives considered

- **Strict frequency order** (0 frame, 1 pass, 2 material, 3 draw). The
  textbook layout, and rejected because it spends all four slots and leaves
  the application nothing. It also gains little here: the pass group and the
  material group are never rebound in the same loop, since the G-buffer pass
  has no pass-group input and the lighting pass has no material.
- **Keep the G-buffer in group 1**, overlapping the material slot, on the
  grounds that the lighting pass has no material. This is what the code did,
  and it would leave *both* 2 and 3 free. Rejected: one index would mean
  "material" in one pass and "G-buffer" in another, which is how you get
  mystifying validation errors, and it forecloses a fullscreen pass that
  wants a material graph of its own — plausible for post-processing, which is
  the empty `shaders/filter/` category.
- **Object transforms in their own group.** Correct by frequency, and
  rejected because it costs a quarter of the budget to hold 128 bytes that a
  dynamic offset addresses for free.
- **Object transforms as immediate data** (`immediate_size`, i.e. push
  constants). Rejected: two `mat4x4f` exactly exhausts the typical 128-byte
  limit, leaving nothing for anything else, and the feature is unavailable on
  WebGPU.
- **Object transforms in a storage buffer indexed by `instance_index`.** The
  more modern answer, and it needs no offsets at all. Not the baseline
  because read-only storage in the vertex stage rules out the WebGL fallback;
  it stays available as an opt-in later, since it does not change the slot
  allocation.

## Consequences

- The application gets exactly one free slot, not the two a bare
  `maxBindGroups` count suggests. That is the honest cost of the deferred
  path needing a pass group, and of a graph having to compile for either path
  (ADR 0005) — a node claiming group 3 would work in forward and collide in
  deferred, so group 3 is not offered to node authors.
- `wxsl/lighting_pass.wxsl` moves its G-buffer bindings from group 1 to
  group 3, and the deferred pipeline layout becomes
  `[Some(frame), None, None, Some(pass)]`. `wgpu` takes `Option`s here, so
  the holes are expressible; the material group is genuinely unbound in the
  lighting pass, which is correct, because in deferred the material's
  parameters are consumed while packing the G-buffer.
- Group indices become part of the ABI in the ADR 0008 sense: the `.wxsl`
  source and the Rust constants must agree, so a test asserts that the
  shipped shaders declare the groups the constants name.
- Material parameter uniforms have a home reserved before they are written,
  which was the point.
