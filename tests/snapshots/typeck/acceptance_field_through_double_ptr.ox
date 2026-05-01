struct Point { x: i32, y: i32 }

fn get_x(s: *const *mut Point) -> i32 {
    s.x
}
