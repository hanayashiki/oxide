fn main() -> i32 {
    let a: [i32; 3] = [10, 20, 30];
    let p: *const [i32; 3] = &a;
    (*p)[2]
}
