set -e

LLC=/opt/homebrew/opt/llvm@18/bin/llc

$LLC -O0 -o hello.s hello.ll
cc -O0 -o hello hello.s

./hello
