fn add(a: i32, b: i32) -> i32 { a + b }
fn apply(f: fn(i32, i32) -> i32, x: i32, y: i32) -> i32 { f(x, y) }
fn main() -> i32 { apply(add, 1, 2) }
