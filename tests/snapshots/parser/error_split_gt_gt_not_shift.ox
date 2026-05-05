// `> >` (whitespace between) is `Gt Gt` — neither is `JointGt`, so level
// 9 shift's `JointGt Gt` peek fails. Level 5 then matches `Gt` as a
// comparison; its RHS expects an expression but sees another `Gt`, which
// can't start one. Parse error — matches Rust, which also rejects this.
fn main() { let _ = 1 > > 2; }
