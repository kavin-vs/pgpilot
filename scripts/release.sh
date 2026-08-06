#!/usr/bin/env bash
# Manual release trigger — for when you don't want to wait on/rely on
# .github/workflows/release.yml (CI is down, testing packaging before a
# real tag, etc). Builds whatever targets this machine can produce
# natively via scripts/build-target.sh (the same script CI's build job
# calls) and publishes them to the GitHub release for the tag matching
# Cargo.toml's current version.
#
# Requires that tag to already exist (git tag vX.Y.Z && git push origin
# vX.Y.Z) — pushing a tag is a deliberate, visible step this script does
# NOT take for you, matching the release-pgpilot skill's "ask before
# pushing" rule. This script only builds + publishes.
#
# True cross-platform builds (Linux/Windows from a Mac) aren't attempted
# here — that needs `cross`/Docker, which isn't set up in this repo since
# CI's native per-OS runners already cover it. This script only builds
# what the current OS can natively produce; run it on the OS(es) you want
# assets for, or just let CI handle the rest.
set -eu
cd "$(dirname "$0")/.."

version=$(grep -m1 '^version' Cargo.toml | sed -E 's/version = "(.*)"/\1/')
tag="v$version"

echo "==> Release $tag"

if [ -n "$(git status --porcelain)" ]; then
  echo "error: working tree is not clean — commit or stash first" >&2
  exit 1
fi

if ! git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  echo "error: tag $tag doesn't exist yet." >&2
  echo "  git tag $tag && git push origin $tag" >&2
  echo "(that also triggers CI to do this same build+publish — this script is only needed as a manual fallback)" >&2
  exit 1
fi

echo "==> Preflight: cargo build + clippy"
cargo build --release
cargo clippy --all-targets

case "$(uname -s)" in
  Darwin) targets=(aarch64-apple-darwin x86_64-apple-darwin) ;;
  Linux) targets=(x86_64-unknown-linux-gnu) ;;
  *)
    echo "error: no release target known for $(uname -s) — build via CI instead" >&2
    exit 1
    ;;
esac

echo "==> Building: ${targets[*]}"
assets=()
for t in "${targets[@]}"; do
  assets+=("$(scripts/build-target.sh "$t" "$tag")")
done
printf 'built: %s\n' "${assets[@]}"

if ! command -v gh >/dev/null 2>&1; then
  echo "==> gh CLI not found — assets are in dist/, upload them to the release manually"
  exit 0
fi

echo "==> Publishing to GitHub release $tag"
if gh release view "$tag" >/dev/null 2>&1; then
  gh release upload "$tag" "${assets[@]}" --clobber
else
  gh release create "$tag" "${assets[@]}" --generate-notes --title "$tag"
fi
