struct Inner { v: i32 }
struct Outer { i: Inner }

fn main() -> i32 {
    let o = Outer { i: Inner { v: 99 } };
    o.i.v
}
