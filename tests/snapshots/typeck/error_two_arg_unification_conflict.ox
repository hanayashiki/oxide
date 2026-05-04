fn dup<T>(a: T, b: T) -> T { a }
fn main() {
    let x: i32 = 1;
    let y: i64 = 2;
    dup(x, y);
}
