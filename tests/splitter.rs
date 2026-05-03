//! Unit tests for the multi-file fixture splitter (`tests/common/multi_file.rs`).
//!
//! Lives in its own integration-test binary so the tests run once,
//! not once per test crate that does `mod common;`.

mod common;

use std::path::{Path, PathBuf};

use common::multi_file::{SplitError, split_fixture};

#[test]
fn no_headers_passthrough() {
    let r = split_fixture("fn main() {}\n", Path::new("solo.ox")).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, PathBuf::from("solo.ox"));
    assert_eq!(r[0].1, "fn main() {}\n");
}

#[test]
fn single_header_one_file() {
    let src = "/// main.ox\nfn main() {}\n";
    let r = split_fixture(src, Path::new("ignored.ox")).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, PathBuf::from("main.ox"));
    assert_eq!(r[0].1, "fn main() {}\n");
}

#[test]
fn multiple_headers_split_in_source_order() {
    let src = "\
/// main.ox
import \"./util.ox\";
fn main() -> i32 { 0 }
/// util.ox
fn add_one(x: i32) -> i32 { x + 1 }
";
    let r = split_fixture(src, Path::new("ignored.ox")).unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].0, PathBuf::from("main.ox"));
    assert!(r[0].1.contains("import \"./util.ox\""));
    assert!(r[0].1.contains("fn main() -> i32 { 0 }"));
    assert_eq!(r[1].0, PathBuf::from("util.ox"));
    assert!(r[1].1.contains("fn add_one"));
}

#[test]
fn diamond_four_files() {
    let src = "\
/// main.ox
import \"./a.ox\";
import \"./b.ox\";
/// a.ox
import \"./util.ox\";
/// b.ox
import \"./util.ox\";
/// util.ox
fn shared() {}
";
    let r = split_fixture(src, Path::new("x.ox")).unwrap();
    assert_eq!(
        r.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
        vec![
            PathBuf::from("main.ox"),
            PathBuf::from("a.ox"),
            PathBuf::from("b.ox"),
            PathBuf::from("util.ox"),
        ]
    );
}

#[test]
fn leading_blank_lines_are_ok() {
    let src = "\n\n/// main.ox\nfn main() {}\n";
    let r = split_fixture(src, Path::new("ignored.ox")).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].1, "fn main() {}\n");
}

#[test]
fn leading_content_errors() {
    let src = "fn stray() {}\n/// main.ox\nfn main() {}\n";
    let err = split_fixture(src, Path::new("x.ox")).unwrap_err();
    let SplitError::LeadingContent { line } = err else {
        panic!("expected LeadingContent, got {err:?}");
    };
    assert_eq!(line, 1);
}

#[test]
fn duplicate_path_errors() {
    let src = "/// main.ox\nfn a() {}\n/// main.ox\nfn b() {}\n";
    let err = split_fixture(src, Path::new("x.ox")).unwrap_err();
    let SplitError::DuplicatePath {
        path,
        first_line,
        dup_line,
    } = err
    else {
        panic!("expected DuplicatePath, got {err:?}");
    };
    assert_eq!(path, PathBuf::from("main.ox"));
    assert_eq!(first_line, 1);
    assert_eq!(dup_line, 3);
}

#[test]
fn quadruple_slash_is_not_a_header() {
    let src = "//// not a header\nfn main() {}\n";
    let r = split_fixture(src, Path::new("solo.ox")).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, PathBuf::from("solo.ox"));
}

#[test]
fn triple_slash_without_path_is_doc_comment() {
    let src = "///\n///   \nfn main() {}\n";
    let r = split_fixture(src, Path::new("solo.ox")).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, PathBuf::from("solo.ox"));
}
