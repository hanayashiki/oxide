// Contravariance on params: a fn that accepts `*const i32` is a
// subtype of a fn that accepts `*mut i32` (the more general
// signature wins on the param side). Spec/19_FN_PTR.md §3.1.
fn read_only(p: *const i32) -> i32 { 0 }

fn main() -> i32 {
    let f: fn(*mut i32) -> i32 = read_only;
    0
}
