//! Shared helpers for integration tests.
//!
//! Each `render_*` produces the byte payload that the corresponding
//! snapshot suite (`tests/{parser,hir,typeck}_snapshot.rs`) diffs against
//! its `.snap` files. All three go through the `oxide::reporter` pipeline
//! (color=false) so error output reads as human-friendly diagnostics
//! instead of `Debug`-formatted enum dumps.
//!
//! `render_typeck` always emits a `== diagnostics ==` section followed by
//! `== types ==`. `render_parser` / `render_hir` emit a `== diagnostics ==`
//! section only when the corresponding layer reports errors. `render_hir`
//! panics on parse errors (snapshot inputs must parse cleanly).
//!
//! `dead_code` is allowed because each test binary only calls one of the
//! three renderers; the unused ones still get compiled in.

#![allow(dead_code)]

use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

use inkwell::OptimizationLevel;
use inkwell::context::Context;
use inkwell::execution_engine::JitFunction;

use oxide::codegen::codegen;
use oxide::hir::lower;
use oxide::hir::pretty::pretty_print as hir_pretty_print;
use oxide::lexer::lex;
use oxide::parser::parse;
use oxide::parser::pretty::pretty_print as parser_pretty_print;
use oxide::reporter::{
    Diagnostic, FileId, SourceMap, emit, from_hir_error, from_parse_error, from_typeck_error,
};
use oxide::typeck::check;

fn render_diagnostics(diags: &[Diagnostic], map: &SourceMap) -> String {
    let mut buf: Vec<u8> = Vec::new();
    for d in diags {
        emit(d, map, &mut buf, false).expect("write to Vec failed");
    }
    String::from_utf8(buf).expect("non-utf8 in diagnostic output")
}

fn make_map(file_name: &str, src: &str) -> (SourceMap, FileId) {
    let mut map = SourceMap::new();
    let file = map.add(PathBuf::from(file_name), src.to_string());
    (map, file)
}

/// Walks a snapshot directory, rendering every `<name>.ox` and comparing
/// the output to `<name>.snap`. Diffs go to stderr (one block per failing
/// snapshot — does not fail-fast); the test panics only at the end with a
/// `passed/failed` summary, so a single run shows every regression.
///
/// `UPDATE_EXPECT=1 cargo test ...` rewrites the `.snap` files instead of
/// diffing.
pub fn assert_snapshots(dir: &Path, render: impl Fn(&str, &str) -> String) {
    let update = std::env::var_os("UPDATE_EXPECT").is_some();

    let mut ox_files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ox"))
        .collect();
    ox_files.sort();

    assert!(!ox_files.is_empty(), "no .ox files under {}", dir.display());

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut updated = 0usize;

    for ox_path in &ox_files {
        let stem = ox_path.file_stem().unwrap().to_string_lossy().into_owned();
        let file_name = format!("{stem}.ox");
        let src = fs::read_to_string(ox_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", ox_path.display()));

        let actual = render(&file_name, &src);
        let snap_path = ox_path.with_extension("snap");

        let maybe_update = match fs::read_to_string(&snap_path) {
            Ok(expected) => {
                let matches = expected == actual;
                if matches {
                    passed += 1;
                    None
                } else if update {
                    updated += 1;
                    Some(actual)
                } else {
                    failed += 1;
                    eprintln!("{}", format_mismatch(&snap_path, &expected, &actual));
                    None
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                updated += 1;
                Some(actual)
            }
            Err(e) => panic!("read {}: {e}", snap_path.display()),
        };

        if let Some(new_content) = maybe_update {
            fs::write(&snap_path, new_content)
                .unwrap_or_else(|e| panic!("write {}: {e}", snap_path.display()));
        }
    }

    if update {
        eprintln!("{} snapshot(s) updated under {}", updated, dir.display());
        return;
    }

    eprintln!(
        "snapshot summary for {}: {} passed, {} failed, {} updated",
        dir.display(),
        passed,
        failed,
        updated,
    );

    if failed > 0 {
        panic!(
            "{} snapshot mismatch(es) (see stderr for diffs). Run with UPDATE_EXPECT=1 to bless.",
            failed
        );
    }
}

fn format_mismatch(path: &Path, expected: &str, actual: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "--- {} ---", path.display());
    let exp: Vec<&str> = expected.lines().collect();
    let act: Vec<&str> = actual.lines().collect();
    let n = exp.len().max(act.len());
    for i in 0..n {
        let e = exp.get(i).copied().unwrap_or("");
        let a = act.get(i).copied().unwrap_or("");
        if e == a {
            let _ = writeln!(out, "  {e}");
        } else {
            if i < exp.len() {
                let _ = writeln!(out, "- {e}");
            }
            if i < act.len() {
                let _ = writeln!(out, "+ {a}");
            }
        }
    }
    out
}

pub fn render_parser(file_name: &str, src: &str) -> String {
    let tokens = lex(src);
    let (module, errors) = parse(&tokens);
    let mut out = parser_pretty_print(&module);
    if !errors.is_empty() {
        let (map, file) = make_map(file_name, src);
        let diags: Vec<_> = errors.iter().map(|e| from_parse_error(e, file)).collect();
        out.push_str("== diagnostics ==\n");
        out.push_str(&render_diagnostics(&diags, &map));
    }
    out
}

pub fn render_hir(file_name: &str, src: &str) -> String {
    let tokens = lex(src);
    let (module, parse_errs) = parse(&tokens);
    assert!(
        parse_errs.is_empty(),
        "parse errors in {file_name}: {parse_errs:#?}"
    );
    let (hir, errors) = lower(&module);
    let mut out = hir_pretty_print(&hir);
    if !errors.is_empty() {
        let (map, file) = make_map(file_name, src);
        let diags: Vec<_> = errors.iter().map(|e| from_hir_error(e, file)).collect();
        out.push_str("== diagnostics ==\n");
        out.push_str(&render_diagnostics(&diags, &map));
    }
    out
}

/// JIT-compile `src` end-to-end (lex → parse → HIR → typeck → codegen)
/// and run the function `entry` with no arguments. Returns the LLVM IR
/// text alongside whatever the function returned.
///
/// **Constraints:**
/// - Test programs must compile clean: any parse / HIR / typeck error
///   panics so the test fails loudly with the diagnostic.
/// - `entry` is a no-arg function. Multi-arg variants can be added when
///   needed.
/// - `R` must be a primitive that survives the C ABI return convention
///   for this target (i32, i64, bool, etc.). Struct return is the
///   deferred ABI work; tests should return primitives that encode
///   what they want to verify (e.g., `let p = Point{...}; p.x`).
///
/// Safety: the caller asserts the function's actual return type matches
/// `R`. A mismatch here is undefined behaviour.
pub unsafe fn jit_run_with_ir<R: Copy + 'static>(src: &str, entry: &str) -> (String, R) {
    let tokens = lex(src);
    let (ast, parse_errs) = parse(&tokens);
    assert!(parse_errs.is_empty(), "parse errors: {parse_errs:#?}");
    let (hir, hir_errs) = lower(&ast);
    assert!(hir_errs.is_empty(), "hir errors: {hir_errs:#?}");
    let (results, type_errs) = check(&hir);
    assert!(type_errs.is_empty(), "type errors: {type_errs:#?}");

    let ctx = Context::create();
    let module = codegen(&ctx, &hir, &results, "jit");

    // Capture IR text before handing the module to the execution engine
    // (which takes ownership). The string ends with a trailing newline.
    let ir = module.print_to_string().to_string();

    let ee = module
        .create_jit_execution_engine(OptimizationLevel::None)
        .expect("failed to create JIT execution engine");

    let func: JitFunction<'_, unsafe extern "C" fn() -> R> = unsafe {
        ee.get_function(entry)
            .unwrap_or_else(|e| panic!("function `{entry}` not found in JIT module: {e:?}"))
    };
    let result = unsafe { func.call() };
    (ir, result)
}

/// Convenience wrapper around `jit_run_with_ir` for callers that only
/// want the runtime result.
pub unsafe fn jit_run<R: Copy + 'static>(src: &str, entry: &str) -> R {
    let (_ir, r) = unsafe { jit_run_with_ir::<R>(src, entry) };
    r
}

pub fn render_typeck(file_name: &str, src: &str) -> String {
    let tokens = lex(src);
    let (ast, parse_errs) = parse(&tokens);
    assert!(
        parse_errs.is_empty(),
        "parse errors in {file_name}: {parse_errs:#?}"
    );
    let (hir, hir_errs) = lower(&ast);
    assert!(
        hir_errs.is_empty(),
        "hir errors in {file_name}: {hir_errs:#?}"
    );
    let (results, type_errors) = check(&hir);

    let (map, file) = make_map(file_name, src);

    let mut out = String::new();

    out.push_str("== diagnostics ==\n");
    if !type_errors.is_empty() {
        let diags: Vec<_> = type_errors
            .iter()
            .map(|e| from_typeck_error(e, file, &results.tys))
            .collect();
        out.push_str(&render_diagnostics(&diags, &map));
    }

    out.push_str("== types ==\n");
    for (fid, sig) in results.fn_sigs.iter_enumerated() {
        let f = &hir.fns[fid];
        let params: Vec<_> = f
            .params
            .iter()
            .zip(&sig.params)
            .map(|(&lid, &ty)| {
                let local = &hir.locals[lid];
                let mut_kw = if local.mutable { "mut " } else { "" };
                format!(
                    "{}{}[Local({})]: {}",
                    mut_kw,
                    local.name,
                    lid.raw(),
                    results.tys.render(ty)
                )
            })
            .collect();
        writeln!(
            out,
            "Fn[{}] {}({}) -> {}",
            fid.raw(),
            f.name,
            params.join(", "),
            results.tys.render(sig.ret),
        )
        .unwrap();
        for (eid, &ty) in results.expr_tys.iter_enumerated() {
            if fid.raw() == 0 {
                writeln!(out, "  HExprId({}) : {}", eid.raw(), results.tys.render(ty)).unwrap();
            }
        }
    }

    out
}
