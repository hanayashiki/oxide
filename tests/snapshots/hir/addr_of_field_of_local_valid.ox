struct Point { x: i32, y: i32 }

fn f() {
    let mut p = Point { x: 1, y: 2 };
    &p.x;
    &mut p.y;
}
