#!/usr/bin/env bash
set -euo pipefail
repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_dir"
bash dawn/tests/build_renderer.sh
cargo build -p wxsl-ffi --lib
case "$(uname -s)" in
    Darwin) compiler="$repo_dir/target/debug/libwxsl_ffi.dylib" ;;
    Linux) compiler="$repo_dir/target/debug/libwxsl_ffi.so" ;;
    *) echo "Use runtime_smoke manually on Windows; see dawn/README.md" >&2; exit 1 ;;
esac
mkdir -p target/dawn-assets
cargo run -p wxsl --example gallery -- --export-pipeline target/dawn-assets/gallery.pipeline.json
while read -r scene_name mode input pipeline || [[ -n "$scene_name" ]]; do
    args=(--out "target/dawn-assets/$scene_name" --reference)
    case "$mode" in
        default) args+=(--probes) ;;
        scene) args+=(--scene "$input") ;;
        pipeline) args+=(--pipeline "$input") ;;
        sample) args+=(--sample "$input" --width 192 --height 144) ;;
        *) echo "Unknown fixture mode: $mode" >&2; exit 1 ;;
    esac
    cargo run -p wxsl-ffi --example bake_docs -- "${args[@]}"
    verify=(--assets "target/dawn-assets/$scene_name" --out "target/dawn-images/$scene_name" --verify)
    if [[ "$pipeline" != all ]]; then verify+=(--pipeline "$pipeline"); fi
    target/dawn-render/pbr_cube_cpp "${verify[@]}"
    if [[ "$scene_name" == pbr ]]; then
        target/dawn-render/renderer_smoke target/dawn-assets/pbr
        target/dawn-render/runtime_smoke target/dawn-assets/pbr "$compiler"
        target/dawn-render/pbr_cube_cpp --assets target/dawn-assets/pbr --compiler "$compiler" \
            --out target/dawn-images/runtime --verify
    fi
done < dawn/tests/scenes.txt
