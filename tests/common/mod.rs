//! Shared helpers for integration tests.
//!
//! `render_typeck` mirrors the rendering of `examples/oxide-typeck-example.rs`
//! but writes to a `String` (color=false) so the typeck snapshot suite can
//! diff it against `.snap` files. Parse / HIR errors panic — the typeck
//! snapshots assume their `.ox` inputs are syntactically and HIR-valid.

use std::fmt::Write;
use std::path::PathBuf;

use oxide::hir::lower;
use oxide::lexer::lex;
use oxide::parser::parse;
use oxide::reporter::{SourceMap, emit, from_typeck_error};
use oxide::typeck::check;

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

    let mut map = SourceMap::new();
    let file = map.add(PathBuf::from(file_name), src.to_string());

    let mut out = String::new();

    out.push_str("== diagnostics ==\n");
    if !type_errors.is_empty() {
        let mut buf: Vec<u8> = Vec::new();
        for err in &type_errors {
            let diag = from_typeck_error(err, file, &results.tys);
            emit(&diag, &map, &mut buf, false).expect("write to Vec failed");
        }
        out.push_str(&String::from_utf8(buf).expect("non-utf8 in diagnostic output"));
    }

    out.push_str("== types ==\n");
    for (fid, sig) in results.fn_sigs.iter_enumerated() {
        let f = &hir.fns[fid];
        let params: Vec<_> = f
            .params
            .iter()
            .zip(&sig.params)
            .map(|(&lid, &ty)| {
                format!(
                    "{}[Local({})]: {}",
                    hir.locals[lid].name,
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
                writeln!(out, "  HExprId({}) : {}", eid.raw(), results.tys.render(ty))
                    .unwrap();
            }
        }
    }

    out
}
