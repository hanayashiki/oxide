// `fn(*mut i32) -> i32` cannot stand in for `fn(*const i32) -> i32`:
// the contravariant param rule needs `*const i32 <: *mut i32`, which
// is the rejected `Const → Mut` direction. Spec/19_FN_PTR.md §3.1.
fn writer(p: *mut i32) -> i32 { 0 }

fn main() -> i32 {
    let f: fn(*const i32) -> i32 = writer;
    0
}
