# Lexer Spec

The lexer turns a source string into a flat stream of tokens with source spans.
It does no semantic work: keywords are recognized lexically, numbers are not
range-checked, escape sequences in strings are validated but not interpreted
into runtime values.

## Goals

- Hand-written, no lexer-generator dependency.
- Single forward pass over `&str`, no backtracking.
- Each token carries a byte-offset `Span` for diagnostics.
- Errors are recoverable: an invalid character produces an `Error` token and
  lexing continues.

## Token

Every token carries both a byte-offset span (for slicing the source) and an
LSP-style line/column range (for diagnostics and editor integration). They
are stored directly, not computed lazily.

```rust
pub struct BytePos { pub offset: usize }            // byte offset into source

pub struct LspPos {                                 // 0-indexed, LSP-compatible
    pub line: u32,
    pub character: u32,                             // UTF-16 code units, per LSP spec
}

pub struct Span {
    pub start: BytePos,
    pub end:   BytePos,                             // half-open
    pub lsp_start: LspPos,
    pub lsp_end:   LspPos,
}

pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}
```

`character` counts UTF-16 code units to match the LSP default encoding. ASCII
source is the common case and counts identically to bytes; non-ASCII (e.g. an
emoji in a string literal) advances `character` by 1 or 2 depending on the
code point. The lexer tracks this incrementally as it walks the source.

### TokenKind

Grouped here for clarity; in code it is one flat enum.

**Literals**
- `Int(u64)`            — decimal `123`, hex `0xFF`, binary `0b1010`, with optional `_` separators
- `Bool(bool)`          — `true` / `false` (lexed as keywords, surfaced as bool literal)
- `Char(char)`          — `'a'`, `'\n'`, `'\x7F'`, `'\\'`, `'\''`
- `Str(String)`         — `"hello\n"`; escapes validated, value stored decoded

**Identifier**
- `Ident(String)`       — `[A-Za-z_][A-Za-z0-9_]*` that is not a keyword

**Keywords** (each its own variant: `KwFn`, `KwLet`, ...)
- `fn let mut if else while for return break continue`
- `struct enum as`
- `true false`
- `null`                — null pointer literal
- `sizeof`              — operator-keyword
- Reserved (lexed as keyword, parser rejects until used): `match impl trait pub use mod`

**Primitive type names** are *not* keywords; they are normal identifiers
resolved by the parser/typeck (`i8 i16 i32 i64 u8 u16 u32 u64 bool void`).
This keeps the keyword set small and lets us add types without touching the lexer.

**Punctuation**
- `( ) { } [ ]`
- `, ; :  ::  ->  .  ..`

**Operators** (longest-match wins; e.g. `==` beats `=`)
- Arithmetic: `+ - * / %`
- Comparison: `== != < <= > >=`
- Logical:    `&& || !`
- Bitwise:    `& | ^ ~ << >>`
- Assignment: `= += -= *= /= %= &= |= ^= <<= >>=`
- Address-of / deref share `&` and `*` with bitwise/arith; disambiguation is the parser's job.

**Trivia & control**
- `Eof`
- `Error(LexError)` — see below

Whitespace and comments are *consumed*, not emitted as tokens.

## Lexical rules

### Whitespace
` ` `\t` `\r` `\n` are skipped. Newlines are not significant; statements end with `;`.

### Comments
- `// ...` to end of line.
- `/* ... */` block comments, **nestable** (Rust-style).
- Comments are skipped, not emitted. An unterminated block comment is a
  `LexError::UnterminatedBlockComment` at the opening `/*`.

### Identifiers & keywords
- Pattern: `[A-Za-z_][A-Za-z0-9_]*`.
- A bare `_` is a valid identifier (parser may give it special meaning later).
- After matching, look up in the keyword table; if hit, emit the keyword
  variant, else `Ident`.

### Integer literals
- Decimal: `[0-9][0-9_]*`
- Hex:     `0x[0-9A-Fa-f_]+`
- Binary:  `0b[01_]+`
- Underscores are stripped before parsing; leading/trailing/double underscores are allowed inside the digit run but the literal must contain at least one digit.
- No suffix syntax in v0 (`123i32` is deferred). Type comes from context.
- Value parsed into `u64`; overflow is `LexError::IntOverflow`.

### Float literals
- **Out of scope for v0.** Reserved syntactically (a `.` after digits will be a parser error for now). Revisit when we add `f32`/`f64`.

### Char literals
- `'<one char or escape>'`
- Escapes: `\n \r \t \\ \' \" \0 \xHH` (two hex digits, byte value, must be ASCII).
- No unicode `\u{...}` escapes in v0.
- Errors: `EmptyChar`, `UnterminatedChar`, `BadEscape`.

### String literals
- `"..."`, same escape set as char literals plus `\"`.
- No raw strings, no multi-line strings in v0.
- Errors: `UnterminatedString`, `BadEscape`.

### Operator/punctuation lexing
Maximal munch: at each position, try the longest operator that matches.
Concretely the table is sorted by length descending; first match wins.

## Errors

```rust
pub enum LexError {
    UnexpectedChar(char),
    UnterminatedBlockComment,
    UnterminatedString,
    UnterminatedChar,
    EmptyChar,
    BadEscape,
    IntOverflow,
    InvalidDigit,            // e.g. '8' inside a 0b literal
}
```

Strategy: emit `Token { kind: Error(e), span }` and resume at the next byte
(or next reasonable boundary, e.g. after the closing quote for string errors
if found). The parser treats `Error` tokens as poison and reports them once.

## API

```rust
pub fn lex(src: &str) -> Vec<Token>;        // always ends with Eof
```

A streaming `Lexer` iterator may come later; for v0 we materialize the vec
because the parser wants random access for lookahead.

## Out of scope (v0)

- Float literals
- String/char unicode escapes
- Raw strings, byte strings, byte literals
- Suffixed numeric literals (`123u32`)
- Shebang line
- Doc comments distinguished from regular comments
