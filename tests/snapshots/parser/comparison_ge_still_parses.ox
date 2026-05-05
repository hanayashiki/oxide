// `>=` is now `JointGt Eq`. Level 5's `Ge` branch matches it; the plain
// `Gt`/`JointGt` comparison branches don't fire because they sit after
// the `Ge` branch in the choice and choice tries left-to-right.
fn main() { let b: bool = 1 >= 2; }
