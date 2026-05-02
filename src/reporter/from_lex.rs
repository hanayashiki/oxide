use crate::lexer::LexError;
use super::diagnostic::{Diagnostic, Label};
use super::source_map::{FileId, Span};

/// Map a lex error and its span into a structured diagnostic.
pub fn from_lex_error(err: &LexError, file: FileId, span: Span) -> Diagnostic {
    match err {
        LexError::UnexpectedChar(c) => {
            Diagnostic::error("E0001", format!("unexpected character {:?}", c))
                .with_label(Label::primary(file, span, "not a valid token"))
        }
        LexError::UnterminatedBlockComment => {
            Diagnostic::error("E0002", "unterminated block comment")
                .with_label(Label::primary(file, span, "comment starts here"))
                .with_help("block comments nest; check for an unmatched `/*`")
        }
        LexError::UnterminatedString => {
            Diagnostic::error("E0003", "unterminated string literal")
                .with_label(Label::primary(file, span, "string starts here"))
        }
        LexError::UnterminatedChar => {
            Diagnostic::error("E0004", "unterminated char literal")
                .with_label(Label::primary(file, span, "char literal starts here"))
        }
        LexError::EmptyChar => {
            Diagnostic::error("E0005", "empty char literal")
                .with_label(Label::primary(file, span, "no character between quotes"))
                .with_help("use '\\0' for the null byte")
        }
        LexError::BadEscape => {
            Diagnostic::error("E0006", "invalid escape sequence")
                .with_label(Label::primary(file, span, "this escape is not recognised"))
                .with_help("valid escapes: \\n \\r \\t \\\\ \\' \\\" \\0 \\xHH")
        }
        LexError::IntOverflow => {
            Diagnostic::error("E0007", "integer literal overflows u64")
                .with_label(Label::primary(file, span, "value exceeds 2^64 - 1"))
        }
        LexError::InvalidDigit => {
            Diagnostic::error("E0008", "invalid digit for numeric base")
                .with_label(Label::primary(file, span, "this digit is not valid here"))
        }
    }
}
