// `>>=` in expression position is `JointGt JointGt Eq`. Pratt level 9
// peeks `JointGt then Gt`, second token is `JointGt` not `Gt`, choice
// rewinds, level 1's `JointGt JointGt Eq` matches as `AssignOp::Shr`.
fn main() { let mut x: i32 = 4; x >>= 1; }
