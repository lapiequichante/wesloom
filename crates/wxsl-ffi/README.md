# wxsl-ffi

The device-free C ABI (ADR 0049): JSON documents in; plans, WGSL and
computed layouts out. It links core, frame, the compiler and the standard
library, never wgpu or the editor. `wxsl-render` is only a dev-dependency,
used for parity tests.

Build it with `cargo build -p wxsl-ffi --lib` and include the generated
`include/wxsl.h`.

**The calls, ownership rules, request and response shapes are documented in
[`docs/cpp.md`](../../docs/cpp.md).**

## Verification and header updates

```sh
cargo test -p wxsl-ffi                     # header pin, API tests, release gate
cargo run -p wxsl-ffi --example export_header -- crates/wxsl-ffi/include/wxsl.h
bash dawn/tests/run_abi_smoke.sh           # C11/C++11 harness, no GPU
cargo tree -p wxsl-ffi -e normal           # no renderer or GUI
```

`build.rs` generates the header from `src/ffi.rs`. Doc comments become
`/** */` comments, and an unsupported signature type fails the build. A test
compares the checked-in copy with the generated one.

Review ABI compatibility before changing a signature or the plan schema.
Field offsets computed from a material are data, not a change to C struct
layouts.
