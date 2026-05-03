//! Lex, parse, and lower an Oxide source string to HIR. Prints the HIR as
//! a tree (showing resolved `LocalId`/`FnId`s), or renders any diagnostics
//! through the reporter.
//!
//! ```text
//! cargo run --example oxide-hir-example -- -p 'fn add(a: i32, b: i32) { a + b }'
//! HirModule
//!   Fn[0] add(a[Local(0)]: i32, b[Local(1)]: i32)
//!     Block
//!       tail: Binary(Add, Local(0, "a"), Local(1, "b"))
//! ```

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use oxide::hir::{lower, pretty::pretty_print};
use oxide::lexer::lex;
use oxide::parser::parse;
use oxide::reporter::{SourceMap, emit, from_hir_error, from_parse_error};

#[derive(Parser, Debug)]
#[command(name = "oxide-hir-example", version, about)]
struct Args {
    /// Source string to lower to HIR.
    #[arg(short = 'p', long = "parse")]
    parse: String,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let mut map = SourceMap::new();
    let file = map.add(PathBuf::from("<arg>"), args.parse.clone());
    let tokens = lex(&args.parse, file);
    let (module, parse_errors) = parse(&tokens, file);

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

    let (hir, hir_errors) = lower(&module);

    if !hir_errors.is_empty() {
        let mut out = stderr.lock();
        for err in &hir_errors {
            let diag = from_hir_error(err);
            emit(&diag, &map, &mut out, color).expect("write to stderr failed");
        }
        return ExitCode::from(1);
    }

    print!("{}", pretty_print(&hir, &map));
    ExitCode::SUCCESS
}
