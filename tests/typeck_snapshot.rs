//! File-based snapshot tests for the type checker.
//!
//! Layout: every `tests/snapshots/typeck/<name>.ox` is paired with a
//! `<name>.snap` containing what `render_typeck` produces. To regenerate
//! all `.snap` files (e.g. after intentionally changing diagnostic
//! wording or the pretty-print format), run:
//!
//! ```text
//! UPDATE_EXPECT=1 cargo test --test typeck_snapshot
//! ```

mod common;

use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn typeck_snapshot() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/typeck");
    let update = std::env::var_os("UPDATE_EXPECT").is_some();

    let mut ox_files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("typeck snapshot dir missing")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ox"))
        .collect();
    ox_files.sort();

    assert!(!ox_files.is_empty(), "no .ox files under {}", dir.display());

    let mut failures: Vec<String> = Vec::new();

    for ox_path in &ox_files {
        let stem = ox_path.file_stem().unwrap().to_string_lossy().into_owned();
        let file_name = format!("{stem}.ox");
        let src = fs::read_to_string(ox_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", ox_path.display()));

        let actual = common::render_typeck(&file_name, &src);
        let snap_path = ox_path.with_extension("snap");

        if update {
            fs::write(&snap_path, &actual)
                .unwrap_or_else(|e| panic!("write {}: {e}", snap_path.display()));
            continue;
        }

        let expected = fs::read_to_string(&snap_path).unwrap_or_default();
        if actual != expected {
            failures.push(format_mismatch(&snap_path, &expected, &actual));
        }
    }

    if !failures.is_empty() {
        let msg = format!(
            "{} snapshot mismatch(es). Run with UPDATE_EXPECT=1 to bless.\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
        panic!("{msg}");
    }
}

fn format_mismatch(path: &Path, expected: &str, actual: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("--- {} ---\n", path.display()));
    let exp_lines: Vec<&str> = expected.lines().collect();
    let act_lines: Vec<&str> = actual.lines().collect();
    let n = exp_lines.len().max(act_lines.len());
    for i in 0..n {
        let e = exp_lines.get(i).copied().unwrap_or("");
        let a = act_lines.get(i).copied().unwrap_or("");
        if e == a {
            out.push_str(&format!("  {e}\n"));
        } else {
            if i < exp_lines.len() {
                out.push_str(&format!("- {e}\n"));
            }
            if i < act_lines.len() {
                out.push_str(&format!("+ {a}\n"));
            }
        }
    }
    out
}
