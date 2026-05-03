#!/usr/bin/env bash
#
# Download a pre-built Oxide compiler for the host platform and install
# it to ~/.oxide/bin/oxide. Supported hosts:
#
#   - macOS aarch64  → aarch64-apple-darwin
#   - Linux x86_64   → x86_64-unknown-linux-gnu
#
# Usage:
#   curl -sSf https://oxide.cwang.io/install.sh | sh
#   ./install.sh                                       # from a clone
#
# Env overrides:
#   OXIDE_DIST_URL   default: https://oxide.cwang.io/dist
#   OXIDE_HOME       default: $HOME/.oxide

set -euo pipefail

BASE_URL="${OXIDE_DIST_URL:-https://oxide.cwang.io/dist}"
INSTALL_DIR="${OXIDE_HOME:-$HOME/.oxide}"
INSTALL_BIN="$INSTALL_DIR/bin"

HOST_OS="$(uname -s)"
HOST_ARCH="$(uname -m)"
case "$HOST_OS/$HOST_ARCH" in
    Darwin/arm64)
        TRIPLE="aarch64-apple-darwin"
        ;;
    Linux/x86_64)
        TRIPLE="x86_64-unknown-linux-gnu"
        ;;
    *)
        echo "error: unsupported host $HOST_OS/$HOST_ARCH" >&2
        exit 1
        ;;
esac

if ! command -v curl >/dev/null 2>&1; then
    echo "error: curl not found on \$PATH — install curl first" >&2
    exit 1
fi

URL="$BASE_URL/oxide-$TRIPLE"
TMPFILE="$(mktemp -t oxide-install.XXXXXX)"
trap 'rm -f "$TMPFILE"' EXIT

echo "downloading $URL"
if ! curl -fsSL "$URL" -o "$TMPFILE"; then
    echo "error: download failed (URL: $URL)" >&2
    exit 1
fi

# Sanity-check the downloaded file: ELF (\x7fELF) for Linux, Mach-O magic
# for macOS. A 404 page from a CDN often returns HTML with 200 OK; the
# magic-byte check rejects those before we move it onto $PATH.
MAGIC_HEX="$(head -c 4 "$TMPFILE" | od -An -tx1 -v | tr -d ' \n')"
case "$MAGIC_HEX" in
    7f454c46)            # ELF
        ;;
    feedface|feedfacf)   # Mach-O 32 / 64 (little-endian)
        ;;
    cefaedfe|cffaedfe)   # Mach-O 32 / 64 (byte-swapped on disk)
        ;;
    cafebabe|bebafeca)   # Mach-O fat (universal)
        ;;
    *)
        echo "error: downloaded file is not a recognized binary (magic: $MAGIC_HEX)" >&2
        echo "       check that \$OXIDE_DIST_URL is reachable and serves a real binary" >&2
        exit 1
        ;;
esac

mkdir -p "$INSTALL_BIN"
install -m 0755 "$TMPFILE" "$INSTALL_BIN/oxide"

echo
echo "installed: $INSTALL_BIN/oxide"
echo

case ":${PATH:-}:" in
    *":$INSTALL_BIN:"*)
        echo "$INSTALL_BIN is already on \$PATH — try: oxide --help"
        ;;
    *)
        echo "add this line to your shell rc (~/.zshrc, ~/.bashrc, ...) to use it:"
        echo
        echo "    export PATH=\"$INSTALL_BIN:\$PATH\""
        ;;
esac
