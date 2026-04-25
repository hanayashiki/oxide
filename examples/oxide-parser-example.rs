//! Lex and parse an Oxide source string. Prints the AST as a tree, or renders
//! any diagnostics through the reporter.
//!
//! ```text
//! cargo run --example oxide-parser-example -- -p 'fn add(a: i32, b: i32) { a + b }'
//! Module
//!   Fn add(a: i32, b: i32)
//!     Block
//!       tail: Binary(Add, Ident("a"), Ident("b"))
//! ```

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use oxide::lexer::lex;
use oxide::parser::{parse, pretty::pretty_print};
use oxide::reporter::{SourceMap, emit, from_parse_error};

#[derive(Parser, Debug)]
#[command(name = "oxide-parser-example", version, about)]
struct Args {
    /// Source string to parse.
    #[arg(short = 'p', long = "parse")]
    parse: String,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let tokens = lex(&args.parse);
    let (module, errors) = parse(&tokens);

    if !errors.is_empty() {
        let mut map = SourceMap::new();
        let file = map.add(PathBuf::from("<arg>"), args.parse.clone());
        let stderr = std::io::stderr();
        let color = stderr.is_terminal();
        let mut out = stderr.lock();
        for err in &errors {
            let diag = from_parse_error(err, file);
            emit(&diag, &map, &mut out, color).expect("write to stderr failed");
        }
        return ExitCode::from(1);
    }

    print!("{}", pretty_print(&module));
    ExitCode::SUCCESS
}
