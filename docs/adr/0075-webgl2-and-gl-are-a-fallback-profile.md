# 0075. WebGL2 and GL are a fallback profile; compute stays

Date: 2026-10-13

Status: Accepted — amends 0010, 0021 and 0039; plan3 N10

## Context

plan.md took WebGL off the table: the instance transforms are a storage
buffer read in the vertex stage (ADR 0021), and "WebGL is not a target, so
`VERTEX_STORAGE` is a cost only if that ever changes". plan3 N10 changed it,
and assumed the port would trade compute away for fragment lowerings.

The decision here keeps compute. WebGPU, Metal, Vulkan and D3D12 have it,
and the shipped pipelines already use it: the BRDF LUT bake, the bakes of
ADR 0045, the GPU MSDF generator, buffer effects. What has neither compute
nor storage is WebGL2, GLES 3.0 and old desktop GL. A Dawn compatibility
device on GLES 3.1 has compute but may have no storage in the vertex stage.
Those are a fallback, and both renderers have to be able to run them. The
wgpu renderer runs in the browser through wgpu's WebGL2 backend. The Dawn
renderer can run on a compatibility device and through Emscripten.

## Decision

**A `Profile` is data, in `wxsl-frame`** (`compute`, `storage_buffers`). Each
backend reads its own device. wgpu reads its limits (`device_profile`). Dawn
reads a bundle's `frame.profile`, which the exporter or the C ABI wire
config's `profile` field sets. The shared half then picks every fallback
once:

- **Instance rows become data textures, not vertex attributes.** Under
  `wxsl_instance_texture` (an ABI macro the renderer pins over every
  material), `instances`, `previous_instances` and a material's attribute
  array are `rgba32uint` textures, `INSTANCE_TEXTURE_WIDTH` texels wide,
  holding the same storage-layout bytes. They are padded per row by
  `resources::texel_rows`. Generated loaders (`resources::texel_loader`,
  from the host table and from the material's `BufferLayout`) rebuild a row
  by `instance_index`. Readers call `instance_row`,
  `previous_instance_row` and `material_instance` under both shapes.
- **Every effect declares its `Downlevel`.** It is `Portable`, `NativeOnly`,
  or a `Fragment` arm: the same inputs and parameters, its one storage
  texture written as a colour attachment. `EffectRegistry::add` refuses a
  compute effect declared `Portable`. The pipeline compiler lowers a
  compute pass to a screen pass under the same label, policy and target,
  and refuses a native-only effect by pass name (`PipelineError::NativeOnly`).
  `Renderer::set_graph` refuses again for hand-built pass lists.
- **The LUT and the bakes keep one integral each.** `brdf_integrate.wxsl`
  sits under both arms of the LUT. A generated bake's `fragment_source`
  shares its value function. Fragment positions are texel centres, so the
  arms agree to the bit.
- **Pass-group depth is read as `texture_2d<f32>` through a point
  sampler.** naga turns every depth texture into a GLSL `sampler2DShadow`,
  which cannot return a depth value. Depth resources a pass reads are
  therefore bound as unfilterable float. They are sampled at the texel
  centre with `textureSampleLevel(.., point_sampler, .., 0.0).x`, where
  `point_sampler` is a non-filtering frame binding
  (`BINDING_POINT_SAMPLER`). A plain `textureLoad` translates, but GL then
  reads the texture with no sampler object, and a depth texture whose
  default filter is linear is incomplete there: it reads zero. The shadow
  atlas, the one comparison-sampled texture, stays `texture_depth_2d` in
  the frame group. Its receiver-plane taps compare at texel centres through
  the comparison sampler, because a GL texture unit holds one sampler.
- **No shader queries what GLSL ES 3.00 lacks.** `Scene.environment_levels`
  (the old padding word) replaces `textureNumLevels`. The LUT spells
  `reverseBits` out. Generated uniform structs end in explicit padding
  words to a multiple of 16 bytes. F32 literals are suffixed `f`, as the
  integers already were, because naga leaves an all-abstract builtin call
  untyped.

## Alternatives considered

- **Per-instance vertex attributes for everyone** (plan3's lean). A mat4
  spends four attribute slots and the previous-frame row four more. The
  rows are also read in the fragment stage, through flat varyings that
  would cost locations. Data textures keep one indexing scheme, at the cost
  of a second shape. The agreement tests pay for that.
- **A uniform array at dynamic offsets.** WebGL2 guarantees 16 KiB per
  binding, so draws would be chunked at 128 instances, and an instance
  index would mean different things per chunk.
- **Fragment-only everywhere.** This throws compute away on the devices
  that have it, which is the opposite of the request.
- **Depth as `texture_depth_2d` read with `textureLoad`.** It translated
  in naga, then failed in the browser's GLSL compiler as a
  `sampler2DShadow`. Read as float with `textureLoad`, it compiled but drew
  deferred without shadows in WebGL2, for the incomplete-texture reason
  above. Only the browser showed either failure.

## Consequences

- `cargo test -p wxsl --test downlevel_corpus`: the GLSL ES 3.00 column,
  run through naga as wgpu's GL backend runs it. It covers every material
  stage, the lighting pass over every model, every effect against its
  `Downlevel` declaration (a native-only one that translates must be
  promoted), the bake arm and the UI pass. `graph_to_wgsl`'s every-node
  sweep runs the column too. The column first found real bugs:
  `blur_kawase` and `truchet_arcs` were invalid WGSL on every device, and
  `normalize`'s zero default never compiled. The column translates but
  does not compile the GLSL, so the browser smoke stays the last word.
- `cargo test -p wxsl --test downlevel`: forward and deferred, three
  instanced draws and a shadowed ground, on `GpuContext::headless_webgl2()`
  (a native adapter under `downlevel_webgl2_defaults`). The result equals
  the full device's frame exactly, with no validation errors. The LUT is
  identical through either arm, and a hand-built compute pass is refused
  by name. `bake_passes` adds the bake's fragment arm. Each agreement test
  was mutation-checked.
- Dawn: the renderer binds texture rows and bakes the LUT by fragments
  from the bundle's profile. `run_renderer_smoke.sh` exports
  `--profile webgl2` and `--profile compat` bundles and verifies them
  against wgpu with the unchanged comparator.
- Native-only today: the IBL prefilters (`textureNumLevels`) and the
  storage-buffer ramp pair. Everything else in the shipped registry,
  including peeling, refraction, DOF and the environment background, is
  `Portable` by the column.
- Not checked here: GL's 15 inter-stage varyings (`MAX_VARYING_LOCATIONS`
  still says 16), 4 colour attachments for a wide G-buffer, and
  float-render-target extensions. These are device limits a document can
  exceed; they are reported by wgpu, not by name.
- The wasm builds and the browser smoke are recorded in plan3 N10.
