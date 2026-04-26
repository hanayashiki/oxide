//! File-based snapshot tests for the parser.
//!
//! Layout: every `tests/snapshots/parser/<name>.ox` is paired with a
//! `<name>.snap` containing what `render_parser` produces. Diffs go to
//! stderr; mismatches are aggregated and reported with a summary at the
//! end. To regenerate all `.snap` files:
//!
//! ```text
//! UPDATE_EXPECT=1 cargo test --test parser_snapshot
//! ```

mod common;

use std::path::Path;

#[test]
fn parser_snapshot() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/parser");
    common::assert_snapshots(&dir, common::render_parser);
}
