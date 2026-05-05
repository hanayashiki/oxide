// `>>=` lexes as `JointGt JointGt Eq` (three single-`>` tokens, each with
// its joint flag derived from the next char). The two type closes eat one
// `JointGt` each, leaving `Eq 0` for the assignment. Rust accepts this
// without a space; we should too.
fn main() { let s: Vec<Vec<i32>>=0; }
