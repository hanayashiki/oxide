use super::diagnostic::{Diagnostic, Label};
use super::from_lex::from_lex_error;
use super::source_map::FileId;
use crate::parser::ParseError;

/// Map a parse error and its captured spans into a structured diagnostic.
/// E0105 (`LexErrorToken`) re-uses `from_lex_error` for the message body so
/// users see the underlying lex diagnostic rather than a generic "unexpected
/// token" cascade.
pub fn from_parse_error(err: &ParseError, file: FileId) -> Diagnostic {
    match err {
        ParseError::UnexpectedToken { expected, found, span } => {
            let exp = if expected.is_empty() {
                "(unspecified)".to_string()
            } else {
                expected.join(", ")
            };
            Diagnostic::error("E0101", format!("unexpected token: found {:?}", found))
                .with_label(Label::primary(file, span.clone(), "unexpected here"))
                .with_note(format!("expected: {}", exp))
        }
        ParseError::UnexpectedEof { expected, span } => {
            let exp = if expected.is_empty() {
                "(unspecified)".to_string()
            } else {
                expected.join(", ")
            };
            Diagnostic::error("E0102", "unexpected end of input")
                .with_label(Label::primary(file, span.clone(), "input ended here"))
                .with_note(format!("expected: {}", exp))
        }
        ParseError::BadStatement { span } => {
            Diagnostic::error("E0103", "could not parse this statement")
                .with_label(Label::primary(file, span.clone(), "starting here"))
        }
        ParseError::Custom { message, span } => {
            Diagnostic::error("E0107", message.clone())
                .with_label(Label::primary(file, span.clone(), "here"))
        }
        ParseError::ReservedKeyword { kw, span } => {
            Diagnostic::error("E0104", format!("reserved keyword `{kw}` is not yet supported"))
                .with_label(Label::primary(file, span.clone(), "reserved for future use"))
        }
        ParseError::LexErrorToken { err, span } => from_lex_error(err, file, span.clone()),
    }
}
