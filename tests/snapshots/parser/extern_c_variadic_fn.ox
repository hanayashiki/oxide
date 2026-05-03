extern "C" {
    fn printf(fmt: *const u8, ...) -> i32;
    fn open(path: *const u8, flags: i32, ...) -> i32;
}
