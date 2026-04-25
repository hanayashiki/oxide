# Oxide

A Rust-like syntax C-ish language for purpose of learning LLVM, written in Rust.

## Spec-driven development

Compiler has stable structure so our development follows:

1. Write spec in spec/*.md, e.g. `spec/PARSER.md`, `spec/IR.md`, `spec/LEXER.md`
2. Iterate spec with human
3. Implement the spec
