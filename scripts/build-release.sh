#!/usr/bin/env bash
#
# Build release binaries for the supported targets and stage them under
# releases/ at the repo root.
#
#   - macOS aarch64  → native cargo build (only on Darwin/arm64 host)
#   - Linux x86_64   → docker build via docker/linux-x86_64.Dockerfile
#
# This script does NOT upload anywhere — see scripts/publish.sh for that.
# Run order: build-release.sh → publish.sh → cd docs && mdbook build →
# wrangler pages deploy docs/book.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$REPO_ROOT/releases"

mkdir -p "$DIST_DIR"

# ---------- macOS aarch64 (native) ----------
HOST_OS="$(uname -s)"
HOST_ARCH="$(uname -m)"
if [[ "$HOST_OS/$HOST_ARCH" == "Darwin/arm64" ]]; then
    if ! command -v cargo >/dev/null 2>&1; then
        echo "error: cargo not on \$PATH — install Rust via https://rustup.rs first" >&2
        exit 1
    fi
    echo "==> building macOS aarch64 (native cargo)"
    rustup target list --installed 2>/dev/null | grep -qx aarch64-apple-darwin \
        || rustup target add aarch64-apple-darwin
    (cd "$REPO_ROOT" && cargo build --release --target aarch64-apple-darwin)
    # gzip into dist (debug symbols preserved). gzip -9 -c lets us stage
    # straight at the final filename without a temporary copy.
    gzip -9 -c "$REPO_ROOT/target/aarch64-apple-darwin/release/oxide" \
        > "$DIST_DIR/oxide-aarch64-apple-darwin.gz"
    chmod 0644 "$DIST_DIR/oxide-aarch64-apple-darwin.gz"
    echo "    → $DIST_DIR/oxide-aarch64-apple-darwin.gz"
else
    echo "==> skipping macOS aarch64 build — not on Darwin/arm64 host (this is $HOST_OS/$HOST_ARCH)"
fi

# ---------- Linux x86_64 (docker) ----------
if command -v docker >/dev/null 2>&1; then
    echo "==> building Linux x86_64 (docker)"
    # `docker build --output` uses BuildKit's export stage to pull just
    # the binary out of the `export` stage and onto the host filesystem.
    # No named image is created.
    # --platform=linux/amd64 forces an x86_64 container even on Apple
    # Silicon hosts (Docker Desktop uses Rosetta 2 to emulate, which is
    # surprisingly fast — sub-minute clean builds). Keeping the platform
    # flag here rather than in the Dockerfile silences BuildKit's
    # FromPlatformFlagConstDisallowed warning.
    docker build \
        --platform linux/amd64 \
        --output "$DIST_DIR" \
        --file "$REPO_ROOT/docker/linux-x86_64.Dockerfile" \
        --target export \
        "$REPO_ROOT"
    mv "$DIST_DIR/oxide.gz" "$DIST_DIR/oxide-x86_64-unknown-linux-gnu.gz"
    chmod 0644 "$DIST_DIR/oxide-x86_64-unknown-linux-gnu.gz"
    echo "    → $DIST_DIR/oxide-x86_64-unknown-linux-gnu.gz"
else
    echo "==> skipping Linux x86_64 build — docker not on \$PATH"
fi

echo
echo "staged binaries:"
ls -la "$DIST_DIR" | grep -v '^total' | grep -v '^d' || true
echo
echo "next: ./scripts/publish.sh   (uploads to R2)"
