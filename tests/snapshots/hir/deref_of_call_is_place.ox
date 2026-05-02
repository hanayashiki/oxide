extern "C" { fn make_ptr() -> *mut i32; }
fn f() { *make_ptr() = 1; }
