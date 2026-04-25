#!/usr/bin/env bash
#
# compile.sh — write each C source's LLVM IR (or asm/bc) to a file.
#
# Usage:
#   ./compile.sh [file.c ...] [-O0|-O1|-O2|-O3|-Os|-Oz] [--asm] [--bc]
#
# With no .c arguments, processes every *.c in the current directory.
# Output is whatever clang produces, untouched.

set -euo pipefail

opt="-O0"
mode="ir"   # ir | asm | bc
files=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        -O0|-O1|-O2|-O3|-Os|-Oz) opt="$1" ;;
        --asm) mode="asm" ;;
        --bc)  mode="bc" ;;
        -h|--help) sed -n '1,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *.c) files+=("$1") ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
    shift
done

if [[ ${#files[@]} -eq 0 ]]; then
    shopt -s nullglob
    files=(*.c)
    [[ ${#files[@]} -gt 0 ]] || { echo "no .c files in $(pwd)" >&2; exit 1; }
fi

common=( -fno-discard-value-names -Xclang -disable-O0-optnone "$opt" )

for src in "${files[@]}"; do
    case "$mode" in
        ir)  out="${src%.c}.ll"; clang -S -emit-llvm "${common[@]}" "$src" -o "$out" ;;
        asm) out="${src%.c}.s";  clang -S            "${common[@]}" "$src" -o "$out" ;;
        bc)  out="${src%.c}.bc"; clang -c -emit-llvm "${common[@]}" "$src" -o "$out" ;;
    esac
    echo "wrote $out" >&2
done
