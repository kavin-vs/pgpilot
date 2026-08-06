#!/usr/bin/env bash
# Builds and packages a single release target. Called by
# .github/workflows/release.yml (one leg of its matrix) and by
# scripts/release.sh (the manual release path) — same logic either way,
# so CI and a local run never drift apart.
#
# Usage: scripts/build-target.sh <target-triple> [tag]
# Prints the path to the packaged .tar.gz on stdout.
set -eu

target="${1:?usage: build-target.sh <target-triple> [tag]}"
tag="${2:-v$(grep -m1 '^version' Cargo.toml | sed -E 's/version = "(.*)"/\1/')}"
bin_name=pgpilot

rustup target add "$target" >/dev/null 2>&1 || true
cargo build --release --locked --target "$target"

mkdir -p dist
archive="dist/${bin_name}-${tag}-${target}.tar.gz"
tar czf "$archive" -C "target/$target/release" "$bin_name"
echo "$archive"
