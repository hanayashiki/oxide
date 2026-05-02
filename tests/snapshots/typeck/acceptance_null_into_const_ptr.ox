extern "C" {
    fn puts(s: *const u8) -> i32;
}

fn main() -> i32 {
    let s: *const u8 = null;
    puts(s);
    0
}
