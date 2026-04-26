struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let mut p = Point { x: 11, y: 22 };
    let _q = &mut p.x;
    p.x
}
