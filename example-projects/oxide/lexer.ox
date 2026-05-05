// lexer.ox — stage-1 Oxide lexer.
//
// Source bytes → `Vec<Token>` plus a `StrBuf` "pool" that backs
// identifier and string-literal payloads. Errors are embedded as
// `kind = TK_ERROR` tokens inline; the parser decides what to do
// with them.
//
// Design constraints (see SPEC.M2.md):
//   - Tagged-struct Token (no enum-with-payload in v0).
//   - Pool-interned identifier/string payloads (one StrBuf for the
//     whole pass — saves N malloc/free pairs).
//   - Byte-position spans only — LSP UTF-16 columns deferred.
//   - ASCII source outside string literals; multi-byte UTF-8 inside
//     strings is preserved unchanged.

import "stdlib.ox";
import "stdio.ox";
import "string.ox";
import "intrinsics.ox";
import "./util/vec.ox";
import "./util/strbuf.ox";

// ---------------------------------------------------------------- //
// Token kind tags                                                  //
// ---------------------------------------------------------------- //
//
// Discriminant ranges:
//   0..=9     non-keyword variants (literals, ident, eof, error)
//   10..=39   keywords
//   40..=99   punctuation + operators

fn TK_EOF()    -> u8 { 0 }
fn TK_INT()    -> u8 { 1 }
fn TK_BOOL()   -> u8 { 2 }
fn TK_CHAR()   -> u8 { 3 }
fn TK_STR()    -> u8 { 4 }
fn TK_IDENT()  -> u8 { 5 }
fn TK_ERROR()  -> u8 { 6 }

// Keywords
fn TK_KW_FN()       -> u8 { 10 }
fn TK_KW_LET()      -> u8 { 11 }
fn TK_KW_MUT()      -> u8 { 12 }
fn TK_KW_IF()       -> u8 { 13 }
fn TK_KW_ELSE()     -> u8 { 14 }
fn TK_KW_WHILE()    -> u8 { 15 }
fn TK_KW_LOOP()     -> u8 { 16 }
fn TK_KW_FOR()      -> u8 { 17 }
fn TK_KW_RETURN()   -> u8 { 18 }
fn TK_KW_BREAK()    -> u8 { 19 }
fn TK_KW_CONTINUE() -> u8 { 20 }
fn TK_KW_STRUCT()   -> u8 { 21 }
fn TK_KW_ENUM()     -> u8 { 22 }
fn TK_KW_AS()       -> u8 { 23 }
fn TK_KW_NULL()     -> u8 { 24 }
fn TK_KW_SIZEOF()   -> u8 { 25 }
fn TK_KW_EXTERN()   -> u8 { 26 }
fn TK_KW_IMPORT()   -> u8 { 27 }
fn TK_KW_CONST()    -> u8 { 28 }
fn TK_KW_MATCH()    -> u8 { 29 }
fn TK_KW_IMPL()     -> u8 { 30 }
fn TK_KW_TRAIT()    -> u8 { 31 }
fn TK_KW_PUB()      -> u8 { 32 }
fn TK_KW_USE()      -> u8 { 33 }
fn TK_KW_MOD()      -> u8 { 34 }

// Punctuation
fn TK_LPAREN()    -> u8 { 40 }
fn TK_RPAREN()    -> u8 { 41 }
fn TK_LBRACE()    -> u8 { 42 }
fn TK_RBRACE()    -> u8 { 43 }
fn TK_LBRACKET()  -> u8 { 44 }
fn TK_RBRACKET()  -> u8 { 45 }
fn TK_COMMA()     -> u8 { 46 }
fn TK_SEMI()      -> u8 { 47 }
fn TK_COLON()     -> u8 { 48 }
fn TK_COLONCOLON()-> u8 { 49 }
fn TK_ARROW()     -> u8 { 50 }
fn TK_DOT()       -> u8 { 51 }
fn TK_DOTDOT()    -> u8 { 52 }
fn TK_DOTDOTDOT() -> u8 { 53 }

// Operators
fn TK_PLUS()      -> u8 { 60 }
fn TK_MINUS()     -> u8 { 61 }
fn TK_STAR()      -> u8 { 62 }
fn TK_SLASH()     -> u8 { 63 }
fn TK_PERCENT()   -> u8 { 64 }
fn TK_EQ()        -> u8 { 65 }
fn TK_EQEQ()      -> u8 { 66 }
fn TK_NE()        -> u8 { 67 }
fn TK_LT()        -> u8 { 68 }
fn TK_LE()        -> u8 { 69 }
fn TK_GT()        -> u8 { 70 }    // followed by whitespace/EOF
fn TK_JOINT_GT()  -> u8 { 71 }    // followed by non-whitespace
fn TK_ANDAND()    -> u8 { 72 }
fn TK_OROR()      -> u8 { 73 }
fn TK_BANG()      -> u8 { 74 }
fn TK_AMP()       -> u8 { 75 }
fn TK_PIPE()      -> u8 { 76 }
fn TK_CARET()     -> u8 { 77 }
fn TK_TILDE()     -> u8 { 78 }
fn TK_SHL()       -> u8 { 79 }
fn TK_PLUSEQ()    -> u8 { 80 }
fn TK_MINUSEQ()   -> u8 { 81 }
fn TK_STAREQ()    -> u8 { 82 }
fn TK_SLASHEQ()   -> u8 { 83 }
fn TK_PERCENTEQ() -> u8 { 84 }
fn TK_AMPEQ()     -> u8 { 85 }
fn TK_PIPEEQ()    -> u8 { 86 }
fn TK_CARETEQ()   -> u8 { 87 }
fn TK_SHLEQ()     -> u8 { 88 }

// ---------------------------------------------------------------- //
// LexError tags                                                    //
// ---------------------------------------------------------------- //

fn LE_UNEXPECTED_CHAR()           -> u8 { 0 }
fn LE_UNTERMINATED_BLK_COMMENT()  -> u8 { 1 }
fn LE_UNTERMINATED_STRING()       -> u8 { 2 }
fn LE_UNTERMINATED_CHAR()         -> u8 { 3 }
fn LE_EMPTY_CHAR()                -> u8 { 4 }
fn LE_BAD_ESCAPE()                -> u8 { 5 }
fn LE_INT_OVERFLOW()              -> u8 { 6 }
fn LE_INVALID_DIGIT()             -> u8 { 7 }

// ---------------------------------------------------------------- //
// Token shape                                                      //
// ---------------------------------------------------------------- //

struct Token {
    kind:       u8,
    int_val:    u64,      // TK_INT
    bool_val:   bool,     // TK_BOOL
    char_val:   u32,      // TK_CHAR (Unicode scalar; ASCII-only in v0)
    str_off:    usize,    // TK_IDENT / TK_STR (pool offset)
    str_len:    usize,    // TK_IDENT / TK_STR (pool byte length)
    err_kind:   u8,       // TK_ERROR
    err_byte:   u8,       // TK_ERROR (LE_UNEXPECTED_CHAR's offending byte)
    span_start: usize,
    span_end:   usize,
}

fn token_zero() -> Token {
    Token {
        kind: 0,
        int_val: 0,
        bool_val: false,
        char_val: 0,
        str_off: 0,
        str_len: 0,
        err_kind: 0,
        err_byte: 0,
        span_start: 0,
        span_end: 0,
    }
}

// ---------------------------------------------------------------- //
// Lexer state                                                      //
// ---------------------------------------------------------------- //

struct Lexer {
    src:     *const [u8],
    src_len: usize,
    pos:     usize,
    tokens:  Vec<Token>,
    pool:    StrBuf,
}

fn lex(src: *const [u8], src_len: usize) -> Lexer {
    let mut lx: Lexer = Lexer {
        src:     src,
        src_len: src_len,
        pos:     0,
        tokens:  vec_new::<Token>(),
        pool:    strbuf_new(),
    };
    lex_run(&mut lx);
    lx
}

// ---------------------------------------------------------------- //
// Byte-stream helpers                                              //
// ---------------------------------------------------------------- //

fn lx_at_eof(lx: *const Lexer) -> bool { lx.pos >= lx.src_len }

fn lx_peek(lx: *const Lexer) -> i32 {
    if lx.pos >= lx.src_len { -1 } else { lx.src[lx.pos] as i32 }
}

fn lx_peek_at(lx: *const Lexer, off: usize) -> i32 {
    let p: usize = lx.pos + off;
    if p >= lx.src_len { -1 } else { lx.src[p] as i32 }
}

fn lx_bump(lx: *mut Lexer) -> i32 {
    if lx.pos >= lx.src_len { return -1; }
    let b: u8 = lx.src[lx.pos];
    lx.pos = lx.pos + 1;
    b as i32
}

fn is_ascii_alpha(b: i32) -> bool {
    (b >= 65 && b <= 90) || (b >= 97 && b <= 122)
}

fn is_ascii_digit(b: i32) -> bool {
    b >= 48 && b <= 57
}

fn is_ident_start(b: i32) -> bool {
    is_ascii_alpha(b) || b == 95           // '_'
}

fn is_ident_cont(b: i32) -> bool {
    is_ident_start(b) || is_ascii_digit(b)
}

fn is_whitespace(b: i32) -> bool {
    b == 32 || b == 9 || b == 13 || b == 10    // ' ' '\t' '\r' '\n'
}

fn digit_value(b: i32, radix: u32) -> i32 {
    let v: i32 = if b >= 48 && b <= 57 {
        b - 48
    } else if b >= 97 && b <= 122 {
        b - 97 + 10
    } else if b >= 65 && b <= 90 {
        b - 65 + 10
    } else {
        -1
    };
    if v < 0 || (v as u32) >= radix { -1 } else { v }
}

// ---------------------------------------------------------------- //
// Top-level scan                                                   //
// ---------------------------------------------------------------- //

fn lex_run(lx: *mut Lexer) {
    loop {
        if lex_skip_trivia(lx) {
            // Hit unterminated block comment. The error token has
            // been pushed; finish with EOF.
            let mut eof: Token = token_zero();
            eof.kind = TK_EOF();
            eof.span_start = lx.src_len;
            eof.span_end   = lx.src_len;
            vec_push::<Token>(&mut lx.tokens, eof);
            return;
        }
        let start: usize = lx.pos;
        if lx_at_eof(lx) {
            let mut eof: Token = token_zero();
            eof.kind = TK_EOF();
            eof.span_start = start;
            eof.span_end   = start;
            vec_push::<Token>(&mut lx.tokens, eof);
            return;
        }
        let c: i32 = lx_peek(lx);
        if is_ident_start(c) {
            scan_ident(lx, start);
        } else if is_ascii_digit(c) {
            scan_number(lx, start);
        } else if c == 39 {                          // '\''
            scan_char(lx, start);
        } else if c == 34 {                          // '"'
            scan_string(lx, start);
        } else {
            scan_op_or_unexpected(lx, start, c);
        }
    }
}

// Returns true if a fatal trivia error pushed an error token (caller
// should bail out). Otherwise returns false; lx.pos is at the first
// non-trivia byte (or EOF).
fn lex_skip_trivia(lx: *mut Lexer) -> bool {
    loop {
        if lx_at_eof(lx) { return false; }
        let c: i32 = lx_peek(lx);
        if is_whitespace(c) {
            lx_bump(lx);
        } else if c == 47 && lx_peek_at(lx, 1) == 47 {       // "//"
            lx_bump(lx); lx_bump(lx);
            while !lx_at_eof(lx) && lx_peek(lx) != 10 {
                lx_bump(lx);
            }
        } else if c == 47 && lx_peek_at(lx, 1) == 42 {       // "/*"
            let start: usize = lx.pos;
            lx_bump(lx); lx_bump(lx);
            let mut depth: u32 = 1;
            while depth > 0 {
                if lx_at_eof(lx) {
                    let mut t: Token = token_zero();
                    t.kind       = TK_ERROR();
                    t.err_kind   = LE_UNTERMINATED_BLK_COMMENT();
                    t.span_start = start;
                    t.span_end   = lx.src_len;
                    vec_push::<Token>(&mut lx.tokens, t);
                    return true;
                }
                let a: i32 = lx_peek(lx);
                let b: i32 = lx_peek_at(lx, 1);
                if a == 47 && b == 42 {
                    lx_bump(lx); lx_bump(lx);
                    depth = depth + 1;
                } else if a == 42 && b == 47 {
                    lx_bump(lx); lx_bump(lx);
                    depth = depth - 1;
                } else {
                    lx_bump(lx);
                }
            }
        } else {
            return false;
        }
    }
}

// ---------------------------------------------------------------- //
// Identifier / keyword                                             //
// ---------------------------------------------------------------- //

fn scan_ident(lx: *mut Lexer, start: usize) {
    let begin: usize = lx.pos;
    while !lx_at_eof(lx) && is_ident_cont(lx_peek(lx)) {
        lx_bump(lx);
    }
    let end: usize = lx.pos;
    let len: usize = end - begin;

    // Try keyword lookup first; if no match, intern as Ident.
    let kind: u8 = lookup_keyword(lx.src, begin, len);
    let mut t: Token = token_zero();
    t.span_start = start;
    t.span_end   = end;
    if kind == 255 {
        // Plain identifier
        let off: usize = strbuf_len(&lx.pool);
        let mut i: usize = 0;
        while i < len {
            strbuf_push_byte(&mut lx.pool, lx.src[begin + i]);
            i = i + 1;
        }
        t.kind    = TK_IDENT();
        t.str_off = off;
        t.str_len = len;
    } else if kind == TK_BOOL() + 100 {
        // Special-case marker for "true"
        t.kind     = TK_BOOL();
        t.bool_val = true;
    } else if kind == TK_BOOL() + 101 {
        // Special-case marker for "false"
        t.kind     = TK_BOOL();
        t.bool_val = false;
    } else {
        t.kind = kind;
    }
    vec_push::<Token>(&mut lx.tokens, t);
}

// Returns 255 if no match; TK_BOOL+100 / TK_BOOL+101 for "true" /
// "false" (caller decodes); otherwise the keyword's TK_KW_* tag.
fn lookup_keyword(src: *const [u8], begin: usize, len: usize) -> u8 {
    if kw_eq(src, begin, len, "fn", 2)       { return TK_KW_FN(); }
    if kw_eq(src, begin, len, "let", 3)      { return TK_KW_LET(); }
    if kw_eq(src, begin, len, "mut", 3)      { return TK_KW_MUT(); }
    if kw_eq(src, begin, len, "if", 2)       { return TK_KW_IF(); }
    if kw_eq(src, begin, len, "else", 4)     { return TK_KW_ELSE(); }
    if kw_eq(src, begin, len, "while", 5)    { return TK_KW_WHILE(); }
    if kw_eq(src, begin, len, "loop", 4)     { return TK_KW_LOOP(); }
    if kw_eq(src, begin, len, "for", 3)      { return TK_KW_FOR(); }
    if kw_eq(src, begin, len, "return", 6)   { return TK_KW_RETURN(); }
    if kw_eq(src, begin, len, "break", 5)    { return TK_KW_BREAK(); }
    if kw_eq(src, begin, len, "continue", 8) { return TK_KW_CONTINUE(); }
    if kw_eq(src, begin, len, "struct", 6)   { return TK_KW_STRUCT(); }
    if kw_eq(src, begin, len, "enum", 4)     { return TK_KW_ENUM(); }
    if kw_eq(src, begin, len, "as", 2)       { return TK_KW_AS(); }
    if kw_eq(src, begin, len, "null", 4)     { return TK_KW_NULL(); }
    if kw_eq(src, begin, len, "sizeof", 6)   { return TK_KW_SIZEOF(); }
    if kw_eq(src, begin, len, "extern", 6)   { return TK_KW_EXTERN(); }
    if kw_eq(src, begin, len, "import", 6)   { return TK_KW_IMPORT(); }
    if kw_eq(src, begin, len, "const", 5)    { return TK_KW_CONST(); }
    if kw_eq(src, begin, len, "match", 5)    { return TK_KW_MATCH(); }
    if kw_eq(src, begin, len, "impl", 4)     { return TK_KW_IMPL(); }
    if kw_eq(src, begin, len, "trait", 5)    { return TK_KW_TRAIT(); }
    if kw_eq(src, begin, len, "pub", 3)      { return TK_KW_PUB(); }
    if kw_eq(src, begin, len, "use", 3)      { return TK_KW_USE(); }
    if kw_eq(src, begin, len, "mod", 3)      { return TK_KW_MOD(); }
    if kw_eq(src, begin, len, "true", 4)     { return TK_BOOL() + 100; }
    if kw_eq(src, begin, len, "false", 5)    { return TK_BOOL() + 101; }
    return 255;                                  // workaround stage-0 bug
}

fn kw_eq(src: *const [u8], begin: usize, len: usize, kw: *const [u8], kw_len: usize) -> bool {
    if len != kw_len { return false; }
    let mut i: usize = 0;
    while i < len {
        if src[begin + i] != kw[i] { return false; }
        i = i + 1;
    }
    true
}

// ---------------------------------------------------------------- //
// Number literal                                                   //
// ---------------------------------------------------------------- //

fn scan_number(lx: *mut Lexer, start: usize) {
    // We've checked first char is a digit, but haven't consumed it.
    let first: i32 = lx_bump(lx);
    let mut radix: u32 = 10;
    let mut had_prefix: bool = false;
    if first == 48 {                                       // '0'
        let p: i32 = lx_peek(lx);
        if p == 120 || p == 88 {                            // x/X
            lx_bump(lx);
            radix = 16;
            had_prefix = true;
        } else if p == 98 || p == 66 {                       // b/B
            lx_bump(lx);
            radix = 2;
            had_prefix = true;
        }
    }

    let mut value: u64 = 0;
    let mut overflow: bool = false;
    let mut invalid: bool = false;
    let mut any_digit: bool = false;

    if !had_prefix {
        // The first digit was consumed; account for it now.
        if radix == 10 {
            value = (first - 48) as u64;
            any_digit = true;
        }
    }

    loop {
        let c: i32 = lx_peek(lx);
        if c == 95 {                                       // '_' separator
            lx_bump(lx);
        } else {
            let d: i32 = digit_value(c, radix);
            if d >= 0 {
                lx_bump(lx);
                any_digit = true;
                let prev: u64 = value;
                value = prev * (radix as u64) + (d as u64);
                // detect overflow: if (prev * radix + d) wraps, value
                // becomes < prev. Only check post-multiply — `radix`
                // is small so the divide-back trick is precise.
                if value < prev {
                    overflow = true;
                }
            } else if is_ascii_digit(c) {
                // A digit, but not in this radix (e.g. '8' under 0b).
                lx_bump(lx);
                invalid = true;
            } else {
                break;
            }
        }
    }

    let mut t: Token = token_zero();
    t.span_start = start;
    t.span_end   = lx.pos;
    if invalid {
        t.kind     = TK_ERROR();
        t.err_kind = LE_INVALID_DIGIT();
    } else if !any_digit {
        // `0x` or `0b` with no digits.
        t.kind     = TK_ERROR();
        t.err_kind = LE_INVALID_DIGIT();
    } else if overflow {
        t.kind     = TK_ERROR();
        t.err_kind = LE_INT_OVERFLOW();
    } else {
        t.kind    = TK_INT();
        t.int_val = value;
    }
    vec_push::<Token>(&mut lx.tokens, t);
}

// ---------------------------------------------------------------- //
// Char and string literals                                         //
// ---------------------------------------------------------------- //

// Returns -1 on bad escape, -2 on EOF mid-escape, otherwise the
// scalar value (0..=127 in v0).
fn read_escape(lx: *mut Lexer) -> i32 {
    if lx_at_eof(lx) { return -2; }
    let c: i32 = lx_bump(lx);
    if c == 110 { return 10; }                             // 'n'  → '\n'
    if c == 114 { return 13; }                             // 'r'  → '\r'
    if c == 116 { return 9;  }                             // 't'  → '\t'
    if c == 92  { return 92; }                             // '\\' → '\\'
    if c == 39  { return 39; }                             // '\'' → '\''
    if c == 34  { return 34; }                             // '"'  → '"'
    if c == 48  { return 0;  }                             // '0'  → '\0'
    if c == 120 {                                           // '\xHH'
        if lx_at_eof(lx) { return -1; }
        let h1: i32 = lx_bump(lx);
        if lx_at_eof(lx) { return -1; }
        let h2: i32 = lx_bump(lx);
        let d1: i32 = digit_value(h1, 16);
        let d2: i32 = digit_value(h2, 16);
        if d1 < 0 || d2 < 0 { return -1; }
        let v: i32 = d1 * 16 + d2;
        if v > 127 { return -1; }
        return v;
    }
    return -1;                                   // workaround stage-0 bug
}

fn scan_char(lx: *mut Lexer, start: usize) {
    lx_bump(lx);                                            // opening '\''

    // Empty: ''
    if lx_peek(lx) == 39 {
        lx_bump(lx);
        emit_lex_error(lx, start, LE_EMPTY_CHAR());
        return;
    }

    let p: i32 = lx_peek(lx);
    if p < 0 || p == 10 {                                   // EOF or newline
        emit_lex_error(lx, start, LE_UNTERMINATED_CHAR());
        return;
    }

    let scalar: i32 = if p == 92 {                          // backslash → escape
        lx_bump(lx);
        let v: i32 = read_escape(lx);
        if v < 0 {
            recover_to_quote_or_nl(lx, 39);
            emit_lex_error(lx, start, LE_BAD_ESCAPE());
            return;
        }
        v
    } else {
        lx_bump(lx)
    };

    if lx_peek(lx) == 39 {
        lx_bump(lx);
        let mut t: Token = token_zero();
        t.kind       = TK_CHAR();
        t.char_val   = scalar as u32;
        t.span_start = start;
        t.span_end   = lx.pos;
        vec_push::<Token>(&mut lx.tokens, t);
    } else {
        recover_to_quote_or_nl(lx, 39);
        emit_lex_error(lx, start, LE_UNTERMINATED_CHAR());
    }
}

fn scan_string(lx: *mut Lexer, start: usize) {
    lx_bump(lx);                                            // opening '"'
    let off: usize = strbuf_len(&lx.pool);

    loop {
        if lx_at_eof(lx) {
            // Drop the partial decoded prefix from the pool.
            // (We've used N bytes of `pool` for the partial decode;
            // truncating keeps the pool tidy.)
            pool_truncate(&mut lx.pool, off);
            emit_lex_error(lx, start, LE_UNTERMINATED_STRING());
            return;
        }
        let c: i32 = lx_peek(lx);
        if c == 10 {                                        // raw newline
            pool_truncate(&mut lx.pool, off);
            emit_lex_error(lx, start, LE_UNTERMINATED_STRING());
            return;
        }
        if c == 34 {                                        // closing '"'
            lx_bump(lx);
            let len: usize = strbuf_len(&lx.pool) - off;
            let mut t: Token = token_zero();
            t.kind       = TK_STR();
            t.str_off    = off;
            t.str_len    = len;
            t.span_start = start;
            t.span_end   = lx.pos;
            vec_push::<Token>(&mut lx.tokens, t);
            return;
        }
        if c == 92 {                                        // '\\'
            let esc_start: usize = lx.pos;
            lx_bump(lx);
            let v: i32 = read_escape(lx);
            if v < 0 {
                // Emit the inline escape error but keep scanning so
                // we don't lose the rest of the string.
                let mut t: Token = token_zero();
                t.kind       = TK_ERROR();
                t.err_kind   = LE_BAD_ESCAPE();
                t.span_start = esc_start;
                t.span_end   = lx.pos;
                vec_push::<Token>(&mut lx.tokens, t);
            } else {
                strbuf_push_byte(&mut lx.pool, v as u8);
            }
        } else {
            // Plain byte (may be multi-byte UTF-8 — pass through).
            let b: i32 = lx_bump(lx);
            strbuf_push_byte(&mut lx.pool, b as u8);
        }
    }
}

// ---------------------------------------------------------------- //
// Operators / punctuation / unexpected                             //
// ---------------------------------------------------------------- //

fn scan_op_or_unexpected(lx: *mut Lexer, start: usize, c: i32) {
    // 3-char ops: '<<=', '...'
    let c2: i32 = lx_peek_at(lx, 1);
    let c3: i32 = lx_peek_at(lx, 2);
    if c == 60 && c2 == 60 && c3 == 61 {                    // '<<='
        lx_bump(lx); lx_bump(lx); lx_bump(lx);
        emit_op(lx, start, TK_SHLEQ()); return;
    }
    if c == 46 && c2 == 46 && c3 == 46 {                    // '...'
        lx_bump(lx); lx_bump(lx); lx_bump(lx);
        emit_op(lx, start, TK_DOTDOTDOT()); return;
    }

    // 2-char ops
    let kind2: u8 = match2(c, c2);
    if kind2 != 255 {
        lx_bump(lx); lx_bump(lx);
        emit_op(lx, start, kind2); return;
    }

    // '>' is special: JointGt vs Gt by next-byte whitespace.
    if c == 62 {
        lx_bump(lx);
        let n: i32 = lx_peek(lx);
        let kind: u8 = if n < 0 || is_whitespace(n) { TK_GT() } else { TK_JOINT_GT() };
        emit_op(lx, start, kind);
        return;
    }

    // 1-char ops / punctuation
    let kind1: u8 = match1(c);
    if kind1 != 255 {
        lx_bump(lx);
        emit_op(lx, start, kind1); return;
    }

    // Unrecognised — emit error and consume one byte to make progress.
    lx_bump(lx);
    let mut t: Token = token_zero();
    t.kind       = TK_ERROR();
    t.err_kind   = LE_UNEXPECTED_CHAR();
    t.err_byte   = c as u8;
    t.span_start = start;
    t.span_end   = lx.pos;
    vec_push::<Token>(&mut lx.tokens, t);
}

fn match2(a: i32, b: i32) -> u8 {
    // Returns 255 if no match.
    if a == 61 && b == 61 { return TK_EQEQ(); }       // ==
    if a == 33 && b == 61 { return TK_NE(); }         // !=
    if a == 60 && b == 61 { return TK_LE(); }         // <=
    if a == 38 && b == 38 { return TK_ANDAND(); }     // &&
    if a == 124 && b == 124 { return TK_OROR(); }     // ||
    if a == 60 && b == 60 { return TK_SHL(); }        // <<
    if a == 45 && b == 62 { return TK_ARROW(); }      // ->
    if a == 58 && b == 58 { return TK_COLONCOLON(); } // ::
    if a == 46 && b == 46 { return TK_DOTDOT(); }     // ..
    if a == 43 && b == 61 { return TK_PLUSEQ(); }     // +=
    if a == 45 && b == 61 { return TK_MINUSEQ(); }    // -=
    if a == 42 && b == 61 { return TK_STAREQ(); }     // *=
    if a == 47 && b == 61 { return TK_SLASHEQ(); }    // /=
    if a == 37 && b == 61 { return TK_PERCENTEQ(); }  // %=
    if a == 38 && b == 61 { return TK_AMPEQ(); }      // &=
    if a == 124 && b == 61 { return TK_PIPEEQ(); }    // |=
    if a == 94 && b == 61 { return TK_CARETEQ(); }    // ^=
    return 255;                                       // workaround stage-0 bug
}

fn match1(a: i32) -> u8 {
    if a == 40  { return TK_LPAREN(); }
    if a == 41  { return TK_RPAREN(); }
    if a == 123 { return TK_LBRACE(); }
    if a == 125 { return TK_RBRACE(); }
    if a == 91  { return TK_LBRACKET(); }
    if a == 93  { return TK_RBRACKET(); }
    if a == 44  { return TK_COMMA(); }
    if a == 59  { return TK_SEMI(); }
    if a == 58  { return TK_COLON(); }
    if a == 46  { return TK_DOT(); }
    if a == 43  { return TK_PLUS(); }
    if a == 45  { return TK_MINUS(); }
    if a == 42  { return TK_STAR(); }
    if a == 47  { return TK_SLASH(); }
    if a == 37  { return TK_PERCENT(); }
    if a == 61  { return TK_EQ(); }
    if a == 60  { return TK_LT(); }
    if a == 33  { return TK_BANG(); }
    if a == 38  { return TK_AMP(); }
    if a == 124 { return TK_PIPE(); }
    if a == 94  { return TK_CARET(); }
    if a == 126 { return TK_TILDE(); }
    return 255;                                       // workaround stage-0 bug
}

fn emit_op(lx: *mut Lexer, start: usize, kind: u8) {
    let mut t: Token = token_zero();
    t.kind       = kind;
    t.span_start = start;
    t.span_end   = lx.pos;
    vec_push::<Token>(&mut lx.tokens, t);
}

fn emit_lex_error(lx: *mut Lexer, start: usize, err_kind: u8) {
    let mut t: Token = token_zero();
    t.kind       = TK_ERROR();
    t.err_kind   = err_kind;
    t.span_start = start;
    t.span_end   = lx.pos;
    vec_push::<Token>(&mut lx.tokens, t);
}

// Skip ahead until `end_byte`, newline, or EOF — used by error
// recovery in char/string scanning.
fn recover_to_quote_or_nl(lx: *mut Lexer, end_byte: i32) {
    while !lx_at_eof(lx) {
        let c: i32 = lx_peek(lx);
        if c == end_byte {
            lx_bump(lx);
            return;
        }
        if c == 10 { return; }
        lx_bump(lx);
    }
}

// Truncate `pool` to `len` bytes. (Vec doesn't expose this; we do
// it by hand via direct len mutation.)
fn pool_truncate(s: *mut StrBuf, len: usize) {
    // SAFETY: shrinking len is sound for u8; no Drop to run.
    s.bytes.len = len;
}
