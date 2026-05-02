extern "C" {
    fn puts(s: *const [u8]) -> i32;
}

fn main() -> i32 {
    puts("hello world");
    puts("goodbye");
    0
}
