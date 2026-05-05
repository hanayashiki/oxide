// Nested generic in *type position* (parameter type) — same `JointGt Gt`
// shape as turbofish, exercises the `gt_close()` helper at line 107 in
// `type_parser`.
fn f(x: Vec<Vec<i32>>) {}
