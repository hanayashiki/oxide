struct P { x: i32, y: i32 }

fn main() -> i32 {
    let mut p = P { x: 0, y: 0 };
    let q: *mut P = &mut p;
    let xp: *mut i32 = &mut q.x;
    *xp = 42;
    p.x
}
