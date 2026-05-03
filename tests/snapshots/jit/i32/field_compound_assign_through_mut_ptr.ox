struct P { x: i32, y: i32 }

fn main() -> i32 {
    let mut p = P { x: 7, y: 0 };
    let q: *mut P = &mut p;
    q.x = q.x + 3;
    p.x
}
