fn one(x: i32) -> i32 { x }
fn main() -> i32 {
    let f: fn(i32, i32) -> i32 = one;
    0
}
