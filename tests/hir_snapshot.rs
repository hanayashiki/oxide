//! File-based snapshot tests for HIR lowering.
//!
//! Layout: every `tests/snapshots/hir/<name>.ox` is paired with a
//! `<name>.snap` containing what `render_hir` produces. Diffs go to
//! stderr; mismatches are aggregated and reported with a summary at the
//! end. To regenerate all `.snap` files:
//!
//! ```text
//! UPDATE_EXPECT=1 cargo test --test hir_snapshot
//! ```

mod common;

use std::path::Path;

#[test]
fn hir_snapshot() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/hir");
    common::assert_snapshots(&dir, common::render_hir);
}
