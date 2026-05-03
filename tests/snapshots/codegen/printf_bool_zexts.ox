extern "C" { fn printf(fmt: *const [u8], ...) -> i32; }

fn main() -> i32 {
    let b: bool = true;
    printf("b = %d\n", b);
    0
}
