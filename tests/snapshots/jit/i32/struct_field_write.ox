struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let mut p = Point { x: 3, y: 4 };
    p.x = 4
    p.y = 5
    p.y
}
