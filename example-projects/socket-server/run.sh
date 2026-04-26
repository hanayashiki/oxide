#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

mkdir -p target

# 1. Oxide source -> LLVM IR
cargo run --quiet --example oxide-codegen-example -- -f server.ox -o target/server.ll

# 2. LLVM IR -> executable. socket/bind/accept etc. live in libSystem
#    on macOS; `cc` links it by default.
cc target/server.ll -o target/server

# 3. Run. Test with `curl -i http://localhost:8080/` from another terminal.
echo "--- ./server (Ctrl+C to stop) ---"
./target/server
