extern "C" { fn printf(fmt: *const [u8], ...) -> i32; }

fn main() -> i32 {
    let c: u8 = 65;
    printf("c = %d\n", c);
    0
}
