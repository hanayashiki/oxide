fn square(x: i32) -> i32 { x * x }
fn apply(f: fn(i32) -> i32, x: i32) -> i32 { f(x) }
fn main() -> i32 { apply(square, 3) }
