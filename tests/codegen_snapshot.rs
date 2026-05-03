//! File-based snapshot tests for codegen IR.
//!
//! Layout: every `tests/snapshots/codegen/<name>.ox` is paired with a
//! `<name>.snap` containing the LLVM IR text produced by
//! `render_codegen`. Diffs go to stderr; mismatches are aggregated and
//! reported with a summary at the end. To regenerate all `.snap` files
//! (e.g. after intentionally changing IR shape):
//!
//! ```text
//! UPDATE_EXPECT=1 cargo test --test codegen_snapshot
//! ```

mod common;

use std::path::Path;

#[test]
fn codegen_snapshot() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/codegen");
    common::assert_snapshots(&dir, common::render_codegen);
}
