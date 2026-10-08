# Dawn backend work

B2 provides the dynamic C ABI harness here. The Dawn renderer itself is B3
and is not implemented yet; these tests need neither Dawn nor a GPU.

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
`crates/wxsl-ffi/README.md`. B3's offline-first device binary will consume
exported JSON/WGSL/layout artifacts; using the runtime cdylib in that renderer
is B4, not a requirement imposed by this harness.
