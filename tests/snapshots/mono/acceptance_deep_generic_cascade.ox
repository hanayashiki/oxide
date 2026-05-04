fn a<T>(x: T) -> T { b(x) }
fn b<T>(x: T) -> T { c(x) }
fn c<T>(x: T) -> T { d(x) }
fn d<T>(x: T) -> T { e(x) }
fn e<T>(x: T) -> T { x }

fn main() -> i32 {
    a(42)
}
