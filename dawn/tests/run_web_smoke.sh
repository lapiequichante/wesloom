#!/usr/bin/env bash
# The browser column (ADR 0075), in headless Chrome:
#  * the wgpu renderer (`examples/web.rs`) on WebGL2 and on WebGPU, forward
#    and deferred, 120 frames each with no validation error;
#  * the Dawn renderer built with Emscripten, on WebGPU, verifying a full
#    and a webgl2-profile bundle against the wgpu references.
# Requires Google Chrome (or CHROME=...), Node 22+, python3, and the GPU the
# reference images were rendered on.
set -euo pipefail
repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_dir"
smoke() { node dawn/tests/web_smoke.mjs "$@"; }
serve() { python3 -m http.server -d "$1" "$2" > /dev/null 2>&1 & echo $!; }

# wgpu: the demo cube.
cargo install wasm-bindgen-cli --version 0.2.128 --locked --root target/tools --quiet
cargo build -p wxsl --example web --features webgl --target wasm32-unknown-unknown --release
target/tools/bin/wasm-bindgen --target web --out-dir target/web --no-typescript \
    target/wasm32-unknown-unknown/release/examples/web.wasm
cp crates/wxsl/examples/web.html target/web/index.html
server=$(serve target/web 8765)
trap 'kill $server ${dawn_server:-} 2> /dev/null' EXIT
sleep 1
for query in "webgl" "webgl&deferred"; do
    smoke "http://localhost:8765/?$query" "target/web/$query.png" --until "rendered 120 frames"
done
for query in "" "deferred"; do
    smoke "http://localhost:8765/?$query" "target/web/webgpu-$query.png" \
        --until "rendered 120 frames · profile full" --enable-unsafe-webgpu
done

# Dawn: the offline renderer, compiled with Emscripten.
for profile in full webgl2; do
    assets="target/dawn-assets/web-$profile"
    cargo run -p wxsl-ffi --example bake_docs -- --out "$assets" --reference --profile "$profile"
    bash dawn/tests/build_wasm.sh "$assets"
    dawn_server=$(serve target/dawn-web 8766)
    sleep 1
    smoke http://localhost:8766/ "target/dawn-web/$profile.png" --until "exit 0" \
        --enable-unsafe-webgpu
    kill "$dawn_server"
done
