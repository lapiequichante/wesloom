# AGENTS.md

Guidance for coding agents. Before a non-trivial change, read this file,
`docs/architecture.md`, and the ADRs for the area (`docs/adr/README.md`).
The current plan is the newest `plan*.md`.

## Working style

- Make the smallest change that fully solves the request. Reuse existing
  modules, APIs, patterns and tests before adding abstractions or files.
- Don't refactor, reformat or rename unrelated code. Keep public behavior
  unless the request requires changing it.
- Make reasonable assumptions and proceed. Ask only when a missing choice
  would change the design or risk data loss.
- Verify proportionally: focused tests first, then the broader checks the
  changed boundary needs.
- Keep explanations and comments concise. Document decisions and
  invariants, not mechanics.

## The workspace

`wxsl` builds shader graphs in WXSL (WGSL extended) and renders them through
data-driven forward/deferred pipelines on wgpu, or on Dawn through a C ABI.
It ships a node library and a self-drawn visual editor. Each module's doc
comment says what it holds and which ADR governs it; that is the source of
truth.

| Crate | Depends on (in-workspace) | wgpu? |
|---|---|---|
| `wxsl-core`: graph model, codegen, documents | nothing | no |
| `wxsl-lang`: WXSL → WGSL compiler | `wxsl-core` | no |
| `wxsl-frame`: frame plans, scheduling, presets, effects | `wxsl-core` | no (ADR 0048) |
| `wxsl-ffi`: data-only C ABI | `core`, `frame`, `lang`, `stdlib` | no (ADR 0049) |
| `wxsl-stdlib`: node library | `wxsl-core` (+ `wxsl-lang` at build time) | no |
| `wxsl-render`: wgpu renderer and UI layer | `core`, `frame`, `lang` | yes |
| `wxsl-editor`: node editor, draws itself | `core`, `render`, `lang` | yes (ADR 0013) |
| `wxsl`: facade, behind features | all of the above | via `render`/`editor` |

Boundaries that must hold (ADR 0002):

- `wxsl-core`, `wxsl-frame` and `wxsl-ffi` never depend on wgpu, a GUI
  toolkit or the renderer. `wxsl-render` is only a dev-dependency of
  `wxsl-ffi`, used for parity tests.
- `wxsl-core` and `wxsl-render` never depend on `wxsl-editor` or
  `wxsl-stdlib`.
- `wxsl-stdlib → wxsl-lang` stays a **build**-dependency (ADR 0020).
- No GUI toolkit anywhere; the editor draws itself with the renderer.
- `dawn/` consumes offline exports, or loads the same data ABI at runtime
  (ADRs 0051, 0052). Never duplicate variant keys, shader compilation or
  layout rules in C++.

## ADRs

If an ADR covers the area and your change conflicts with it, say so and
propose superseding it; don't work around it quietly. A nontrivial
architectural choice gets an ADR first: a new public boundary, crate,
heavy dependency, or a change to the feature matrix.

To write one, copy `docs/adr/template.md` to the next number, keep it short
(context, decision, consequences), and link it from `docs/adr/README.md`.
Mark the replaced ADR "Superseded by NNNN". To record drift, edit the
existing ADR's Consequences section rather than diverging silently.

## Licensing

Workspace code is MIT/Apache-2.0. Ports into `wxsl-stdlib` are allowed only
from audited MIT, Apache-2.0 or BSD-2/3-Clause sources (ADR 0053):

- check the exact file and its provenance at a pinned commit;
- keep the copyright and license text;
- note the source, symbol, revision and your changes in the shader;
- update the root and packaged `NOTICE`.

Any other license needs explicit review. `refs/` is research material,
never a build input. Details: `crates/wxsl-stdlib/shaders/README.md`.

## Commands

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo check -p wxsl --no-default-features      # graph model only
cargo check -p wxsl --features editor          # pulls wxsl-render transitively
cargo tree -p wxsl-frame -e normal             # only wxsl-core in-workspace
cargo tree -p wxsl-ffi -e normal               # no renderer/GUI
cargo tree -p wxsl --no-default-features -e normal | grep -E 'wgpu|winit|egui'  # must print nothing
```

A change to crate or feature boundaries is not finished until the
`--no-default-features` and `--features editor` checks pass, as well as
`--all-features`.

### Which test to run

Iterate with the narrowest check, then run what the changed boundary needs.
Each test file's header says what it covers.

| You changed | Run |
|---|---|
| graph, typing, codegen, documents | `cargo test -p wxsl-core` |
| scheduling, presets, effects, capabilities, frame layouts | `cargo test -p wxsl-frame` |
| stdlib or generated shader sources (corpus gate, ADR 0029) | `cargo test -p wxsl-stdlib --test lighting_models` |
| a node's WXSL, every node on every stage | `cargo test -p wxsl --test graph_to_wgsl` |
| C ABI, generated header, compiler/layout parity | `cargo test -p wxsl-ffi`, then `bash dawn/tests/run_abi_smoke.sh` |
| anything shipped (release gate, ADR 0077) | `cargo test -p wxsl-ffi --test release_gate` |
| fixed host layouts, Dawn recording | `cargo test -p wxsl-frame --test host_header`, then `bash dawn/tests/run_renderer_smoke.sh` |
| Dawn runtime or parity | `cargo test -p wxsl-ffi --example bake_docs`, then `bash dawn/tests/run_parity.sh` |
| wgpu adapters, shader variants, renderer UI | `cargo test -p wxsl-render` |
| a material or pipeline feature on a device | the matching `cargo test -p wxsl --test <name>` |
| `wxsl_core::resources` (uniform or instance layout) | `material_resources` and `material_geometry` (they share `tests/probe`) |
| editor or UI behavior | `cargo test -p wxsl --features editor --test editor_frame` |
| WebGL2/GL fallback (ADR 0075) | `--test downlevel_corpus` (no device), `--test downlevel`, the wasm check, then `node dawn/tests/web_smoke.mjs` |
| a picture | `cargo run -p wxsl --example pbr_cube -- --headless`, or `gallery -- --screenshot` |

The wasm check is
`cargo check -p wxsl --features editor,webgl --target wasm32-unknown-unknown`.

GPU tests skip when there is no adapter, and a skip is not a pass. CI has no
GPU, so anything that can be tested without a device is factored so that it
is: the scheduler, layouts, atlas packing, text layout, hit-testing and
input. Shared layouts and ABI tables deserve both a focused test and a
device test.

## Conventions

The ADR named on each line holds the reasoning.

- **Safety.** `unsafe` is forbidden in `wxsl-core` and avoided elsewhere.
  Foreign-memory access stays in `wxsl-ffi/src/ffi.rs`, whose header is
  test-pinned.
- **Generated code.**
  - Logic lives in Rust and shader text in `.wxsl` templates; generators
    fill holes (ADR 0031).
  - Never edit generated text: the lighting pass, the G-buffer, material
    modules, host layouts.
  - Host layouts are edited in `wxsl_core::host` (ADR 0050). Regenerate
    the C header with
    `cargo run -p wxsl-frame --example export_host_header -- dawn/include/wxsl_host.h`.
  - The shader ABI has two halves to edit together: `wxsl_core::abi` and
    `crates/wxsl-stdlib/shaders/wxsl/`.
- **Nodes.** The `.wxsl` file *is* the node (ADR 0020). It lives in
  `crates/wxsl-stdlib/shaders/<category>/` and has:
  - one `fn` per file;
  - a label line and a documentation paragraph;
  - one parameter per line, each with `// … @default <value>`.
  A knob that cannot be a socket is a macro variable (`wxsl_core::macros`),
  never string substitution.
- **Pipelines are data** (ADRs 0021, 0033).
  - The stock pipelines are documents in
    `crates/wxsl-frame/assets/presets/`; never edit generated pass lists.
  - Adding a pass means adding a `PassDesc`, never a `begin_render_pass`.
  - Everything checkable is checked in the pure `RenderGraph::schedule`.
  - A document error names the document node.
- **Effects are data** (ADRs 0034, 0035, 0042).
  - Adding an effect means an `Effect` row plus its shader, never a
    `PassKind` arm.
  - A knob the host tunes at runtime is a declared parameter. A structural
    knob stays a `const`.
  - A new post effect is a screen graph first (ADR 0040).
- **Policies.** A non-default policy writes only stable storage
  (ADR 0035).
- **Material stages** are rows in `abi::MATERIAL_STAGES` (ADR 0022). The
  exceptions are `velocity` (ADR 0046) and the peel stages
  (ADRs 0047, 0057).
- **Lighting models** are a `.wxsl` file under `shaders/lighting/models/`
  plus a `DEFAULT_MODELS` entry (ADR 0028).
- **Material features** are a `FEATURES` row plus a module under
  `shaders/wxsl/features/` (ADR 0037).
- **A material's configuration** is one `MaterialConfig` (ADR 0038). A new
  knob is a field there; `resolve` is the only place a name becomes an id.
- **Bakes** are graph declarations, never nodes (ADR 0045). The table is
  host-owned, and the bake pass is declared before the pass that samples
  it.
- **Identity and versions** (ADR 0044).
  - Registry ids are `package.name`, and a registration without a package
    is refused.
  - Documents carry `version` and `abi`: absent means this build, newer is
    refused by name.
  - Scene checks report every mismatch.
- **Linear light.** Everything is linear radiance until the `tonemap`
  effect (ADR 0039). Put no display transform anywhere else.
- **Domains.** A graph's domain is surface, screen or document
  (ADR 0040). Filter with `NodeRegistry::in_domain`; never add a second
  registry or palette.
- **Scene, environment, draw list.** A scene is what exists, an
  environment is camera and lights, and a draw list is what a frame
  submits. The renderer takes a draw list. Translating a scene into one is
  the facade's job (`wxsl::scene`).
- **Two implementations must agree.**
  - The MSDF generator (CPU and compute) is checked in `editor_frame`.
  - Forward and deferred are checked in `render_cube` and the other device
    tests.
  - The C ABI and wgpu are checked by the parity harness.
- **One editor, several canvas modes.** The pipeline canvas is a mode of
  the same editor (ADR 0043), not a second editor.

## Where things are

- `crates/wxsl/examples/`:
  - `pbr_cube` is the first demo (`--help` lists its options);
  - `editor` is the node editor;
  - `gallery` shows every pipeline, `--screenshot` for PNGs;
  - `scene_check` runs the capability check;
  - `cinema` runs the post chain.
- `crates/wxsl/assets/`:
  - `pbr_cube.wxsl.json` is the commented node format;
  - `*.scene.json` are scene documents.
- `docs/architecture.md` (the overall design), `docs/glossary.md` (terms),
  `docs/adr/` (decisions).
