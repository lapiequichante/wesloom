# 0046. Velocity is a stage; TAA is a policy'd chain

Date: 2026-10-04

Status: Accepted

Implements plan3's N2, the last slice of plan.md's M7. The graph half
pre-existed — M5's `previous_time` in the scene uniform and the
`wxsl_previous_frame` macro were landed "because retrofitting them once
many materials read `time` is a migration" — and the effects work (ADRs
0034, 0040, 0042) removed everything downstream of the resolve. What was
new: the stage, the previous-frame transform row, and the two consumers.

## Context

The stage table has carried `Velocity`'s empty chair since M2: *screen
motion per fragment — position, current and previous*. Every temporal
technique the plans name (TAA, stabilised SSR, motion blur) reads that
one buffer, and none of it was expressible, because the transform at
`t-1` did not exist anywhere:

* the frame group carried the instance transforms **once** — a shader
  could know where an object *is*, never where it *was*;
* the camera uniform carried **one** view-projection — a still scene
  under a moving camera would have had no motion at all;
* `pass.screen`'s document node had fixed sockets whose one `image` input
  cannot name a three-texture read, and a temporal resolve is exactly
  that: colour, velocity, and its own last output.

## Decision

### The velocity stage is one table row — and it transforms twice

`MATERIAL_STAGES` gains the row the plan always named: fragment entry
`fs_velocity`, returning one `vec2f` of screen motion at `@location(0)`,
uv units per frame, `y` down — the orientation every screen-space uv in
the ABI already uses, so `uv - motion` is where the fragment was and no
consumer needs a flip. The stage reads **no surface**: `needs_surface`
stops being `output != Nothing` and becomes the material-function
question it always meant to be, with `always_has_fragment` carrying the
other half (a colour-writing stage has a fragment program; a
depth-shaped one has one only when the material discards). Partitioning
is unchanged — the velocity module gets the vertex offset and the alpha
test, and not the shading function, which is the stage's share of the
ADR 0025 saving.

The vertex entry is the one vertex entry in the ABI that differs from
every other's, and it differs in exactly one way: it transforms each
vertex **twice** — this frame's camera and instance row, and last
frame's — and the fragment interpolates the difference. Both clips ride
the output as plain varyings (`current_clip`, `previous_clip`), because
`@builtin(position)` arrives in a fragment stage as *framebuffer
coordinates*; the current clip duplicated as a varying is the price of a
per-fragment motion vector and the difference of the two interpolated
clips is exact.

The graph half costs nothing new: the vertex partition is emitted **once**
and evaluated **twice** — this frame against the context every stage
builds, last frame against `previous_vertex_context`, a twin that reads
`previous_instances` and `camera.previous_view_proj` and stamps
`time = previous_frame_time()`. A displacement driven by `input.time`
answers for the frame that has passed without the graph knowing a
previous frame exists — which is what M5's macro was for, realized one
context at a time instead of one compilation at a time.

### The previous frame lives beside the current one, not inside it

Two frame-group additions, both second arrays beside the current ones
and both "no motion" by default:

* `BINDING_PREVIOUS_INSTANCES` (8) — the instance rows as last frame had
  them, indexed by the same `@builtin(instance_index)`. A second array
  rather than a wider row, by ADR 0024's stride rule. The host fills it
  from each draw's `previous` transform — `DrawItem::with_previous`;
  a draw that states none reads its own current transform there, which
  is zero motion and every frame's behaviour before the row existed.
* `camera.previous_view_proj` and `previous_position` — filled from
  `Environment::previous_camera`, the same shape of fact as
  `previous_time`: "the previous frame" is a fact about the sequence of
  frames, and the application that moves the camera fills it exactly as
  `advance` fills the clock. `None` — the default — carries the current
  matrix there.

`Camera` also grows `jitter`, an NDC sub-pixel offset applied inside
`view_proj`. It exists so a TAA pipeline can move the projection a
fraction of a texel per frame — the half that turns accumulation into
antialiasing — and it composes honestly because each frame's jitter
rides *its* matrix: the velocity stage measures the difference of the
two jittered projections, so the jitter cancels in the motion vector and
the reprojection lands where the surface was actually sampled.

### TAA and motion blur are effects; the resolve's ring is the pipeline's

`wxsl.taa` and `wxsl.motion_blur` ship as effect descriptors beside
bloom — shader files, declared inputs, one parameter each (`blend`,
`strength`). TAA's three inputs are the reason the fixed `pass.screen`
socket set was never going to be enough, and the fix is the one N3
already established for compute: **one derived definition per screen
effect** — `pass.screen.<effect-id>`, whose sockets *are* the
declaration, registered by `EffectRegistry::node_defs` beside the
compute rows. A colour input's `history` lives on the declaration
(`EffectInput.history`): TAA's `history` input reads a frame back
through the ring `into` writes, which is what makes the pass order
against the scene and never against itself — `Read::previous` creates no
ordering edge, and writing the history and reading it is the shape
`reading_history_is_not_an_ordering_edge` has been about since the
policies landed. Effects the fixed set *can* wire keep the generic
`pass.screen` and its palette sugar; the derived rows appear through
`document_registry` like any other node.

The resolve answers disocclusion by clamping, twice: the history is
pulled into the current frame's 3×3 neighbourhood box (shrunk halfway
toward the current pixel, so a *changing* shading field — a rotating
surface crossing world-space noise — is rejected fast instead of
trailing), and the blend falls off with the history's distance from the
**current pixel** relative to that box. Measuring against the box alone
is the bug that taught this: an unwritten ring's zeros are invisible to
a box whose dark end is zero, and a frame-one blend toward black is a
hole that rewrites itself every frame after. Motion blur is deliberately
dumber — eight taps back along the fragment's own velocity — and reads
linear radiance before the display transform, per ADR 0039.

### A velocity target's unwritten texel means zero motion

The document compiler clears a velocity-stage pass's target to
transparent black, not the pipeline's radiance clear colour. The frame's
clear colour is *light* (ADR 0039); a velocity buffer's empty value is
*no motion*, and a background carrying the clear colour as motion
reprojects every empty pixel a step sideways and bleeds the history
across every silhouette. The stage owns what its output means where
nothing drew.

## Alternatives considered

**A second material compilation per velocity pass** — the whole graph
compiled again under `wxsl_previous_frame`, two variants, two module
mounts. It duplicates every fragment's worth of generated code to reach
a fact one extra context constructor reaches, and the variant cache
would hold two sources whose only difference is a clock. Rejected for
the same reason the macro system pins values per module: one module, one
clock, and the previous context is the second clock.

**The previous transforms as vertex attributes** — the shape N10's web
column may yet pick for everyone. It would spend four attribute slots
per object on a fact only a velocity pass reads, and the storage array
costs one upload the frame was already doing. Revisit if the WebGL2
column forces the instance shape; the stage and the ABI functions do not
change either way.

**TAA as a screen graph** — the domain exists, and a resolve is a
material over the frame. But a resolve wants three image inputs and the
graph-authored screen ABI binds exactly one — the same hole M7 recorded
honestly and N3 closed for *compute* only. Landing the derived rows for
screen effects closes it for both; converting the resolve to a graph
then becomes pure authoring, with no machinery owed.

**Skipping the `history` field on the input and letting the document
wire the ring twice** — into the resolve and into its `history` socket,
with the history depth a property of the pass. The declaration is the
contract both sides compile against; a temporal input is a fact about
the *effect*, and a document that cannot say "last frame" without a
second spelling of the same wire is a document that cannot be checked.

## Consequences

* The plan's bar holds as a measurement, not a squint: `motion.rs`
  renders the spinning cube raw and under the resolve — the resolve's
  frame-to-frame difference is *lower* than the raw chain's (0.313 vs
  0.317) while staying within 0.02 of what the raw chain drew of the
  same instant; a still scene converges to the raw image exactly; a
  moving cube under the blur loses half its horizontal gradient energy.
  The velocity buffer itself is checked against the projection the test
  computes — translation, rotation, and camera motion, all exact.
* The gallery gains `taa` (the deferred chain with the resolve, the
  history ring the resolve itself writes) and `motion-blur` (the forward
  chain with the blur between the shading and the display transform) —
  both documents, no hand-built Rust, and the palette rows the pipeline
  canvas needed already exist.
* The corpus gate compiles every shipped material for the velocity stage
  as it did for the others; a material whose geometry spends the last
  two inter-stage locations is a named per-stage error, which no
  graph-level check could have seen.
* `written()` no longer counts a depth attachment loaded with
  `depth_write: false` as a write — testing against a prepass's depth is
  a read, and counting it as a write made a third depth-loading pass
  mutually dependent with the pass before it. The scheduler's cycle
  error said "a pass reading what it writes" about a frame that had
  none.
* A temporal pipeline without jitter converges to a slightly softer
  image, not an antialiased one — the history accumulates what the
  single sample saw. `Camera::jitter` is the host's half and is
  deliberately unexercised by the demos; a Halton sequence driven by the
  frame index is an application decision the ABI now supports.
* The known limitation, named: a surface whose *displacement* is
  time-driven answers for the previous frame through
  `previous_vertex_context` — but only the displacement; the material's
  *shading* changing under motion (noise fields, animated emissive) is
  smoothed by the clamp, not resolved by it, and trails for a few
  frames by design. Variance clipping or a change-detection channel is
  the escalation, and it lands with a consumer that needs it.
* If this changes, also update `AGENTS.md` (the stage bullet, the
  effects bullet, the gallery list), `docs/architecture.md`, and the
  glossary's *Velocity* entry.
