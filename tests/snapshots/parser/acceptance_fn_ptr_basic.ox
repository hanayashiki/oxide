fn use_pred(p: fn(i32) -> bool) -> bool { p(0) }
fn use_thunk(t: fn() -> i32) -> i32 { t() }
fn use_void(t: fn()) { t(); }
fn use_named_param(p: fn(ok: i32) -> bool) -> bool { p(0) }
