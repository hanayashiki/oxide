extern "C" { fn puts(s: *const [u8]) -> i32; }

const HELLO: *const [u8; 6] = "hello";

fn main() -> i32 {
    puts(HELLO);
    puts(HELLO);
    0
}
