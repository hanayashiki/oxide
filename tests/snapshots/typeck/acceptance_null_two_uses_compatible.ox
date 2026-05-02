extern "C" {
    fn puts(s: *const u8) -> i32;
    fn read_const(s: *const u8) -> i32;
}

fn main() -> i32 {
    let p = null;
    puts(p);
    read_const(p);
    0
}
