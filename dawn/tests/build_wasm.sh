#!/usr/bin/env bash
# Build the Dawn renderer for the browser (ADR 0075): Emscripten and the
# pinned Dawn release's emdawnwebgpu port, with an exported bundle
# preloaded at /assets. Output: target/dawn-web/{index.html,pbr_cube_cpp.*}.
set -euo pipefail
repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_dir"
assets="${1:-target/dawn-assets/pbr}"
release=v20261008.181332
port="$repo_dir/target/emdawnwebgpu/emdawnwebgpu_pkg"
if [ ! -f "$port/emdawnwebgpu.port.py" ]; then
    mkdir -p target/emdawnwebgpu
    curl -sSL -o target/emdawnwebgpu/pkg.zip \
        "https://github.com/google/dawn/releases/download/$release/emdawnwebgpu_pkg-$release.zip"
    echo "3b3e659150f4622ec7d57a614c9be444b4af2703080440a02178cb9b244720c8  target/emdawnwebgpu/pkg.zip" \
        | shasum -a 256 -c -
    unzip -q -o target/emdawnwebgpu/pkg.zip -d target/emdawnwebgpu
fi
if [ ! -f target/emsdk/emsdk_env.sh ]; then
    git clone -q --depth 1 https://github.com/emscripten-core/emsdk.git target/emsdk
    target/emsdk/emsdk install latest
    target/emsdk/emsdk activate latest
fi
# shellcheck disable=SC1091
source target/emsdk/emsdk_env.sh > /dev/null
# Header-only dependencies, from the native build's Conan cache.
bash dawn/tests/build_renderer.sh > /dev/null
json="$repo_dir/$(dirname "$(dirname "$(find target/conan -path '*nlohmann/json.hpp' | head -1)")")"
stb="$repo_dir/$(dirname "$(find target/conan -name stb_image.h | head -1)")"
emcmake cmake -S dawn -B target/dawn-wasm -DCMAKE_BUILD_TYPE=Release \
    -DWXSL_BUILD_RENDERER=ON -DWXSL_BUILD_ABI_TESTS=OFF \
    -DWXSL_EMDAWNWEBGPU="$port" -DWXSL_HEADER_DIRS="$json;$stb" \
    -DWXSL_WEB_ASSETS="$repo_dir/$assets"
cmake --build target/dawn-wasm --parallel
mkdir -p target/dawn-web
cp target/dawn-wasm/pbr_cube_cpp.{js,wasm,data} dawn/web/index.html target/dawn-web/
