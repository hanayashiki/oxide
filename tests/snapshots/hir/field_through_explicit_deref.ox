struct Point { x: i32, y: i32 }
fn f(p: *mut Point) { (*p).x = 7; }
