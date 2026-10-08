#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_dir"
cargo build -p wxsl-ffi --lib
cargo run -p wxsl-ffi --example abi_fixture -- target/dawn-abi/fixtures
case "$(uname -s)" in
    Darwin) library="$repo_dir/target/debug/libwxsl_ffi.dylib" ;;
    Linux) library="$repo_dir/target/debug/libwxsl_ffi.so" ;;
    *) echo "Use CMake directly with the platform's cdylib path (see dawn/README.md)." >&2; exit 1 ;;
esac
cmake -S dawn -B target/dawn-abi \
    -DWXSL_FFI_LIBRARY="$library" \
    -DWXSL_ABI_FIXTURES="$repo_dir/target/dawn-abi/fixtures"
cmake --build target/dawn-abi
ctest --test-dir target/dawn-abi --output-on-failure
