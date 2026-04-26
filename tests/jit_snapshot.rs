//! File-based JIT-execution tests.
//!
//! Each `tests/snapshots/jit/<return-type>/<name>.ox` is JIT-compiled and
//! its `main` function is executed; the returned primitive is rendered as
//! a single decimal line and compared against `<name>.snap`. Programs are
//! grouped by return type because `jit_run::<R>` is statically typed —
//! the runner for `i32` lives here, the runner for `i64`/`bool`/etc.
//! would each be its own dedicated test.
//!
//! ```text
//! UPDATE_EXPECT=1 cargo test --test jit_snapshot
//! ```

mod common;

use std::path::Path;

#[test]
fn jit_i32_snapshot() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/jit/i32");
    common::assert_snapshots(&dir, |_file_name, src| {
        let (ir, r): (String, i32) = unsafe { common::jit_run_with_ir(src, "main") };
        // `ir` already ends with a trailing newline, so no separator
        // needed before `== result ==`.
        format!("== ir ==\n{ir}== result ==\n{r}\n")
    });
}
