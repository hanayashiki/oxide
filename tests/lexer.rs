use oxide::lexer::{BytePos, LexError, LspPos, Span, Token, TokenKind, lex};

fn kinds(src: &str) -> Vec<TokenKind> {
    lex(src).into_iter().map(|t| t.kind).collect()
}

fn ident(s: &str) -> TokenKind {
    TokenKind::Ident(s.to_string())
}

fn s(s: &str) -> TokenKind {
    TokenKind::Str(s.to_string())
}

// ---------- empty / trivia ----------

#[test]
fn empty_input_yields_only_eof() {
    let toks = lex("");
    assert_eq!(toks.len(), 1);
    assert_eq!(toks[0].kind, TokenKind::Eof);
    assert_eq!(toks[0].span.start, BytePos::new(0));
    assert_eq!(toks[0].span.end, BytePos::new(0));
    assert_eq!(toks[0].span.lsp_start, LspPos::new(0, 0));
}

#[test]
fn whitespace_only_yields_only_eof() {
    assert_eq!(kinds("   \t\n\r\n"), vec![TokenKind::Eof]);
}

#[test]
fn line_comment_skipped() {
    assert_eq!(kinds("// hello world\n"), vec![TokenKind::Eof]);
}

#[test]
fn block_comment_skipped() {
    assert_eq!(kinds("/* hi */"), vec![TokenKind::Eof]);
}

#[test]
fn nested_block_comment_skipped() {
    assert_eq!(
        kinds("/* a /* b */ c */ x"),
        vec![ident("x"), TokenKind::Eof]
    );
}

#[test]
fn unterminated_block_comment_is_error() {
    let toks = kinds("/* never closed");
    assert_eq!(
        toks,
        vec![
            TokenKind::Error(LexError::UnterminatedBlockComment),
            TokenKind::Eof,
        ]
    );
}

// ---------- keywords ----------

#[test]
fn all_real_keywords() {
    use TokenKind::*;
    assert_eq!(
        kinds("fn let mut if else while for return break continue struct enum as null sizeof"),
        vec![
            KwFn, KwLet, KwMut, KwIf, KwElse, KwWhile, KwFor, KwReturn, KwBreak, KwContinue,
            KwStruct, KwEnum, KwAs, KwNull, KwSizeof, Eof,
        ],
    );
}

#[test]
fn reserved_keywords() {
    use TokenKind::*;
    assert_eq!(
        kinds("match impl trait pub use mod"),
        vec![KwMatch, KwImpl, KwTrait, KwPub, KwUse, KwMod, Eof],
    );
}

#[test]
fn true_false_are_bool_literals() {
    use TokenKind::*;
    assert_eq!(kinds("true false"), vec![Bool(true), Bool(false), Eof]);
}

// ---------- identifiers ----------

#[test]
fn identifiers() {
    assert_eq!(
        kinds("_ _x x_1 Foo123"),
        vec![
            ident("_"),
            ident("_x"),
            ident("x_1"),
            ident("Foo123"),
            TokenKind::Eof
        ],
    );
}

#[test]
fn keyword_prefix_is_still_ident() {
    // `fnord` is not the keyword `fn`.
    assert_eq!(kinds("fnord"), vec![ident("fnord"), TokenKind::Eof]);
}

// ---------- integers ----------

#[test]
fn decimal_int() {
    use TokenKind::*;
    assert_eq!(
        kinds("0 1 42 1_000_000"),
        vec![Int(0), Int(1), Int(42), Int(1_000_000), Eof]
    );
}

#[test]
fn hex_int_mixed_case() {
    use TokenKind::*;
    assert_eq!(
        kinds("0xFF 0xff 0xAbCd"),
        vec![Int(0xFF), Int(0xFF), Int(0xABCD), Eof]
    );
}

#[test]
fn binary_int() {
    use TokenKind::*;
    assert_eq!(kinds("0b101 0b1_0_1"), vec![Int(0b101), Int(0b101), Eof]);
}

#[test]
fn int_overflow() {
    let toks = kinds("99999999999999999999"); // > u64::MAX
    assert_eq!(
        toks,
        vec![TokenKind::Error(LexError::IntOverflow), TokenKind::Eof]
    );
}

#[test]
fn invalid_digit_for_base() {
    assert_eq!(
        kinds("0b2"),
        vec![TokenKind::Error(LexError::InvalidDigit), TokenKind::Eof]
    );
}

#[test]
fn empty_hex_prefix_is_invalid_digit() {
    assert_eq!(
        kinds("0x"),
        vec![TokenKind::Error(LexError::InvalidDigit), TokenKind::Eof]
    );
}

// ---------- chars ----------

#[test]
fn char_literals() {
    use TokenKind::*;
    assert_eq!(
        kinds(r"'a' '\n' '\\' '\'' '\x7F'"),
        vec![
            Char('a'),
            Char('\n'),
            Char('\\'),
            Char('\''),
            Char('\x7F'),
            Eof
        ],
    );
}

#[test]
fn empty_char_literal() {
    assert_eq!(
        kinds("''"),
        vec![TokenKind::Error(LexError::EmptyChar), TokenKind::Eof]
    );
}

#[test]
fn unterminated_char_at_eof() {
    assert_eq!(
        kinds("'a"),
        vec![TokenKind::Error(LexError::UnterminatedChar), TokenKind::Eof]
    );
}

#[test]
fn bad_escape_in_char() {
    assert_eq!(
        kinds(r"'\q'"),
        vec![TokenKind::Error(LexError::BadEscape), TokenKind::Eof]
    );
}

// ---------- strings ----------

#[test]
fn string_literals() {
    assert_eq!(
        kinds(r#""" "abc" "a\nb""#),
        vec![s(""), s("abc"), s("a\nb"), TokenKind::Eof]
    );
}

#[test]
fn unterminated_string_at_eof() {
    assert_eq!(
        kinds(r#""abc"#),
        vec![
            TokenKind::Error(LexError::UnterminatedString),
            TokenKind::Eof
        ],
    );
}

#[test]
fn unterminated_string_at_newline() {
    assert_eq!(
        kinds("\"abc\nrest"),
        vec![
            TokenKind::Error(LexError::UnterminatedString),
            ident("rest"),
            TokenKind::Eof,
        ],
    );
}

#[test]
fn bad_escape_in_string_recovers_and_emits_str() {
    let toks = kinds(r#""a\qb""#);
    // BadEscape error followed by the rest of the string ("ab").
    assert_eq!(
        toks,
        vec![
            TokenKind::Error(LexError::BadEscape),
            s("ab"),
            TokenKind::Eof
        ],
    );
}

// ---------- operators / punctuation ----------

#[test]
fn one_char_ops_and_punct() {
    use TokenKind::*;
    assert_eq!(
        kinds("( ) { } [ ] , ; : . + - * / % = < > ! & | ^ ~"),
        vec![
            LParen, RParen, LBrace, RBrace, LBracket, RBracket, Comma, Semi, Colon, Dot, Plus,
            Minus, Star, Slash, Percent, Eq, Lt, Gt, Bang, Amp, Pipe, Caret, Tilde, Eof,
        ],
    );
}

#[test]
fn two_char_ops() {
    use TokenKind::*;
    assert_eq!(
        kinds("== != <= >= && || << >> -> :: .. += -= *= /= %= &= |= ^="),
        vec![
            EqEq, Ne, Le, Ge, AndAnd, OrOr, Shl, Shr, Arrow, ColonColon, DotDot, PlusEq, MinusEq,
            StarEq, SlashEq, PercentEq, AmpEq, PipeEq, CaretEq, Eof,
        ],
    );
}

#[test]
fn three_char_ops() {
    use TokenKind::*;
    assert_eq!(kinds("<<= >>="), vec![ShlEq, ShrEq, Eof]);
}

#[test]
fn longest_match_shleq_is_one_token() {
    use TokenKind::*;
    // <<= must lex as one ShlEq, not Shl + Eq or Lt + Lt + Eq.
    let toks = kinds("a <<= b");
    assert_eq!(toks, vec![ident("a"), ShlEq, ident("b"), Eof]);
}

#[test]
fn longest_match_eqeq_vs_eq() {
    use TokenKind::*;
    assert_eq!(kinds("== ="), vec![EqEq, Eq, Eof]);
}

// ---------- spans ----------

#[test]
fn byte_span_for_simple_token() {
    let toks = lex("foo");
    assert_eq!(toks[0].span.start, BytePos::new(0));
    assert_eq!(toks[0].span.end, BytePos::new(3));
    assert_eq!(toks[0].span.lsp_start, LspPos::new(0, 0));
    assert_eq!(toks[0].span.lsp_end, LspPos::new(0, 3));
}

#[test]
fn span_across_newline() {
    // "foo\nbar"
    let toks = lex("foo\nbar");
    assert_eq!(toks.len(), 3); // foo, bar, eof
    let bar = &toks[1].span;
    assert_eq!(bar.start, BytePos::new(4));
    assert_eq!(bar.end, BytePos::new(7));
    assert_eq!(bar.lsp_start, LspPos::new(1, 0));
    assert_eq!(bar.lsp_end, LspPos::new(1, 3));
}

#[test]
fn span_across_non_ascii_in_string() {
    // Source: "é"x  — 'é' is U+00E9, UTF-8 length 2, UTF-16 length 1.
    // Bytes:   "  é(0xC3,0xA9)  "  x
    // Offsets: 0  1            3  4   5
    let src = "\"é\"x";
    let toks = lex(src);
    // Tokens: Str("é"), Ident("x"), Eof.
    assert_eq!(toks[0].kind, s("é"));
    assert_eq!(toks[0].span.start, BytePos::new(0));
    assert_eq!(toks[0].span.end, BytePos::new(4)); // 1 + 2 + 1
    assert_eq!(toks[0].span.lsp_start, LspPos::new(0, 0));
    assert_eq!(toks[0].span.lsp_end, LspPos::new(0, 3)); // 1 + 1 + 1
    assert_eq!(toks[1].kind, ident("x"));
    assert_eq!(toks[1].span.start, BytePos::new(4));
    assert_eq!(toks[1].span.lsp_start, LspPos::new(0, 3));
}

#[test]
fn eof_span_is_zero_width_at_end() {
    let toks = lex("ab");
    let eof = toks.last().unwrap();
    assert_eq!(eof.kind, TokenKind::Eof);
    assert_eq!(eof.span.start, BytePos::new(2));
    assert_eq!(eof.span.end, BytePos::new(2));
}

// ---------- error recovery ----------

#[test]
fn unexpected_char_then_continues() {
    use TokenKind::*;
    // `@` is not in any token; we should emit Error then continue with the ident.
    let toks = kinds("@foo");
    assert_eq!(
        toks,
        vec![Error(LexError::UnexpectedChar('@')), ident("foo"), Eof]
    );
}

#[test]
fn invalid_digit_then_continues() {
    use TokenKind::*;
    let toks = kinds("0b2 + 1");
    assert_eq!(toks, vec![Error(LexError::InvalidDigit), Plus, Int(1), Eof]);
}

// ---------- integration smoke ----------

#[test]
fn small_program() {
    use TokenKind::*;
    let src = "fn add(a: i32, b: i32) -> i32 { return a + b; }";
    assert_eq!(
        kinds(src),
        vec![
            KwFn,
            ident("add"),
            LParen,
            ident("a"),
            Colon,
            ident("i32"),
            Comma,
            ident("b"),
            Colon,
            ident("i32"),
            RParen,
            Arrow,
            ident("i32"),
            LBrace,
            KwReturn,
            ident("a"),
            Plus,
            ident("b"),
            Semi,
            RBrace,
            Eof,
        ],
    );
}

// ---------- silence unused-import warnings if a Span helper isn't used ----------

#[test]
fn _span_struct_is_constructible() {
    let _ = Span::new(
        BytePos::new(0),
        BytePos::new(0),
        LspPos::new(0, 0),
        LspPos::new(0, 0),
    );
    let _: Token = Token {
        kind: TokenKind::Eof,
        span: Span::new(
            BytePos::new(0),
            BytePos::new(0),
            LspPos::new(0, 0),
            LspPos::new(0, 0),
        ),
    };
}
