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
| `wxsl-ffi`: data-only C ABI | `core`, `frame`, `lang`, `stdlib`, `gltf` | no (ADR 0049) |
| `wxsl-stdlib`: node library | `wxsl-core` (+ `wxsl-lang` at build time) | no |
| `wxsl-gltf`: glTF import, scene documents out | `core`, `frame` (the registry is an argument) | no (ADR 0080) |
| `wxsl-prt`: offline PRT bake (shadows, bounces), a CLI and graph rewiring | `core`, `gltf`, `stdlib` | no (ADR 0092) |
| `wxsl-render`: wgpu renderer and UI layer | `core`, `frame`, `lang` (+ `gltf` behind its feature) | yes |
| `wxsl-editor`: node editor, draws itself | `core`, `render`, `lang` | yes (ADR 0013) |
| `wxsl-mcp`: MCP server over editor commands | `core`, `editor`, `render` | yes (ADR 0083) |
| `wxsl`: facade, behind features | all of the above | via `render`/`editor` |

Boundaries that must hold (ADR 0002):

- `wxsl-core`, `wxsl-frame`, `wxsl-gltf` and `wxsl-ffi` never depend on
  wgpu, a GUI toolkit or the renderer; `wxsl-gltf` never on `wxsl-stdlib`. `wxsl-render` is only a dev-dependency of
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
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
bash scripts/public-api.sh                     # --bless after an intended API change
```

A change to a public item updates `api/<crate>.txt` with
`bash scripts/public-api.sh --bless` (pinned nightly, see the script). A
crate's root re-exports are its stable surface; module paths are unstable
(ADR 0078). Don't add a root re-export casually. Since 0.1.0, a change to
the stable surface (a root re-export, a document format, the C ABI) also
adds an entry under `[Unreleased]` in `CHANGELOG.md`.

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
| editor commands, undo, transcripts, scene picking (no device) | `cargo test -p wxsl-editor` |
| editor or UI behavior | `cargo test -p wxsl --features editor --test editor_frame` |
| a debug view, the pixel probe, the frame panel (ADR 0085) | `cargo test -p wxsl --test graph_to_wgsl` (forward views validate), `cargo test -p wxsl-render probe`, then `editor_frame`; the probe's layers and buffers are in `ibl` and `buffer_resources` |
| MCP protocol, tools, HTTP, path confinement (no device) | `cargo test -p wxsl-mcp`, then `cargo test -p wxsl --features mcp --test mcp` |
| a node's sockets, label or doc (the reference, ADR 0084) | `cargo run -p wxsl-mcp --example node_reference`, then `cargo test -p wxsl-mcp` |
| the authoring guide, or a diagnostic it quotes | `cargo test -p wxsl-mcp`, then `cargo test -p wxsl --features mcp --test mcp` |
| WebGL2/GL fallback (ADR 0075) | `--test downlevel_corpus` (no device), `--test downlevel`, the wasm check, then `node dawn/tests/web_smoke.mjs` |
| glTF import | `cargo test -p wxsl-gltf`, then `cargo test -p wxsl --features gltf --test gltf_import` |
| the PRT bake or its nodes (ADR 0092) | `cargo test -p wxsl-prt`, then `cargo test -p wxsl --test prt` |
| a picture | `cargo run -p wxsl --example pbr_cube -- --headless`, `gallery -- --screenshot`, or `viewer -- <demo> --screenshot out.png` |

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
  - Everything checkable is checked in the pure `FrameGraph::schedule`.
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
  `wxsl_render::scene`'s job, a loader beside the renderer, re-exported as
  `wxsl::scene` (ADR 0082).
- **Two implementations must agree.**
  - The MSDF generator (CPU and compute) is checked in `editor_frame`.
  - Forward and deferred are checked in `render_cube` and the other device
    tests.
  - The C ABI and wgpu are checked by the parity harness.
- **Editor commands** (ADR 0081). A document changes only through
  `Editor::apply(Command)`; a new gesture is a new `Command` variant
  emitted into the frame's requests, never a direct mutation. Undo is
  by snapshot, so a command needs no hand-written inverse.
- **Agents** (ADR 0083). An MCP tool is a plan in `wxsl_mcp::tools` over
  commands and queries; a capability it lacks is a new `Command` or
  `Query` first. Every path a tool takes goes through `Root`. What an
  agent reads is generated or checked: the node reference
  (`docs/reference/nodes.md`) and the authoring guide
  (`crates/wxsl-mcp/authoring.md`) (ADR 0084).
- **Debug views** (ADR 0085). A surface view is a `BASE_DEBUG_VIEWS` row
  with its expression in both `lighting::debug_surface` (forward) and the
  lighting pass's arm. A view of the frame is a `wxsl_editor::views`
  variant (an editor effect, or a pass list it installs), never a
  document node. The probe is generic over the pass list: no view or pass
  gets its own readback.
- **One editor, several canvas modes.** The pipeline canvas and the scene
  mode are modes of the same editor over one renderer (ADRs 0043, 0082),
  not second editors.

## Where things are

- `crates/wxsl/examples/`:
  - `pbr_cube` is the first demo (`--help` lists its options);
  - `editor` is the node editor;
  - `gallery` shows every pipeline, `--screenshot` for PNGs (one module
    per concern under `examples/gallery/`);
  - `viewer` opens a glTF file under an HDR (`--features gltf`);
  - `scene_check` runs the capability check;
  - `cinema` runs the post chain.
  - `editor --features mcp -- --mcp <PORT>` (or `--mcp-stdio --headless`)
    lets an agent drive the editor. `crates/wxsl/tests/transcripts/`
    holds recorded agent sessions, replayed by `--test mcp`.
- `resources/glb/`: Khronos sample models and their licenses.
- `cargo run -p wxsl-prt --release -- <model.glb>` bakes the PRT sidecars
  (`<model>.gray.prt`, `<model>.color.prt`) the `viewer` picks up.
- `crates/wxsl/assets/`:
  - `pbr_cube.wxsl.json` is the commented node format;
  - `*.scene.json` are scene documents.
- `docs/architecture.md` (the overall design), `docs/glossary.md` (terms),
  `docs/adr/` (decisions), `docs/rust.md` and `docs/cpp.md` (using the
  libraries), `docs/reference/nodes.md` (every node, generated),
  `docs/history/` (finished plans).
