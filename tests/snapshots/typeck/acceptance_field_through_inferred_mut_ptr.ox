struct Point { x: i32, y: i32 }

fn f() {
    let mut p = Point { x: 1, y: 2 };
    let ptr = &mut p;
    let _ = ptr.x;
    ptr.x = 5;
}
