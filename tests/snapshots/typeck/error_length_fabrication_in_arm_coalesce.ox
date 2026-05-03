fn unsized_ptr() -> *const [u8] { "hi" }
fn main() -> i32 {
    let s = if true { unsized_ptr() } else { "hi" };
    0
}
