fn first(a: [i32; 3]) -> i32 { a[0] }

fn at(p: *const [i32], i: usize) -> i32 { p[i] }

fn main() -> i32 {
    let a: [i32; 3] = [1, 2, 3];
    let b: [u8; 1024] = [0; 1024];
    first(a) + (b[0] as i32)
}
