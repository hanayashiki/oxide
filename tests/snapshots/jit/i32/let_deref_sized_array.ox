fn main() -> i32 {
    let a: [i32; 3] = [1, 2, 3];
    let p: *const [i32; 3] = &a;
    let b: [i32; 3] = *p;
    b[0] + b[1] + b[2]
}
