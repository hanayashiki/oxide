//! File-based snapshot tests for the type checker.
//!
//! Layout: every `tests/snapshots/typeck/<name>.ox` is paired with a
//! `<name>.snap` containing what `render_typeck` produces. Diffs go to
//! stderr; mismatches are aggregated and reported with a summary at the
//! end. To regenerate all `.snap` files (e.g. after intentionally
//! changing diagnostic wording or the pretty-print format):
//!
//! ```text
//! UPDATE_EXPECT=1 cargo test --test typeck_snapshot
//! ```

mod common;

use std::path::Path;

#[test]
fn typeck_snapshot() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/typeck");
    common::assert_snapshots(&dir, common::render_typeck);
}
