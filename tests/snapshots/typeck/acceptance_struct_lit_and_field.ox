struct Point { x: i32, y: i32 }

fn make() -> Point {
    Point { x: 1, y: 2 }
}

fn x_of_make() -> i32 {
    let p = make();
    p.x
}

fn ptr(p: *mut Point) {
    // deref not supported yet
}