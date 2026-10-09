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

Omit `--reference` to export without a device. `--scene`, `--graph`, `--width`
and `--height` customize export input; the two stock pipeline documents stay
shared. Mesh generation, draw selection, scheduling, layouts and shader
compilation all happen in Rust. C++ consumes JSON/WGSL/upload bytes and links
no Rust library. `--frames N` exercises multiple frames with fixed inputs.

This first offline host supplies procedural meshes, default instance/material
values and a checker texture. File meshes, application uniform blocks,
additional vertex streams, external imports and arbitrary pipeline-document
input are not implemented. Unknown descriptors fail explicitly. Runtime
compilation is B4; window/editor integration and B5's wider corpus are not
included here.

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
`crates/wxsl-ffi/README.md`. Runtime cdylib integration is B4, not a requirement
of the offline renderer or this harness.
