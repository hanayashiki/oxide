struct Pair { a: i32, b: i32 }

fn main() -> i32 {
    let p = Pair { a: 4, b: 38 };
    let pp: *const Pair = &p;
    let s: Pair = *pp;
    s.a + s.b
}
