//! Lex an Oxide source string and print the token kinds, or render any lex
//! errors through the reporter.
//!
//! ```text
//! cargo run --example oxide-lexer-example -- -l 's'
//! [Ident("s")]
//!
//! cargo run --example oxide-lexer-example -- -l "let s = '''"
//! [E0005] Error: empty char literal
//!    ╭─[ <arg>:1:9 ]
//!   ...
//! ```

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use oxide::lexer::{TokenKind, lex};
use oxide::reporter::{SourceMap, emit, from_lex_error};

#[derive(Parser, Debug)]
#[command(name = "oxide-lexer-example", version, about)]
struct Args {
    /// Source string to lex.
    #[arg(short = 'l', long = "lex")]
    lex: String,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let tokens = lex(&args.lex);

    let errors: Vec<_> = tokens
        .iter()
        .filter_map(|t| match &t.kind {
            TokenKind::Error(e) => Some((e.clone(), t.span.clone())),
            _ => None,
        })
        .collect();

    if !errors.is_empty() {
        let mut map = SourceMap::new();
        let file = map.add(PathBuf::from("<arg>"), args.lex.clone());
        let stderr = std::io::stderr();
        let color = stderr.is_terminal();
        let mut out = stderr.lock();
        for (err, span) in &errors {
            let diag = from_lex_error(err, file, span.clone());
            emit(&diag, &map, &mut out, color).expect("write to stderr failed");
        }
        return ExitCode::from(1);
    }

    let kinds: Vec<TokenKind> = tokens
        .into_iter()
        .map(|t| t.kind)
        .filter(|k| !matches!(k, TokenKind::Eof))
        .collect();
    println!("{:?}", kinds);
    ExitCode::SUCCESS
}
