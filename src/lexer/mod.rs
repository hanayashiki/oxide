mod error;
mod scan;
mod span;
mod token;

pub use error::LexError;
pub use span::{BytePos, LspPos, Span};
pub use token::{Token, TokenKind};

/// Tokenise an Oxide source string. The returned vector always ends with
/// `TokenKind::Eof`. Lex errors are surfaced as `TokenKind::Error(_)`
/// tokens; lexing continues past them.
pub fn lex(src: &str) -> Vec<Token> {
    scan::Lexer::new(src).lex()
}
