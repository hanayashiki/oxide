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
    KwImport,
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
    DotDotDot,

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
    /// `>` followed by whitespace or EOF. Closes one generic bracket;
    /// at expression Pratt level 5 it's the comparison operator `>`.
    Gt,
    /// `>` followed by *any non-whitespace* character (another `>`, `=`,
    /// or a closing punctuator like `(`, `;`, `,`). Closes one generic
    /// bracket just like `Gt`. The parser recombines multi-token forms:
    /// `JointGt Eq` → `>=`, `JointGt Gt` → `>>`, `JointGt JointGt Eq` → `>>=`.
    /// This per-character split lets `Foo<Bar<T>>`, `Vec<Vec<i32>>=0`,
    /// and `ox_alloc::<Node<T>>()` all parse without forcing a space.
    /// See `spec/01_LEXER.md` "Joint `>` rule".
    JointGt,
    AndAnd,
    OrOr,
    Bang,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    AmpEq,
    PipeEq,
    CaretEq,
    ShlEq,

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
        "import" => TokenKind::KwImport,
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
