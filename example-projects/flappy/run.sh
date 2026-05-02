#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

mkdir -p target

# 1. Oxide source -> LLVM IR
cargo run --quiet --manifest-path ../../Cargo.toml --example oxide-codegen-example -- -f flappy.ox -o target/flappy.ll

# 2. LLVM IR -> executable. libc symbols are linked by default.
cc target/flappy.ll -o target/flappy

# 3. Run. Restore the terminal on exit even if the user kills the
#    process via Ctrl-C — `stty` was put in a non-canonical mode and
#    leaving it that way is rude to the surrounding shell.
trap 'stty icanon echo' EXIT INT TERM

echo "--- ./flappy (SPACE=flap, q=quit, r=restart) ---"
./target/flappy
