// fn-pointer types as struct fields. The struct lowers to a 2-pointer
// aggregate; field-access-then-call is the standard Field + Call shape
// (no special parens form needed). Returns dbl(one()) = 2.
struct Callbacks {
    on_init: fn() -> i32,
    on_done: fn(i32) -> i32,
}

fn one() -> i32 { 1 }
fn dbl(x: i32) -> i32 { x + x }

fn main() -> i32 {
    let c = Callbacks { on_init: one, on_done: dbl };
    let a = c.on_init();
    c.on_done(a)
}
