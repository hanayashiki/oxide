use crate::reporter::Span;

use super::error::LexError;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum TokenKind {
    // Literals
    Int(u64),
    Bool(bool),
    Char(char),
    Str(String),

    // Identifier
    Ident(String),

    // Keywords
    KwFn,
    KwLet,
    KwMut,
    KwIf,
    KwElse,
    KwWhile,
    KwLoop,
    KwFor,
    KwReturn,
    KwBreak,
    KwContinue,
    KwStruct,
    KwEnum,
    KwAs,
    KwNull,
    KwSizeof,
    KwExtern,
    KwConst,

    // Reserved
    KwMatch,
    KwImpl,
    KwTrait,
    KwPub,
    KwUse,
    KwMod,

    // Punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    ColonColon,
    Arrow,
    Dot,
    DotDot,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
    Bang,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl,
    Shr,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    AmpEq,
    PipeEq,
    CaretEq,
    ShlEq,
    ShrEq,

    // Trivia & control
    Eof,
    Error(LexError),
}

/// Look up a keyword (or `true`/`false` literal) by its lexeme.
/// Returns `None` for plain identifiers.
pub fn keyword_lookup(s: &str) -> Option<TokenKind> {
    Some(match s {
        "fn" => TokenKind::KwFn,
        "let" => TokenKind::KwLet,
        "mut" => TokenKind::KwMut,
        "if" => TokenKind::KwIf,
        "else" => TokenKind::KwElse,
        "while" => TokenKind::KwWhile,
        "loop" => TokenKind::KwLoop,
        "for" => TokenKind::KwFor,
        "return" => TokenKind::KwReturn,
        "break" => TokenKind::KwBreak,
        "continue" => TokenKind::KwContinue,
        "struct" => TokenKind::KwStruct,
        "enum" => TokenKind::KwEnum,
        "as" => TokenKind::KwAs,
        "null" => TokenKind::KwNull,
        "sizeof" => TokenKind::KwSizeof,
        "extern" => TokenKind::KwExtern,
        "const" => TokenKind::KwConst,
        "match" => TokenKind::KwMatch,
        "impl" => TokenKind::KwImpl,
        "trait" => TokenKind::KwTrait,
        "pub" => TokenKind::KwPub,
        "use" => TokenKind::KwUse,
        "mod" => TokenKind::KwMod,
        "true" => TokenKind::Bool(true),
        "false" => TokenKind::Bool(false),
        _ => return None,
    })
}
