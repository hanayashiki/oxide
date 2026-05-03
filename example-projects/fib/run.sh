#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

# 1. Oxide source -> LLVM IR
cargo run --quiet --bin oxide -- fib.ox --emit ir -o fib.ll

# 2. LLVM IR -> object file
cc -c fib.ll -o target/fib.o

# 3. Runtime C -> object file
cc -c runtime.c -o target/runtime.o

# 4. Link both
cc target/fib.o target/runtime.o -o target/fib

# 5. Run
echo "--- ./fib ---"
./target/fib
