//! Integration tests for the bundled stdlib (`stdio.ox`, `stdlib.ox`,
//! `string.ox`).
//!
//! Verifies:
//!  - Bundled files are auto-mounted when `VfsHost::new` is called with
//!    an empty user mount table.
//!  - Stdlib name resolution wins over disk fallthrough (the spec's
//!    "stdlib hardcode first" rule).
//!  - User mounts override the bundled stdlib (collision policy).
//!  - `import "stdio.ox";` from a real program loads, lowers, and
//!    typechecks clean.
//!  - Each bundled file lowers + typechecks in isolation (regression
//!    guard for future edits to `stdlib/*.ox`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use oxide::hir::lower_program;
use oxide::loader::{BuilderHost, VfsHost, load_program};
use oxide::reporter::SourceMap;
use oxide::typeck::check;

fn vfs(entries: &[(&str, &str)]) -> VfsHost {
    VfsHost::new(
        entries
            .iter()
            .map(|(k, v)| (PathBuf::from(k), v.to_string()))
            .collect::<HashMap<_, _>>(),
    )
}

#[test]
fn stdlib_auto_mounted_on_vfs_new() {
    let host = VfsHost::new(HashMap::new());
    let s = host
        .read(Path::new("stdio.ox"))
        .expect("stdio.ox should be auto-mounted");
    assert!(
        s.contains("fn puts"),
        "expected `fn puts` in bundled stdio.ox, got len={}",
        s.len()
    );

    let stdlib_src = host
        .read(Path::new("stdlib.ox"))
        .expect("stdlib.ox should be auto-mounted");
    assert!(stdlib_src.contains("fn malloc"));

    let string_src = host
        .read(Path::new("string.ox"))
        .expect("string.ox should be auto-mounted");
    assert!(string_src.contains("fn strlen"));
}

#[test]
fn stdlib_resolution_returns_bare_name_key() {
    // From any importing path, `import "stdio.ox";` resolves to the
    // bare name (the mount key), not joined with the importer's parent.
    let host = VfsHost::new(HashMap::new());
    let r = host
        .resolve(Path::new("/some/deep/dir/main.ox"), "stdio.ox")
        .expect("stdlib resolve");
    assert_eq!(r, PathBuf::from("stdio.ox"));
}

#[test]
fn user_mount_overrides_bundled_stdlib() {
    // Pass a user mount keyed exactly like a stdlib entry; the user
    // bytes win because `VfsHost::new` extends after pre-mounting.
    let user_replacement = "// user-replaced stdio\nextern \"C\" { fn puts(s: *const [u8]) -> i32; }\n";
    let host = vfs(&[("stdio.ox", user_replacement)]);
    let s = host.read(Path::new("stdio.ox")).expect("read");
    assert_eq!(s, user_replacement);
}

#[test]
fn import_stdio_compiles_and_uses_puts() {
    // Smoke: a real program imports stdio.ox and calls puts. Should
    // load, lower, and typecheck without errors.
    let host = vfs(&[(
        "/main.ox",
        "import \"stdio.ox\";\n\
         fn main() -> i32 { puts(\"hello stdlib\"); 0 }\n",
    )]);
    let mut map = SourceMap::new();
    let (files, root, load_errs) = load_program(&host, &mut map, PathBuf::from("/main.ox"));
    assert!(load_errs.is_empty(), "load errors: {load_errs:#?}");
    assert_eq!(files.len(), 2, "main + stdio.ox");

    let (hir, hir_errs) = lower_program(files, root);
    assert!(hir_errs.is_empty(), "hir errors: {hir_errs:#?}");

    let (_results, type_errs) = check(&hir);
    assert!(type_errs.is_empty(), "type errors: {type_errs:#?}");
}

#[test]
fn each_bundled_file_lowers_clean() {
    // Regression guard: each stdlib file imported alone (with a tiny
    // wrapper main) must load + lower + typecheck cleanly. Catches
    // future edits that break parsing of bundled files.
    for (lib, sniff_fn) in [
        ("stdio.ox", "puts"),
        ("stdlib.ox", "malloc"),
        ("string.ox", "strlen"),
    ] {
        let main_src = format!(
            "import \"{lib}\";\n\
             fn main() -> i32 {{ 0 }}\n"
        );
        let host = vfs(&[("/main.ox", &main_src)]);
        let mut map = SourceMap::new();
        let (files, root, load_errs) =
            load_program(&host, &mut map, PathBuf::from("/main.ox"));
        assert!(
            load_errs.is_empty(),
            "load_errs for {lib}: {load_errs:#?}"
        );
        assert_eq!(files.len(), 2, "{lib} loads alongside main");

        let (hir, hir_errs) = lower_program(files, root);
        assert!(hir_errs.is_empty(), "hir errors for {lib}: {hir_errs:#?}");

        let (_results, type_errs) = check(&hir);
        assert!(
            type_errs.is_empty(),
            "type errors for {lib}: {type_errs:#?}"
        );

        // Sniff that the expected fn made it through to HIR by name.
        let found = hir.fns.iter().any(|f| f.name == sniff_fn);
        assert!(
            found,
            "expected fn `{sniff_fn}` in HIR after importing {lib}"
        );
    }
}
