struct Point { x: i32, y: i32 }

extern "C" {
    fn use_int(p: *mut i32);
}

fn f() {
    let mut p = Point { x: 1, y: 2 };
    use_int(&mut p.x);
}
