use crate::lexer::{LexError, Span, TokenKind};

#[derive(Clone, Debug)]
pub enum ParseError {
    UnexpectedToken {
        expected: Vec<&'static str>,
        found: TokenKind,
        span: Span,
    },
    UnexpectedEof {
        expected: Vec<&'static str>,
        span: Span,
    },
    BadStatement {
        span: Span,
    },
    ReservedKeyword {
        kw: &'static str,
        span: Span,
    },
    LexErrorToken {
        err: LexError,
        span: Span,
    },
    InvalidAssignTarget {
        span: Span,
    },
}

impl ParseError {
    pub fn span(&self) -> &Span {
        match self {
            Self::UnexpectedToken { span, .. }
            | Self::UnexpectedEof { span, .. }
            | Self::BadStatement { span }
            | Self::ReservedKeyword { span, .. }
            | Self::LexErrorToken { span, .. }
            | Self::InvalidAssignTarget { span } => span,
        }
    }
}
