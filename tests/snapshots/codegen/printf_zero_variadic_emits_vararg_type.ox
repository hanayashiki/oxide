extern "C" { fn printf(fmt: *const [u8], ...) -> i32; }

fn main() -> i32 {
    printf("hi\n");
    0
}
