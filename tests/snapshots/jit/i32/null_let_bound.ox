fn accept(_p: *const u8) -> i32 { 0 }

fn main() -> i32 {
    let p: *const u8 = null;
    accept(p)
}
