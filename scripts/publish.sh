#!/usr/bin/env bash
#
# Upload staged release binaries from releases/ to the Cloudflare R2
# bucket via wrangler. Run after scripts/build-release.sh.
#
# Prereqs (one-time):
#   - npm/node on $PATH
#   - npx wrangler login
#   - npx wrangler r2 bucket create oxide-dist
#   - npx wrangler r2 bucket dev-url enable oxide-dist
#     (this prints the public r2.dev URL; copy it into install.sh's
#      OXIDE_DIST_URL default if not already there)
#
# Usage:   ./scripts/publish.sh [bucket-name]
#          BUCKET=oxide-dist ./scripts/publish.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$REPO_ROOT/releases"
BUCKET="${1:-${BUCKET:-oxide-dist}}"

if [[ ! -d "$DIST_DIR" ]]; then
    echo "error: $DIST_DIR does not exist — run ./scripts/build-release.sh first" >&2
    exit 1
fi

shopt -s nullglob
GZS=("$DIST_DIR"/oxide-*.gz)
if (( ${#GZS[@]} == 0 )); then
    echo "error: no oxide-*.gz files in $DIST_DIR — run ./scripts/build-release.sh first" >&2
    exit 1
fi

if ! command -v npx >/dev/null 2>&1; then
    echo "error: npx not on \$PATH — install Node.js" >&2
    exit 1
fi

echo "uploading ${#GZS[@]} file(s) to r2://$BUCKET ..."
for f in "${GZS[@]}"; do
    name="$(basename "$f")"
    echo "  -> $name"
    # --remote pushes to the actual R2 bucket (not the local emulation
    # that wrangler dev uses). --content-type=application/gzip so the
    # CDN serves it correctly; --content-encoding=identity so curl does
    # not auto-decompress (install.sh handles gunzip itself).
    npx --yes wrangler r2 object put \
        "$BUCKET/$name" \
        --file="$f" \
        --remote \
        --content-type="application/gzip" \
        --content-encoding="identity"
done

echo
echo "done. uploaded files:"
for f in "${GZS[@]}"; do
    echo "  $BUCKET/$(basename "$f")"
done
