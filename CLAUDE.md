# Oxide

A Rust-like syntax C-ish language for purpose of learning LLVM, written in Rust.

## Spec-driven development

Compiler has stable structure so our development follows:

1. Write spec in spec/*.md, e.g. `spec/PARSER.md`, `spec/IR.md`, `spec/LEXER.md`
2. Iterate spec with human
3. Implement the spec

## Ask question

1. Do not throw many questions all at once, ask your human one at a time. The answer to the first can affect the following, so ask one after another you.

## Bug Reporting

When reviewingg, report directly to your human and ask for confirmation; 

## Committing

A local `.git/hooks/pre-commit` is setup to guard the commit. Tests and types must be green to commit.

## Style

- Prefer `if let Pattern = expr` over `if matches!(expr, Pattern)`
