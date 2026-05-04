fn pair<T>(a: T, b: T) -> T { a }
fn outer<U>(x: U, y: U) -> U { pair(x, y) }
fn main() -> i32 { outer::<i32>(1, 2) }
