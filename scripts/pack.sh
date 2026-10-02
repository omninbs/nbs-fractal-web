#!/usr/bin/env bash
# Build the release binary and wrap it in a self-extracting zstd launcher.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

level="${ZSTD_LEVEL:-19}"
out="${OUT:-dist/nbs-fractal-web}"

cc -O2 -s -o target/sfx tools/sfx.c -lzstd
cargo build --release

mkdir -p "$(dirname "$out")"
target/sfx pack target/release/nbs-fractal-web "$out" "$level"

printf 'plain    %10d B\n' "$(stat -c%s target/release/nbs-fractal-web)"
printf 'packed   %10d B  %s\n' "$(stat -c%s "$out")" "$out"
