struct A { x: i32 }
struct B { x: i32 }

fn coerce(a: A) -> B {
    a
}
