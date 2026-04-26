struct Point { x: i32, y: i32 }

fn f() {
    Point { x: 0, y: 0 } = Point { x: 1, y: 2 };
}
