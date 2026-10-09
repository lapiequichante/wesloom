#!/usr/bin/env bash
set -euo pipefail
repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_dir"
export CONAN_HOME="$repo_dir/target/conan"
conan profile detect --exist-ok
conan export dawn/conan/dawn
conan install dawn -of target/dawn-conan -s compiler.cppstd=20 --build=missing --lockfile=dawn/conan.lock
cmake -S dawn -B target/dawn-render \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_TOOLCHAIN_FILE="$repo_dir/target/dawn-conan/build/Release/generators/conan_toolchain.cmake" \
    -DWXSL_BUILD_RENDERER=ON -DWXSL_BUILD_ABI_TESTS=OFF
cmake --build target/dawn-render --parallel
