// Spec/19_FN_PTR.md §6.iii.2: passing a fn name as a value emits
// `ptr @fn_name` at the arg position.
fn add(a: i32, b: i32) -> i32 { a + b }
fn use_op(f: fn(i32, i32) -> i32) -> i32 { f(10, 20) }

fn main() -> i32 {
    use_op(add)
}
