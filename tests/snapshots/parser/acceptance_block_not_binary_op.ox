// FIXME: if {} block should not consume `-1`.
fn main() -> i32 {
    if true {
        true
    }
    -1
}
