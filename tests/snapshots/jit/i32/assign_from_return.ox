// Divergent rhs in `=` (`x = loop {};` — `return` would be the more
// natural reproducer but Oxide's parser doesn't accept `return` in
// arbitrary expression positions yet). Pre-fix: emit_assign's
// `.expect("assign rhs produced no value")` panicked because the
// loop diverges and emit_loop returned None. After fix: emit_assign
// propagates the divergence and short-circuits before lvalue/store.
fn main() -> i32 {
    let mut x = 7;
    if false {
        x = loop {};       // diverges; the assign+store is unreachable
    }
    x                       // 7
}
