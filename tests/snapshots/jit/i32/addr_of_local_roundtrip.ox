fn main() -> i32 {
    let mut x = 7;
    let p = &mut x;
    let _ignore = p;
    x
}
