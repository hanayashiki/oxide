extern "C" { fn printf(fmt: *const [u8], ...) -> i32; }

fn main() -> i32 {
    let n: i8 = -5;
    printf("n = %d\n", n);
    0
}
