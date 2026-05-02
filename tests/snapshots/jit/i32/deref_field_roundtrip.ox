struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let mut p = Point { x: 0, y: 0 };
    let q: *mut Point = &mut p;
    (*q).x = 7;
    (*q).x
}
