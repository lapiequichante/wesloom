# Dawn backend

`wxsl_render_cpp` is the offline, headless Dawn device backend (ADR 0051).
Conan 2 manages Dawn, nlohmann_json and stb; `conan.lock` pins recipes and
the local Dawn recipe pins its official release archive by SHA-256.
Requires CMake, a C++20 compiler and Conan. Cache/build outputs stay in `target/`.

## Render and verify

From the repository root, with a GPU adapter available:

```sh
bash dawn/tests/run_renderer_smoke.sh
```

This builds Dawn, exports the existing PBR/scene documents and Rust reference
images, renders both stock presets, and compares their pixels. It also checks
history rotation, once-only compute buffers, indirect geometry, stale layout
refusal and deliberately broken uploads. A missing adapter is an error, not
a passing parity check. Image thresholds are fixed in ADR 0051.

For separate build/export/render steps:

```sh
bash dawn/tests/build_renderer.sh
cargo run -p wxsl-ffi --example bake_docs -- --out target/dawn-assets/pbr --reference --probes
target/dawn-render/pbr_cube_cpp --headless --assets target/dawn-assets/pbr --verify
target/dawn-render/renderer_smoke target/dawn-assets/pbr
```

The renderer smoke script also exports `bake_docs --shadow-atlas --reference`:
a matte ground plane and cube under four shadow-casting directional lights.
It compares both paths against wgpu, exercising the atlas's 1024²/512² tiles,
viewport/scissor recording and generated light rectangles (ADR 0066).

Omit `--reference` to export without a device. `--scene`, `--graph`, `--width`
and `--height` customize export input; the two stock pipeline documents stay
shared. Mesh generation, draw selection, scheduling, layouts and shader
compilation all happen in Rust. C++ consumes JSON/WGSL/upload bytes and links
no Rust library. `--frames N` exercises multiple frames with fixed inputs.

This host supplies procedural meshes, default instance/material
values and a checker texture. File meshes, application uniform blocks,
additional vertex streams and general external imports are not implemented. Unknown
descriptors fail explicitly. `--pipeline PATH` adds a document compiled by the
shared compiler; window/editor integration is outside this headless backend.

### HDR environments (S3)

`pass.environment` in a pipeline document expands the shared Once prefilter
recipe, declaring diffuse/GGX frame cubes. `resource.color` with `imported: true`,
`precision: float` and label `source HDR` receives application-decoded linear RGB.
`Renderer::upload_environment_image` validates pixels, returns a reversible
float16 safety scale, and invalidates Once bakes on replacement. Pass that scale
to `set_environment_scale`. No file decoder lives in the renderer library.

The application uses its existing stb Radiance decoder; the exporter can produce
matching Rust references for the same local, non-redistributed HDR:

```sh
cargo run -p wxsl-ffi --example bake_docs -- --out target/dawn-assets/hdr \
  --probes --reference --hdri resources/hdri/lakeside_sunrise_1k.hdr
target/dawn-render/pbr_cube_cpp --assets target/dawn-assets/hdr \
  --pipeline hdr_forward_probe --hdri resources/hdri/lakeside_sunrise_1k.hdr --verify
```

The bundle also exports `hdr.forward.pipeline.json` and
`hdr.deferred.pipeline.json` as copyable documents. The background effect reads
linear colour, opaque depth and radiance; it fills clear-depth pixels from camera
rays before tonemap. Composite transparent layers after this opaque background.
`--hdr-label NAME` selects another authored import label. The probes use a
synthetic constant HDR when `--hdri` is absent, so CI needs no local asset.

## Runtime compilation (B4)

```sh
cargo build -p wxsl-ffi --lib
target/dawn-render/pbr_cube_cpp --assets target/dawn-assets/pbr \
    --compiler "$PWD/target/debug/libwxsl_ffi.dylib" --verify
```

Use `libwxsl_ffi.so` on Linux or `wxsl_ffi.dll` on Windows. The library is
loaded dynamically, not linked; it must match the generated C ABI version.
Runtime mode checks the scene/setup and recompiles bundle requests, using
no offline WGSL. Offline mode still needs no Rust deployment.

`wxsl::Compiler` copies metadata, WGSL, defaults and field tables before
freeing each foreign result. Its single-owner cache holds at most 64 successful
requests; keys/diagnostics/layouts remain Rust computations. `Renderer::compile_material`
compiles all stages before replacing a material. Same-layout graph/macro edits
are supported. Layout, setup or draw-selection changes require a new host bundle
and are refused by name. A failed edit retains the previous rendering.
`upload_material_params` accepts computed, padded bytes and changes no shader or
pipeline. Runtime calls are synchronous; there is no worker/editor API yet.

## Backend parity (B5)

```sh
bash dawn/tests/run_parity.sh
```

The curated list in `tests/scenes.txt` covers both stock presets on PBR and
scene_check, the gallery's actual exported single-pass document, and one sample
per ten stdlib categories (FXAA uses the shipped screen graph). Rust's corpus
gate still covers every node/stage; these samples add rendered agreement, not
exhaustive rendered coverage. The category-list test refuses missing categories.
The comparator keeps ADR 0051's thresholds unchanged. Deliberately omitted
C++ draw recording and corrupted uploads must fail that same comparator.

The runtime test removes WGSL files from its disposable bundle and checks edits,
cache hits, parameter writes, ABI/document errors and last-good rendering.
CI compiles on Linux/macOS without a GPU. To run the GPU job, register a trusted
self-hosted runner labelled `wxsl-gpu` (Linux/macOS, with Rust, CMake and a GPU),
then dispatch CI with `gpu=true`. Missing adapters fail the harness; the job
does not run pull-request code on a persistent runner. Local success does not
prove the remote runner is provisioned.

## Generated layouts

Edit `wxsl-core::host`, not `include/wxsl_host.h`. Then regenerate:

```sh
cargo run -p wxsl-frame --example export_host_header -- dawn/include/wxsl_host.h
cargo test -p wxsl-frame --test host_header
```

Rust/C/WXSL definitions come from the same table (ADR 0050). C/C++ compilers
assert offsets/sizes; exports carry the layout fingerprint and reject a stale
header before Dawn setup. The editor remains wgpu-only.

## C ABI harness

B2's dynamic-loading tests require neither Dawn nor a GPU.

From the repository root:

```sh
bash dawn/tests/run_abi_smoke.sh
```

The runner builds `wxsl-ffi`, generates native Rust shader fixtures, compiles
the same harness as C11 and C++11, then tests dynamic loading, both presets,
exact WGSL parity for all nine material stages, computed field tables and
named capability/version refusals. CMake uses the generated header at
`crates/wxsl-ffi/include/wxsl.h`; it does not link Rust into the harness.

For Windows or a custom Cargo target directory, run the equivalent manually:

```sh
cargo build -p wxsl-ffi --lib
cargo run -p wxsl-ffi --example abi_fixture -- target/dawn-abi/fixtures
cmake -S dawn -B target/dawn-abi \
  -DWXSL_FFI_LIBRARY=/absolute/path/to/wxsl_ffi.dll \
  -DWXSL_ABI_FIXTURES=/absolute/path/to/target/dawn-abi/fixtures
cmake --build target/dawn-abi
ctest --test-dir target/dawn-abi -C Debug --output-on-failure
```

API request shapes and ownership are documented in
`crates/wxsl-ffi/README.md`. Runtime cdylib integration is optional, not a
requirement of the offline renderer or this harness.
