fn use_cmp(cmp: extern "C" fn(*const u8, *const u8) -> i32) -> i32 { 0 }
fn use_printf(p: extern "C" fn(*const u8, ...) -> i32) -> i32 { 0 }
fn use_extern_thunk(t: extern "C" fn()) {}
