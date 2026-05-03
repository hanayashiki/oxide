extern "C" {
    fn printf(fmt: *const [u8], ...) -> i32;
}
struct Point { x: i32, y: i32 }
fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    printf("%d %d\n", p);
    0
}
