struct Point { x: i32, y: i32 }

fn write(s: *const Point) {
    s.x = 1;
}
