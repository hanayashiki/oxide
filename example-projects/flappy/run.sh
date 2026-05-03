#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

mkdir -p target

# 1. Oxide source -> LLVM IR
cargo run --quiet --manifest-path ../../Cargo.toml --bin oxide -- flappy.ox
