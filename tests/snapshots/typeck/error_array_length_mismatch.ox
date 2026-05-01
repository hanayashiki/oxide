fn want3(a: [i32; 3]) -> i32 { a[0] }

fn caller() -> i32 {
    let a: [i32; 4] = [1, 2, 3, 4];
    want3(a)
}
