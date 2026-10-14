# 77. Gate a release on every shipped shader, compiled through the C ABI

Date: 2026-10-14

Status: Accepted

## Context

Plan3's N8 asks for two things before a release: a decision on what a
shipped application embeds, and a production gate that compiles every
shader a release can be asked for, "with minimal libs linked", so a release
cannot ship a generator that fails on a driver.

The format half is already decided by ADR 0049. Authoring and runtime
compilation embed versioned JSON and WXSL. An offline, device-only
application embeds exported plan JSON, WGSL and layouts, without the
compiler. Nothing here changes that.

The gate half was open. The corpus gate (`wxsl-stdlib`'s `lighting_models`)
checks the generators over their telling combinations, `graph_to_wgsl`
sweeps the node library, and `downlevel_corpus` translates to GLSL ES. But
`wxsl-lang` emits WGSL without validating it. Only wgpu, on a device, or a
`wxsl` test linking wgpu ran naga over a release's actual configurations.
And no check went through the surface a non-Rust host actually calls.

## Decision

`crates/wxsl-ffi/tests/release_gate.rs` is the release gate. It reaches
only `wxsl_ffi::api`, the code behind the C ABI, so the binary links what a
Dawn or C++ host loads: `wxsl-core`, `wxsl-frame`, `wxsl-lang` and
`wxsl-stdlib`, never `wxsl-render` or `wgpu`. `naga` is a dev-dependency
used only to validate the output.

* **Configurations crossed:** both device profiles (full and WebGL2), the
  default and the widest lighting set, and no feature channel or each
  shipped one.
* **What it compiles:** for each configuration and each shipped preset,
  `api::pipeline` builds the plan. Every effect the plan's passes name goes
  through `api::shader`. Every material stage they name goes through
  `api::material`, for every shipped material document and every enabled
  model.
* **How it validates:** each WGSL result is validated by naga with no
  optional capabilities. Under WebGL2 every entry point is also translated
  to GLSL ES 3.00, with the pass-group depth-texture check
  `downlevel_corpus` applies.
* All failures are collected and reported by configuration, not just the
  first.

It runs in `cargo test --workspace`, so CI's test job already runs it.

## Alternatives considered

**A dedicated release binary.** It would run the same loop outside the test
harness, with nothing gained over a test and one more entry point to keep
working.

**Validating inside `wxsl-lang`.** Every compile, including the editor's
live ones and the C ABI's, would carry naga. The application's device
validates anyway; the gate only has to prove ahead of time that it will
accept the output.

**Extending the wxsl tests.** They link wgpu, so they cannot show what the
minimal surface produces on its own.

## Consequences

* A new preset, effect, profile, lighting model, feature or shipped material
  document is swept automatically, as long as it is reachable from the
  lists in the test (`PRESETS`, `MATERIALS`, `DEFAULT_MODELS`, `FEATURES`).
  A new shipped preset or material document must be added to those lists.
* Today the gate compiles 424 shaders in about six seconds (debug build). A
  deliberately mistyped line in `tonemap.wxsl` failed it in all 16
  configurations that run tonemap.
* Not covered: effects that only an application registers (bakes,
  `BRDF_LUT` and the other demo effects), and macro combinations outside
  the configurations above. Those stay with `downlevel_corpus` and the
  corpus gate.
* Native validation by Tint or a driver still needs a device; that is
  `run_parity.sh`'s job.
