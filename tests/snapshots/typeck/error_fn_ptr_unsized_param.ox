// Spec/19_FN_PTR.md §4: a fn pointer's params and return must
// themselves be sized. `[u8]` (DST) at param position is rejected
// via the recursive `discharge_sized` walk.
fn use_bad(p: fn([u8]) -> i32) -> i32 { 0 }
