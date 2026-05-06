fn use_named(p: fn(ok: i32, bad: i32) -> bool) -> bool { p(0, 1) }
fn use_unnamed(p: fn(i32, i32) -> bool) -> bool { p(0, 1) }
