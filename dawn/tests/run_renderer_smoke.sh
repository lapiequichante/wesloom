#!/usr/bin/env bash
set -euo pipefail
repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_dir"
bash dawn/tests/build_renderer.sh
cargo run -p wxsl-ffi --example bake_docs -- --out target/dawn-assets/pbr --reference --probes
cargo run -p wxsl-ffi --example bake_docs -- --out target/dawn-assets/scene \
    --scene crates/wxsl/assets/scene_check.scene.json --reference
target/dawn-render/pbr_cube_cpp --headless --assets target/dawn-assets/pbr --out target/dawn-images/pbr --verify
target/dawn-render/pbr_cube_cpp --headless --assets target/dawn-assets/scene --out target/dawn-images/scene --verify
target/dawn-render/renderer_smoke target/dawn-assets/pbr
