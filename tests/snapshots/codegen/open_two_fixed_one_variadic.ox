extern "C" { fn open(path: *const [u8], flags: i32, ...) -> i32; }

fn main() -> i32 {
    open("/tmp/f", 0x42, 0x1A4);
    0
}
