fn at(p: *const [i32; 3], i: usize) -> i32 { p[i] }

fn main() -> i32 {
    let a: [i32; 3] = [11, 22, 33];
    at(&a, 2)
}
