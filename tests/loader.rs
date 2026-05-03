//! Integration tests for `oxide::loader::load_program`.
//!
//! Uses the in-memory `VfsHost` for the graph-shape cases (linear,
//! diamond, cycle) and a real disk path for the "root unreadable"
//! case. All paths in the VFS world are absolute (rooted at `/`) so
//! tests don't depend on the cwd.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use oxide::loader::{LoadError, LoadedFile, VfsHost, load_program};
use oxide::reporter::SourceMap;

fn vfs(entries: &[(&str, &str)]) -> VfsHost {
    VfsHost::new(
        entries
            .iter()
            .map(|(k, v)| (PathBuf::from(k), v.to_string()))
            .collect::<HashMap<_, _>>(),
    )
}

fn find_by_path<'a>(files: &'a [LoadedFile], path: &str) -> &'a LoadedFile {
    files
        .iter()
        .find(|f| f.path == Path::new(path))
        .unwrap_or_else(|| panic!("no LoadedFile at {path}"))
}

#[test]
fn linear_chain_main_imports_util() {
    let host = vfs(&[
        ("/main.ox", "import \"./util.ox\";\n"),
        ("/util.ox", "fn util() {}\n"),
    ]);
    let mut map = SourceMap::new();
    let (files, root, errors) = load_program(&host, &mut map, PathBuf::from("/main.ox"));

    assert!(errors.is_empty(), "unexpected load errors: {errors:#?}");
    assert_eq!(files.len(), 2);
    assert_eq!(files[root].path, PathBuf::from("/main.ox"));

    let main = find_by_path(files.as_raw_slice(), "/main.ox");
    let util = find_by_path(files.as_raw_slice(), "/util.ox");
    assert_eq!(main.direct_imports, vec![util.file]);
    assert_eq!(util.direct_imports, vec![]);
}

#[test]
fn diamond_imports_each_loaded_once() {
    // a imports b and c; b imports d; c imports d. d should be loaded
    // exactly once with both b and c referencing the same FileId.
    let host = vfs(&[
        (
            "/a.ox",
            "import \"./b.ox\";\nimport \"./c.ox\";\nfn a() {}\n",
        ),
        ("/b.ox", "import \"./d.ox\";\nfn b() {}\n"),
        ("/c.ox", "import \"./d.ox\";\nfn c() {}\n"),
        ("/d.ox", "fn d() {}\n"),
    ]);
    let mut map = SourceMap::new();
    let (files, root, errors) = load_program(&host, &mut map, PathBuf::from("/a.ox"));

    assert!(errors.is_empty(), "unexpected load errors: {errors:#?}");
    assert_eq!(files.len(), 4, "expected 4 unique files (no duplication)");
    assert_eq!(files[root].path, PathBuf::from("/a.ox"));

    let a = find_by_path(files.as_raw_slice(), "/a.ox");
    let b = find_by_path(files.as_raw_slice(), "/b.ox");
    let c = find_by_path(files.as_raw_slice(), "/c.ox");
    let d = find_by_path(files.as_raw_slice(), "/d.ox");

    assert_eq!(a.direct_imports, vec![b.file, c.file]);
    assert_eq!(b.direct_imports, vec![d.file]);
    assert_eq!(c.direct_imports, vec![d.file]);
    assert_eq!(d.direct_imports, vec![]);
}

#[test]
fn cycle_loads_both_files_and_terminates() {
    // a imports b, b imports a. Both files load; both direct_imports
    // reference each other; the algorithm terminates.
    let host = vfs(&[
        ("/a.ox", "import \"./b.ox\";\nfn a() {}\n"),
        ("/b.ox", "import \"./a.ox\";\nfn b() {}\n"),
    ]);
    let mut map = SourceMap::new();
    let (files, root, errors) = load_program(&host, &mut map, PathBuf::from("/a.ox"));

    assert!(errors.is_empty(), "unexpected load errors: {errors:#?}");
    assert_eq!(files.len(), 2);
    assert_eq!(files[root].path, PathBuf::from("/a.ox"));

    let a = find_by_path(files.as_raw_slice(), "/a.ox");
    let b = find_by_path(files.as_raw_slice(), "/b.ox");
    assert_eq!(a.direct_imports, vec![b.file]);
    assert_eq!(b.direct_imports, vec![a.file]);
}

#[test]
fn missing_import_emits_import_file_not_found() {
    let host = vfs(&[(
        "/main.ox",
        "import \"./does-not-exist.ox\";\nfn main() {}\n",
    )]);
    let mut map = SourceMap::new();
    let (files, _root, errors) = load_program(&host, &mut map, PathBuf::from("/main.ox"));

    assert_eq!(files.len(), 1, "main itself loaded");
    assert_eq!(errors.len(), 1, "exactly one load error");

    let LoadError::ImportFileNotFound { raw, .. } = &errors[0] else {
        panic!(
            "expected ImportFileNotFound, got {:?}",
            errors[0]
        );
    };
    assert_eq!(raw, "./does-not-exist.ox");
}

#[test]
fn root_unreadable_emits_io_error() {
    // Use a disk path that definitely doesn't exist; empty mount table
    // so resolve falls through to disk and `read` returns ENOENT.
    let host = VfsHost::new(HashMap::new());
    let mut map = SourceMap::new();
    let bogus = PathBuf::from("/oxide-test-this-path-does-not-exist-xyzzy.ox");
    let (files, _root, errors) = load_program(&host, &mut map, bogus.clone());

    assert!(files.is_empty(), "expected no files, got {}", files.len());
    assert_eq!(errors.len(), 1);
    let LoadError::Io { path, .. } = &errors[0] else {
        panic!("expected LoadError::Io, got {:?}", errors[0]);
    };
    assert_eq!(path, &bogus);
}

#[test]
fn parse_error_in_import_collected_as_import_parse_failed() {
    let host = vfs(&[
        ("/main.ox", "import \"./broken.ox\";\nfn main() {}\n"),
        ("/broken.ox", "fn !!! garbage\n"),
    ]);
    let mut map = SourceMap::new();
    let (files, _root, errors) = load_program(&host, &mut map, PathBuf::from("/main.ox"));

    // Both files should be loaded; broken.ox's parse errors get
    // bundled into LoadError::ImportParseFailed.
    assert_eq!(files.len(), 2);
    let parse_failures: Vec<_> = errors
        .iter()
        .filter(|e| matches!(e, LoadError::ImportParseFailed { .. }))
        .collect();
    assert!(
        !parse_failures.is_empty(),
        "expected at least one ImportParseFailed; got {errors:#?}"
    );
}
