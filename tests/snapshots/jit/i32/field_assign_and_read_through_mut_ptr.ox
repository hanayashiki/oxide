struct P { x: i32, y: i32 }

fn main() -> i32 {
    let mut p = P { x: 0, y: 0 };
    let q: *mut P = &mut p;
    q.x = 10;
    q.y = 20;
    q.x + q.y
}
