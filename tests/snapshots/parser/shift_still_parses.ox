// Plain shift `>>` — the second `>` is followed by whitespace so it's
// `Gt`, giving `JointGt Gt`. Pratt level 9 matches the pair. Regression
// guard: shift still works after the per-character `>` lex change.
fn main() { let x: i32 = 1 >> 2; }
