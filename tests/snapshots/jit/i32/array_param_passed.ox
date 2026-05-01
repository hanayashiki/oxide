fn first(a: [i32; 3]) -> i32 { a[0] }

fn main() -> i32 {
    let a: [i32; 3] = [1, 2, 3];
    let b: [u8; 8] = [0; 8];
    first(a) + (b[0] as i32)
}
