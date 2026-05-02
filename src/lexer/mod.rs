mod error;
mod scan;
mod token;

pub use error::LexError;
pub use token::{Token, TokenKind};

use crate::reporter::FileId;

/// Tokenise an Oxide source string. The returned vector always ends with
/// `TokenKind::Eof`. Lex errors are surfaced as `TokenKind::Error(_)`
/// tokens; lexing continues past them.
pub fn lex(src: &str, file: FileId) -> Vec<Token> {
    scan::Lexer::new(src, file).lex()
}
