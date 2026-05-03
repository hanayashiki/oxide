struct P { x: i32, y: i32 }

fn main() -> i32 {
    let mut p = P { x: 7, y: 0 };
    let mut q: *mut P = &mut p;
    let qq: *mut *mut P = &mut q;
    qq.x
}
