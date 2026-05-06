//! Shared helpers for integration tests.
//!
//! Each `render_*` produces the snapshot string that the corresponding
//! suite (`tests/{parser,hir,typeck,mono,codegen}_snapshot.rs`) diffs
//! against its `.snap` files. Every renderer is a thin driver that:
//!   1. Builds a `VfsHost` from the fixture (multi-file via
//!      `/// path` headers, or one synthetic file otherwise).
//!   2. Constructs a `Builder` with a per-renderer `Tapper`.
//!   3. Calls `b.run(target_phase)` (or `b.codegen(...)`).
//!   4. Returns the tapper's accumulated string.
//!
//! Per-renderer tappers panic on errors at phases earlier than the
//! target — matching today's behaviour where `lower_fixture` /
//! `render_typeck` / `render_codegen` assert clean input. The target
//! phase's errors are rendered into the snapshot.
//!
//! `dead_code` is allowed because each test binary only calls one of
//! the renderers; the unused ones still get compiled in.

#![allow(dead_code)]

pub mod multi_file;

use std::collections::HashMap;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

use inkwell::OptimizationLevel;
use inkwell::context::Context;
use inkwell::execution_engine::JitFunction;

use oxide::builder::{
    Builder, Flow, Phase, TapAst, TapCodegen, TapHir, TapLoad, TapMono, TapTypeck, Tapper,
};
use oxide::hir::pretty::pretty_print as hir_pretty_print;
use oxide::loader::VfsHost;
use oxide::parser::pretty::pretty_print as parser_pretty_print;
use oxide::reporter::{
    Diagnostic, SourceMap, emit, from_hir_error, from_load_error, from_mono_error,
    from_parse_error, from_typeck_error,
};
use oxide::session::Session;

use self::multi_file::{build_vfs, split_fixture};

/// Split a fixture (possibly multi-file via `/// path` headers) into a
/// `VfsHost` mount and the root file path. Single-file fixtures
/// (no headers) become a one-element mount keyed at `file_name`.
pub fn vfs_for_fixture(file_name: &str, src: &str) -> (VfsHost, PathBuf) {
    let segments = split_fixture(src, Path::new(file_name)).expect("split fixture");
    let root = segments[0].0.clone();
    let host = build_vfs(segments);
    (host, root)
}

/// Append rendered diagnostics for `errs` to `out`. Replaces today's
/// inline `render_diagnostics`.
pub fn write_diags<E>(
    out: &mut String,
    map: &SourceMap,
    errs: &[E],
    to_diag: impl Fn(&E) -> Diagnostic,
) {
    let mut bytes: Vec<u8> = Vec::new();
    for e in errs {
        let d = to_diag(e);
        emit(&d, map, &mut bytes, false).expect("write to Vec failed");
    }
    out.push_str(&String::from_utf8(bytes).expect("non-utf8 in diagnostic output"));
}

fn render_diagnostics(diags: &[Diagnostic], map: &SourceMap) -> String {
    let mut buf: Vec<u8> = Vec::new();
    for d in diags {
        emit(d, map, &mut buf, false).expect("write to Vec failed");
    }
    String::from_utf8(buf).expect("non-utf8 in diagnostic output")
}

/// Render `LoadError`s (each may produce multiple diagnostics) for
/// panic messages when the test expected clean input.
fn render_load_errs(map: &SourceMap, errs: &[oxide::loader::LoadError]) -> String {
    let mut buf: Vec<u8> = Vec::new();
    for e in errs {
        for d in from_load_error(e) {
            emit(&d, map, &mut buf, false).expect("write to Vec failed");
        }
    }
    String::from_utf8(buf).expect("non-utf8 in diagnostic output")
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

// ============================================================
// Per-renderer Tappers and driver functions.
// ============================================================

/// Parser snapshot: lex + parse, render AST + parse-error diagnostics.
/// Inline mode — single-file inspection, imports ignored. Mirrors
/// today's `render_parser` exactly (which never went through the
/// loader).
#[derive(Default)]
struct ParserSnap {
    out: String,
}

impl Tapper for ParserSnap {
    fn on_ast(&mut self, t: TapAst<'_>) -> Flow {
        self.out = parser_pretty_print(t.ast);
        if !t.errors.is_empty() {
            self.out.push_str("== diagnostics ==\n");
            write_diags(&mut self.out, t.source_map, t.errors, |e| {
                from_parse_error(e, t.root)
            });
        }
        Flow::Continue
    }
}

pub fn render_parser(file_name: &str, src: &str) -> String {
    let host = VfsHost::new(HashMap::new());
    let sess = Session::for_test(&host);
    let mut snap = ParserSnap::default();
    {
        let mut b =
            Builder::from_inline(sess, PathBuf::from(file_name), src.to_string(), &mut snap);
        b.run(Phase::Ast);
    }
    snap.out
}

/// HIR snapshot: lower → render HIR + hir-error diagnostics. Multi-file
/// fixtures (with `/// path` headers) flow through the real loader so
/// imports resolve via VFS. Panics on load errors (parse failures in
/// any file) — matches today's `lower_fixture` `assert!` on
/// `parse_errs.is_empty()`.
#[derive(Default)]
struct HirSnap {
    out: String,
}

impl Tapper for HirSnap {
    fn on_load(&mut self, t: TapLoad<'_>) -> Flow {
        if !t.errors.is_empty() {
            panic!(
                "load errors (parse failures or unresolved imports):\n{}",
                render_load_errs(t.source_map, t.errors)
            );
        }
        Flow::Continue
    }

    fn on_hir(&mut self, t: TapHir<'_>) -> Flow {
        self.out = hir_pretty_print(t.hir, t.source_map);
        if !t.errors.is_empty() {
            self.out.push_str("== diagnostics ==\n");
            write_diags(&mut self.out, t.source_map, t.errors, from_hir_error);
        }
        Flow::Continue
    }
}

pub fn render_hir(file_name: &str, src: &str) -> String {
    let (host, root) = vfs_for_fixture(file_name, src);
    let sess = Session::for_test(&host);
    let mut snap = HirSnap::default();
    {
        let mut b = Builder::from_root(sess, root, &mut snap);
        b.run(Phase::Hir);
    }
    snap.out
}

/// Typeck snapshot: typecheck → render `== diagnostics ==` + `==
/// types ==`. Panics on load / hir errors (input must be clean).
#[derive(Default)]
struct TypeckSnap {
    out: String,
}

impl Tapper for TypeckSnap {
    fn on_load(&mut self, t: TapLoad<'_>) -> Flow {
        if !t.errors.is_empty() {
            panic!("load errors:\n{}", render_load_errs(t.source_map, t.errors));
        }
        Flow::Continue
    }

    fn on_hir(&mut self, t: TapHir<'_>) -> Flow {
        if !t.errors.is_empty() {
            let mut s = String::new();
            write_diags(&mut s, t.source_map, t.errors, from_hir_error);
            panic!("hir errors:\n{s}");
        }
        Flow::Continue
    }

    fn on_typeck(&mut self, t: TapTypeck<'_>) -> Flow {
        self.out.push_str("== diagnostics ==\n");
        if !t.errors.is_empty() {
            write_diags(&mut self.out, t.source_map, t.errors, |e| {
                from_typeck_error(e, t.root, &t.results.tys)
            });
        }
        self.out.push_str("== types ==\n");
        write_typeck_signatures(&mut self.out, t.hir, t.results);
        Flow::Continue
    }
}

pub fn render_typeck(file_name: &str, src: &str) -> String {
    let (host, root) = vfs_for_fixture(file_name, src);
    let sess = Session::for_test(&host);
    let mut snap = TypeckSnap::default();
    {
        let mut b = Builder::from_root(sess, root, &mut snap);
        b.run(Phase::Typeck);
    }
    snap.out
}

/// Mono snapshot: full pipeline through monomorphization. Panics on
/// load / hir / typeck errors. Renders `== diagnostics ==` + `==
/// instances ==` + `== sig ==` (depth-limited to 8 per existing
/// behaviour).
#[derive(Default)]
struct MonoSnap {
    out: String,
}

impl Tapper for MonoSnap {
    fn on_load(&mut self, t: TapLoad<'_>) -> Flow {
        if !t.errors.is_empty() {
            panic!("load errors:\n{}", render_load_errs(t.source_map, t.errors));
        }
        Flow::Continue
    }

    fn on_hir(&mut self, t: TapHir<'_>) -> Flow {
        if !t.errors.is_empty() {
            let mut s = String::new();
            write_diags(&mut s, t.source_map, t.errors, from_hir_error);
            panic!("hir errors:\n{s}");
        }
        Flow::Continue
    }

    fn on_typeck(&mut self, t: TapTypeck<'_>) -> Flow {
        if !t.errors.is_empty() {
            let mut s = String::new();
            write_diags(&mut s, t.source_map, t.errors, |e| {
                from_typeck_error(e, t.root, &t.results.tys)
            });
            panic!("type errors:\n{s}");
        }
        Flow::Continue
    }

    fn on_mono(&mut self, t: TapMono<'_>) -> Flow {
        self.out.push_str("== diagnostics ==\n");
        if !t.errors.is_empty() {
            write_diags(&mut self.out, t.source_map, t.errors, |e| {
                from_mono_error(e, t.root, t.hir, &t.results.tys)
            });
        }

        self.out.push_str("== instances ==\n");
        for (iid, inst) in t.mono.instances.iter_enumerated() {
            let parent = match &inst.origin.parent {
                oxide::mono::InstanceParent::Inst(p) => format!("Inst({})", p.raw()),
                oxide::mono::InstanceParent::Fn(fid) => format!("Fn({})", fid.raw()),
            };
            writeln!(
                self.out,
                "Inst[{}] mangled={} depth={} parent={}",
                iid.raw(),
                inst.mangled,
                inst.depth,
                parent,
            )
            .unwrap();
        }

        self.out.push_str("== sig ==\n");
        for (iid, inst) in t.mono.instances.iter_enumerated() {
            let params: Vec<String> = inst
                .param_tys
                .iter()
                .map(|&ty| t.results.tys.render(ty))
                .collect();
            writeln!(
                self.out,
                "Inst[{}] params=[{}] ret={}",
                iid.raw(),
                params.join(", "),
                t.results.tys.render(inst.ret_ty),
            )
            .unwrap();
        }

        Flow::Continue
    }
}

/// Mono uses a small depth_limit (8) to keep the divergent-mono
/// fixtures' rendered cascade chain readable. Production default is
/// 256 — see `Builder::with_mono_depth_limit`.
pub fn render_mono(file_name: &str, src: &str) -> String {
    let (host, root) = vfs_for_fixture(file_name, src);
    let sess = Session::for_test(&host);
    let mut snap = MonoSnap::default();
    {
        let mut b = Builder::from_root(sess, root, &mut snap).with_mono_depth_limit(8);
        b.run(Phase::Mono);
    }
    snap.out
}

/// Codegen snapshot: lex → parse → lower → typeck → mono → codegen,
/// returning the LLVM IR text. Panics on any pre-codegen error.
#[derive(Default)]
struct CodegenSnap {
    out: String,
}

impl Tapper for CodegenSnap {
    fn on_load(&mut self, t: TapLoad<'_>) -> Flow {
        if !t.errors.is_empty() {
            panic!("load errors:\n{}", render_load_errs(t.source_map, t.errors));
        }
        Flow::Continue
    }

    fn on_hir(&mut self, t: TapHir<'_>) -> Flow {
        if !t.errors.is_empty() {
            let mut s = String::new();
            write_diags(&mut s, t.source_map, t.errors, from_hir_error);
            panic!("hir errors:\n{s}");
        }
        Flow::Continue
    }

    fn on_typeck(&mut self, t: TapTypeck<'_>) -> Flow {
        if !t.errors.is_empty() {
            let mut s = String::new();
            write_diags(&mut s, t.source_map, t.errors, |e| {
                from_typeck_error(e, t.root, &t.results.tys)
            });
            panic!("type errors:\n{s}");
        }
        Flow::Continue
    }

    fn on_mono(&mut self, t: TapMono<'_>) -> Flow {
        if !t.errors.is_empty() {
            let mut s = String::new();
            write_diags(&mut s, t.source_map, t.errors, |e| {
                from_mono_error(e, t.root, t.hir, &t.results.tys)
            });
            panic!("mono errors:\n{s}");
        }
        Flow::Continue
    }

    fn on_codegen(&mut self, t: TapCodegen<'_, '_>) -> Flow {
        self.out = t.module.print_to_string().to_string();
        Flow::Continue
    }
}

pub fn render_codegen(file_name: &str, src: &str) -> String {
    let (host, root) = vfs_for_fixture(file_name, src);
    let sess = Session::for_test(&host);
    let mut snap = CodegenSnap::default();
    {
        let mut b = Builder::from_root(sess, root, &mut snap);
        let ctx = Context::create();
        let _ = b.codegen(&ctx, "test").expect("codegen failed");
    }
    snap.out
}

/// JIT-compile `src` end-to-end (lex → parse → HIR → typeck → mono →
/// codegen) and run the function `entry` with no arguments. Returns
/// the LLVM IR text alongside whatever the function returned.
///
/// **Constraints:**
/// - Test programs must compile clean: any parse / HIR / typeck / mono
///   error panics so the test fails loudly with the diagnostic.
/// - `entry` is a no-arg function. Multi-arg variants can be added
///   when needed.
/// - `R` must be a primitive that survives the C ABI return convention
///   for this target (i32, i64, bool, etc.). Struct return is the
///   deferred ABI work; tests should return primitives that encode
///   what they want to verify (e.g., `let p = Point{...}; p.x`).
///
/// Safety: the caller asserts the function's actual return type
/// matches `R`. A mismatch here is undefined behaviour.
pub unsafe fn jit_run_with_ir<R: Copy + 'static>(entry: &str, src: &str) -> (String, R) {
    let (host, root) = vfs_for_fixture(entry, src);
    let sess = Session::for_test(&host);
    let mut tapper = CodegenSnap::default();
    let ctx = Context::create();
    let module = {
        let mut b = Builder::from_root(sess, root, &mut tapper);

        let r = b.codegen(&ctx, "jit");
        r.expect("codegen failed (compile clean expected)")
    };

    // Capture IR text before handing the module to the execution
    // engine (which takes ownership). The string ends with a trailing
    // newline.
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

// ============================================================
// Pure formatter helpers — extracted from the legacy renderers.
// They consume phase outputs and produce strings; the Builder
// wraps them via per-renderer Tapper impls above.
// ============================================================

/// Render the per-fn signature + per-expr type lines that form the
/// body of `render_typeck`'s `== types ==` section.
fn write_typeck_signatures(
    out: &mut String,
    hir: &oxide::hir::HirProgram,
    results: &oxide::typeck::TypeckResults,
) {
    // Const items first, in `ConstId` (= source) order. Renders the
    // resolved annotation type from `const_tys[cid]` plus the value
    // variant — matches the HIR pretty-printer convention. See
    // spec/18_CONST.md.
    for (cid, hc) in hir.consts.iter_enumerated() {
        let ty = results.tys.render(results.const_tys[cid]);
        let value = match &hc.value {
            oxide::hir::HirConstValue::Int(n) => format!("Int({n})"),
            oxide::hir::HirConstValue::Bool(b) => format!("Bool({b})"),
            oxide::hir::HirConstValue::Char(c) => format!("Char({c})"),
            oxide::hir::HirConstValue::Str(s) => format!("Str({s:?})"),
        };
        writeln!(out, "Const[{}] {}: {} = {}", cid.raw(), hc.name, ty, value,).unwrap();
    }
    for (fid, sig) in results.fn_sigs.iter_enumerated() {
        let f = &hir.fns[fid];
        let generic_params: Vec<String> = sig
            .generic_params
            .iter()
            .map(|p| format!("Param({})", p.raw()))
            .collect();
        let generic_str = if generic_params.is_empty() {
            String::new()
        } else {
            format!("<{}>", generic_params.join(", "))
        };
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
            "Fn[{}] {}{}({}) -> {}",
            fid.raw(),
            f.name,
            generic_str,
            params.join(", "),
            results.tys.render(sig.ret),
        )
        .unwrap();
        // Per-fn expr_tys: walk this fn's body to collect the HExprIds
        // it owns, then emit them in numerical order alongside any
        // fn_ref_type_args entry. Per spec/16_GENERIC.md §Typeck rules,
        // generic-fn bodies legitimately carry `Param(_)` in
        // expr_tys; those render via TyArena.render's Param arm.
        let mut owned: std::collections::BTreeSet<oxide::hir::HExprId> =
            std::collections::BTreeSet::new();
        if let Some(body) = f.body {
            collect_block_exprs(hir, body, &mut owned);
        }
        for eid in owned {
            let ty = results.expr_tys[eid];
            // Append fn_ref_type_args inline if this expr is a generic
            // fn-ref site that recorded its resolved args at finalize.
            let cta_str = match results.fn_ref_type_args.get(&eid) {
                Some(args) => {
                    let parts: Vec<_> = args.iter().map(|&t| results.tys.render(t)).collect();
                    format!("  fn_ref_type_args=[{}]", parts.join(", "))
                }
                None => String::new(),
            };
            writeln!(
                out,
                "  HExprId({}) : {}{}",
                eid.raw(),
                results.tys.render(ty),
                cta_str
            )
            .unwrap();
        }
    }
}

/// Collect every `HExprId` reachable from a body block. Used by the
/// typeck renderer to print each fn's expr_tys under its own header
/// (post spec/16, the old "only Fn[0]" gate is gone).
fn collect_block_exprs(
    hir: &oxide::hir::HirProgram,
    bid: oxide::hir::HBlockId,
    out: &mut std::collections::BTreeSet<oxide::hir::HExprId>,
) {
    let block = &hir.blocks[bid];
    for item in &block.items {
        collect_expr(hir, item.expr, out);
    }
}

fn collect_expr(
    hir: &oxide::hir::HirProgram,
    eid: oxide::hir::HExprId,
    out: &mut std::collections::BTreeSet<oxide::hir::HExprId>,
) {
    use oxide::hir::HirExprKind;
    if !out.insert(eid) {
        return;
    }
    match &hir.exprs[eid].kind {
        HirExprKind::IntLit(_)
        | HirExprKind::BoolLit(_)
        | HirExprKind::CharLit(_)
        | HirExprKind::StrLit(_)
        | HirExprKind::Null
        | HirExprKind::Local(_)
        | HirExprKind::Fn(_)
        | HirExprKind::Const(_)
        | HirExprKind::Unresolved(_)
        | HirExprKind::Continue
        | HirExprKind::Poison => {}
        HirExprKind::Unary { expr, .. } => collect_expr(hir, *expr, out),
        HirExprKind::Binary { lhs, rhs, .. } => {
            collect_expr(hir, *lhs, out);
            collect_expr(hir, *rhs, out);
        }
        HirExprKind::Assign { target, rhs, .. } => {
            collect_expr(hir, *target, out);
            collect_expr(hir, *rhs, out);
        }
        HirExprKind::Call { callee, args, .. } => {
            collect_expr(hir, *callee, out);
            for a in args {
                collect_expr(hir, *a, out);
            }
        }
        HirExprKind::Index { base, index } => {
            collect_expr(hir, *base, out);
            collect_expr(hir, *index, out);
        }
        HirExprKind::Field { base, .. } => collect_expr(hir, *base, out),
        HirExprKind::StructLit { fields, .. } => {
            for f in fields {
                collect_expr(hir, f.value, out);
            }
        }
        HirExprKind::AddrOf { expr, .. } => collect_expr(hir, *expr, out),
        HirExprKind::ArrayLit(lit) => match lit {
            oxide::hir::HirArrayLit::Elems(es) => {
                for e in es {
                    collect_expr(hir, *e, out);
                }
            }
            oxide::hir::HirArrayLit::Repeat { init, .. } => collect_expr(hir, *init, out),
        },
        HirExprKind::Cast { expr, .. } => collect_expr(hir, *expr, out),
        HirExprKind::If {
            cond,
            then_block,
            else_arm,
        } => {
            collect_expr(hir, *cond, out);
            collect_block_exprs(hir, *then_block, out);
            if let Some(arm) = else_arm {
                match arm {
                    oxide::hir::HElseArm::Block(b) => collect_block_exprs(hir, *b, out),
                    oxide::hir::HElseArm::If(e) => collect_expr(hir, *e, out),
                }
            }
        }
        HirExprKind::Block(b) => collect_block_exprs(hir, *b, out),
        HirExprKind::Return(v) => {
            if let Some(v) = v {
                collect_expr(hir, *v, out);
            }
        }
        HirExprKind::Loop {
            init,
            cond,
            update,
            body,
            ..
        } => {
            if let Some(e) = init {
                collect_expr(hir, *e, out);
            }
            if let Some(e) = cond {
                collect_expr(hir, *e, out);
            }
            if let Some(e) = update {
                collect_expr(hir, *e, out);
            }
            collect_block_exprs(hir, *body, out);
        }
        HirExprKind::Break { expr } => {
            if let Some(e) = expr {
                collect_expr(hir, *e, out);
            }
        }
        HirExprKind::Let { init, .. } => {
            if let Some(e) = init {
                collect_expr(hir, *e, out);
            }
        }
    }
}
