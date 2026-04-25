//! Adapter from chumsky's `Rich` errors to our `ParseError` enum. Reserved
//! keywords (E0104) are classified here too — they show up as
//! "unexpected token" from chumsky's perspective, but we want a distinct
//! diagnostic.

use chumsky::error::{Rich, RichPattern, RichReason};
use chumsky::span::SimpleSpan;

use crate::lexer::TokenKind;
use crate::parser::error::ParseError;

use super::builder::ModuleBuilder;

pub(super) fn rich_to_parse_error(
    rich: Rich<'_, TokenKind, SimpleSpan>,
    builder: &ModuleBuilder,
) -> ParseError {
    let span = builder.span(*rich.span());
    let expected: Vec<&'static str> = rich
        .expected()
        .map(|p| match p {
            RichPattern::Token(_) => "token",
            RichPattern::Label(_) => "construct",
            RichPattern::Identifier(_) => "identifier",
            RichPattern::Any => "any token",
            RichPattern::SomethingElse => "something",
            RichPattern::EndOfInput => "end of input",
            _ => "?",
        })
        .collect();

    match rich.reason() {
        RichReason::ExpectedFound { found, .. } => {
            if let Some(tok) = found {
                let kind: TokenKind = (**tok).clone();
                if let Some(kw) = reserved_kw_label(&kind) {
                    return ParseError::ReservedKeyword { kw, span };
                }
                ParseError::UnexpectedToken { expected, found: kind, span }
            } else {
                ParseError::UnexpectedEof { expected, span }
            }
        }
        RichReason::Custom(msg) => ParseError::Custom {
            message: msg.clone(),
            span,
        },
    }
}

fn reserved_kw_label(t: &TokenKind) -> Option<&'static str> {
    Some(match t {
        TokenKind::KwMatch => "match",
        TokenKind::KwImpl => "impl",
        TokenKind::KwTrait => "trait",
        TokenKind::KwPub => "pub",
        TokenKind::KwUse => "use",
        TokenKind::KwMod => "mod",
        _ => return None,
    })
}
