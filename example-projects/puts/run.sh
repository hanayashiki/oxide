#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

# 1. Oxide source -> LLVM IR
cargo run --quiet --bin oxide -- hello.ox --emit ir -o hello.ll

# 2. LLVM IR -> executable. `puts` lives in libc, which `cc` links by
#    default — no extra flags needed.
cc hello.ll -o target/hello

# 3. Run
echo "--- ./hello ---"
./target/hello
