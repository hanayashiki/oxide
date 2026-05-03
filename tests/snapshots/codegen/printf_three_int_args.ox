extern "C" { fn printf(fmt: *const [u8], ...) -> i32; }

fn main() -> i32 {
    printf("a=%d b=%d c=%d\n", 1, 2, 3);
    0
}
