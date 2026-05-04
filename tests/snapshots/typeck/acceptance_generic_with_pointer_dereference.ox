fn get<T>(p: *mut T) -> T {
    *p
}

fn main() -> i32 {
    let mut x = 42;
    let p: *mut i32 = &mut x;
    get(p)
}
