#!/usr/bin/env bash
# Check (or with --bless, rewrite) the public-API snapshots in api/ (ADR 0078).
#
# Needs the pinned nightly and tool:
#   rustup toolchain install nightly-2026-10-13 --profile minimal
#   cargo install cargo-public-api --version 0.52.0 --locked
set -euo pipefail

TOOLCHAIN=nightly-2026-10-13
CRATES=(wxsl-core wxsl-lang wxsl-frame wxsl-stdlib wxsl-render wxsl-editor wxsl-ffi wxsl)

cd "$(dirname "$0")/.."
bless=false
[[ "${1:-}" == "--bless" ]] && bless=true

failed=()
for crate in "${CRATES[@]}"; do
    current=$(cargo "+$TOOLCHAIN" public-api -p "$crate" -s --all-features 2>/dev/null)
    snapshot="api/$crate.txt"
    if $bless; then
        printf '%s\n' "$current" > "$snapshot"
    elif ! diff -u "$snapshot" <(printf '%s\n' "$current") > /dev/null; then
        diff -u "$snapshot" <(printf '%s\n' "$current") || true
        failed+=("$crate")
    fi
done

if (( ${#failed[@]} )); then
    echo "public API changed: ${failed[*]}" >&2
    echo "if intended, run: bash scripts/public-api.sh --bless" >&2
    exit 1
fi
