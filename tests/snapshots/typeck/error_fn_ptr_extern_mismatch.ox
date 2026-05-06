// is_extern_c is invariant per spec/19_FN_PTR.md §3.2 — non-extern
// `fn` cannot stand in for `extern "C" fn`.
fn local(x: i32) -> i32 { x }
fn main() -> i32 {
    let f: extern "C" fn(i32) -> i32 = local;
    0
}
