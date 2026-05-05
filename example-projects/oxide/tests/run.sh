#!/usr/bin/env bash
# Snapshot harness for M1 utilities.
#
# Each test is an Oxide program in this directory (`m1_*.ox`) that
# prints a stable trace to stdout. We run the program through the
# stage-0 oxide compiler and compare to the matching `snapshots/*.snap`
# file. Missing snapshots are auto-blessed (Jest-style); on a diff we
# print a unified diff and exit 1.
#
# Usage:
#   ./tests/run.sh            # run all m1_*.ox tests
#   ./tests/run.sh m1_vec     # run a single test by stem
#   BLESS=1 ./tests/run.sh    # rewrite all snapshots from current output

set -uo pipefail

THIS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$THIS_DIR/../../.." && pwd)"
OXIDE="${OXIDE_BIN:-$REPO_ROOT/target/debug/oxide}"
SNAP_DIR="$THIS_DIR/snapshots"
mkdir -p "$SNAP_DIR"

# Self-host tests use paths relative to the repo root
# (`example-projects/oxide/util/vec.ox` etc.) so we cd there before
# running each test. Without this, running the harness from any other
# directory makes those paths unresolvable.
cd "$REPO_ROOT"

if [ ! -x "$OXIDE" ]; then
    echo "run.sh: stage-0 oxide binary not found at $OXIDE" >&2
    echo "        run \`cargo build --bin oxide\` first" >&2
    exit 2
fi

if [ $# -gt 0 ]; then
    FILTER="$1"
    TESTS=("$THIS_DIR/$FILTER.ox")
else
    TESTS=("$THIS_DIR"/m[0-9]*_*.ox)
fi

pass=0
fail=0
blessed=0

for src in "${TESTS[@]}"; do
    if [ ! -f "$src" ]; then
        echo "run.sh: not found: $src" >&2
        fail=$((fail + 1))
        continue
    fi
    stem="$(basename "$src" .ox)"
    snap="$SNAP_DIR/$stem.snap"
    actual_out="$(mktemp)"
    actual_err="$(mktemp)"

    if ! "$OXIDE" "$src" >"$actual_out" 2>"$actual_err"; then
        echo "FAIL  $stem  (compile/run failed)"
        echo "--- stdout ---"; cat "$actual_out"
        echo "--- stderr ---"; cat "$actual_err"
        fail=$((fail + 1))
        rm -f "$actual_out" "$actual_err"
        continue
    fi

    if [ "${BLESS:-0}" = "1" ] || [ ! -f "$snap" ]; then
        cp "$actual_out" "$snap"
        echo "BLESS $stem"
        blessed=$((blessed + 1))
        rm -f "$actual_out" "$actual_err"
        continue
    fi

    if diff -u "$snap" "$actual_out" >/dev/null 2>&1; then
        echo "PASS  $stem"
        pass=$((pass + 1))
        rm -f "$actual_out" "$actual_err"
    else
        echo "FAIL  $stem"
        diff -u "$snap" "$actual_out" || true
        if [ -s "$actual_err" ]; then
            echo "--- stderr ---"; cat "$actual_err"
        fi
        fail=$((fail + 1))
        rm -f "$actual_out" "$actual_err"
    fi
done

echo
echo "summary: $pass passed, $fail failed, $blessed blessed"
[ "$fail" -eq 0 ]
