//! Lex, parse, lower to HIR, and typecheck. Prints the resolved fn
//! signatures + per-expression types, or renders any diagnostics.
//!
//! ```text
//! cargo run --example oxide-typeck-example -- -p 'fn add(a: i32, b: i32) -> i32 { a + b }'
//! ```

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use oxide::hir::lower;
use oxide::lexer::lex;
use oxide::parser::parse;
use oxide::reporter::{
    SourceMap, emit, from_hir_error, from_parse_error, from_typeck_error,
};
use oxide::typeck::check;

#[derive(Parser, Debug)]
#[command(name = "oxide-typeck-example", version, about)]
struct Args {
    /// Source string to typecheck.
    #[arg(short = 'p', long = "parse")]
    parse: String,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let tokens = lex(&args.parse);
    let (ast, parse_errors) = parse(&tokens);

    let mut map = SourceMap::new();
    let file = map.add(PathBuf::from("<arg>"), args.parse.clone());
    let stderr = std::io::stderr();
    let color = stderr.is_terminal();

    if !parse_errors.is_empty() {
        let mut out = stderr.lock();
        for err in &parse_errors {
            let diag = from_parse_error(err, file);
            emit(&diag, &map, &mut out, color).expect("write to stderr failed");
        }
        return ExitCode::from(1);
    }

    let (hir, hir_errors) = lower(&ast);
    if !hir_errors.is_empty() {
        let mut out = stderr.lock();
        for err in &hir_errors {
            let diag = from_hir_error(err, file);
            emit(&diag, &map, &mut out, color).expect("write to stderr failed");
        }
        return ExitCode::from(1);
    }

    let (results, type_errors) = check(&hir);
    if !type_errors.is_empty() {
        let mut out = stderr.lock();
        for err in &type_errors {
            let diag = from_typeck_error(err, file, &results.tys);
            emit(&diag, &map, &mut out, color).expect("write to stderr failed");
        }
        return ExitCode::from(1);
    }

    // Pretty-print the typed results.
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
        println!(
            "Fn[{}] {}({}) -> {}",
            fid.raw(),
            f.name,
            params.join(", "),
            results.tys.render(sig.ret),
        );
        for (eid, &ty) in results.expr_tys.iter_enumerated() {
            // Only show exprs that belong to this fn's body — we don't track
            // ownership precisely, so just dump everything once after all fns.
            if fid.raw() == 0 {
                println!(
                    "  HExprId({}) : {}",
                    eid.raw(),
                    results.tys.render(ty)
                );
            }
        }
    }

    ExitCode::SUCCESS
}
