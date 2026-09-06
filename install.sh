#!/usr/bin/env sh
set -eu

REPO="kavin-vs/pgpilot"
BIN_NAME="pgpilot"
INSTALL_DIR="${PGPILOT_INSTALL_DIR:-$HOME/.local/bin}"

os=$(uname -s)
arch=$(uname -m)

case "$os" in
  Darwin)
    case "$arch" in
      arm64) target="aarch64-apple-darwin" ;;
      x86_64) target="x86_64-apple-darwin" ;;
      *) echo "unsupported mac arch: $arch" >&2; exit 1 ;;
    esac
    ;;
  Linux)
    target="x86_64-unknown-linux-gnu"
    ;;
  *)
    echo "unsupported OS: $os — download a release from https://github.com/$REPO/releases" >&2
    exit 1
    ;;
esac

# Resolve via the plain releases/latest redirect, not api.github.com --
# the API is rate-limited to 60 unauthenticated requests/hour per IP,
# which a shared NAT/CI runner can exhaust in normal use; this redirect
# is served by github.com itself and isn't subject to that limit.
latest=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")
latest=${latest##*/}
if [ -z "$latest" ] || [ "$latest" = "latest" ]; then
  echo "could not determine latest release — see https://github.com/$REPO/releases" >&2
  exit 1
fi
url="https://github.com/$REPO/releases/download/$latest/$BIN_NAME-$latest-$target.tar.gz"

echo "Installing $BIN_NAME $latest for $target..."
tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT
curl -fsSL "$url" -o "$tmpdir/$BIN_NAME.tar.gz"
tar -xzf "$tmpdir/$BIN_NAME.tar.gz" -C "$tmpdir"
mkdir -p "$INSTALL_DIR"
mv "$tmpdir/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
chmod +x "$INSTALL_DIR/$BIN_NAME"

echo "Installed to $INSTALL_DIR/$BIN_NAME"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "Add this to your shell profile: export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
