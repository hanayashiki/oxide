struct Pair { a: i32, b: i32 }

fn main() -> i32 {
    let p = Pair { a: 10, b: 20 };
    let q = p;
    q.b
}
