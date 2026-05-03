fn unsized_ptr() -> *const [u8] { "hi" }
fn main() -> i32 {
    let s = if true { "hi" } else { unsized_ptr() };
    0
}
