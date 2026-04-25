#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

# 1. Oxide source -> LLVM IR
cargo run --quiet --example oxide-codegen-example -- -f hello.ox -o hello.ll

# 2. LLVM IR -> object file
cc -c hello.ll -o target/hello.o

# 3. Runtime C -> object file
cc -c runtime.c -o target/runtime.o

# 4. Link both
cc target/hello.o target/runtime.o -o target/hello

# 5. Run
echo "--- ./hello ---"
./target/hello
