//! Snapshot tests for the mono pass. Each `tests/snapshots/mono/<name>.ox`
//! is rendered through `common::render_mono` and compared against
//! `<name>.snap`. Auto-bless on first run via `UPDATE_EXPECT=1`.

mod common;

use std::path::Path;

#[test]
fn mono_snapshot() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/mono");
    common::assert_snapshots(&dir, common::render_mono);
}
