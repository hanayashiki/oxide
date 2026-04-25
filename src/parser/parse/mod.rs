//! Parser entry point. Builds the chumsky input stream from `Vec<Token>`,
//! runs the combinator tree, and post-processes errors into our `ParseError`
//! type.

mod builder;
mod extra;
mod rich;
mod syntax;

use chumsky::extra::SimpleState;
use chumsky::input::Stream;
use chumsky::prelude::*;
use chumsky::span::SimpleSpan;

use crate::lexer::{BytePos, LspPos, Span, Token, TokenKind};
use crate::parser::ast::Module;
use crate::parser::error::ParseError;

use builder::ModuleBuilder;

/// Parse a token stream into a `Module`. Always returns the `Module`
/// (possibly with empty arenas for empty/all-error input) alongside any
/// recovered `ParseError`s. Recoverable: there is no "failed to parse" case.
pub fn parse(tokens: &[Token]) -> (Module, Vec<ParseError>) {
    let mut builder = ModuleBuilder::default();

    // Build the byte → LSP lookup so AST node spans can be reconstructed
    // from chumsky's byte-only `SimpleSpan`.
    for t in tokens {
        builder.byte_to_lsp.insert(t.span.start.offset, t.span.lsp_start);
        builder.byte_to_lsp.insert(t.span.end.offset, t.span.lsp_end);
    }

    let module_span: Span = match (tokens.first(), tokens.last()) {
        (Some(first), Some(last)) => Span {
            start: first.span.start,
            end: last.span.end,
            lsp_start: first.span.lsp_start,
            lsp_end: last.span.lsp_end,
        },
        _ => Span {
            start: BytePos { offset: 0 },
            end: BytePos { offset: 0 },
            lsp_start: LspPos { line: 0, character: 0 },
            lsp_end: LspPos { line: 0, character: 0 },
        },
    };

    // Filter `Eof` and divert `Error` tokens into the error list as E0105 so
    // chumsky never sees them (avoids cascade "unexpected token" errors).
    let mut eoi_byte = 0usize;
    let mut chumsky_tokens: Vec<(TokenKind, SimpleSpan)> = Vec::with_capacity(tokens.len());
    for t in tokens {
        eoi_byte = t.span.end.offset.max(eoi_byte);
        match &t.kind {
            TokenKind::Eof => {}
            TokenKind::Error(err) => {
                builder.errors.push(ParseError::LexErrorToken {
                    err: err.clone(),
                    span: t.span.clone(),
                });
            }
            kind => {
                let ss: SimpleSpan = (t.span.start.offset..t.span.end.offset).into();
                chumsky_tokens.push((kind.clone(), ss));
            }
        }
    }

    let eoi: SimpleSpan = (eoi_byte..eoi_byte).into();
    let stream = Stream::from_iter(chumsky_tokens.into_iter())
        .map(eoi, |(t, s): (_, _)| (t, s));

    let mut state = SimpleState::from(builder);
    let result = syntax::module_parser().parse_with_state(stream, &mut state);
    let (output, rich_errs) = result.into_output_errors();

    let mut builder = state.0;
    if let Some(items) = output {
        builder.root_items = items;
    }
    for re in rich_errs {
        builder.errors.push(rich::rich_to_parse_error(re, &builder));
    }

    builder.finish(module_span)
}
