// Covariance on return: `fn() -> *mut T` is a subtype of `fn() -> *const T`
// (returning `*mut` lets the caller still treat it as `*const`).
// Spec/19_FN_PTR.md §3.1.
fn get_mut() -> *mut i32 {
    let mut x: i32 = 0;
    &mut x
}

fn main() -> i32 {
    let f: fn() -> *const i32 = get_mut;
    0
}
