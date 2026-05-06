fn use_ptr_to_fn(p: *mut fn(i32) -> bool) -> bool { false }
fn use_higher_order(f: fn(fn(i32) -> i32) -> i32) -> i32 { 0 }
