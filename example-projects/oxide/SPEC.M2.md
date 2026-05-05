# M2 — Lexer

Port `src/lexer/` to Oxide. Stage-1 input is `*const [u8] + usize`;
stage-1 output is a `Vec<Token>` (errors embedded inline) plus a
`StrBuf` "intern pool" that backs identifier and string-literal
payloads. Spans carry byte positions only — LSP UTF-16 column
tracking is deferred.

## What gets ported

Every `TokenKind` variant the host accepts. The full kind list is
mechanical and one-to-one with `src/lexer/token.rs`.

- Literals: `Int(u64)`, `Bool(bool)`, `Char(u32)`, `Str(...)`.
- `Ident(...)`.
- All keywords (incl. reserved-but-unimplemented: `match`, `impl`,
  `trait`, `pub`, `use`, `mod`).
- All punctuation + operators, including the `Gt` / `JointGt`
  whitespace-sensitivity rule from spec/01.
- `Eof`.
- `Error(LexError)`.

LexError variants ported verbatim:

- `UnexpectedChar(u8)` — payload is the offending byte.
- `UnterminatedBlockComment`
- `UnterminatedString`
- `UnterminatedChar`
- `EmptyChar`
- `BadEscape`
- `IntOverflow`
- `InvalidDigit`

## What gets dropped

- **UTF-16 LSP column tracking.** Stage 0's lexer maintains
  `LspPos { line, character }` to support LSP-position-based
  diagnostics; stage 1 emits byte-position spans only and lets the
  diagnostic renderer (a later milestone) reconstruct line/column
  from the source buffer if it ever needs them.
- **Char-level peek.** Host uses Rust's UTF-8-aware `chars().nth(n)`.
  Stage 1 reads bytes; non-ASCII source outside string literals is
  rejected as `UnexpectedChar`. Inside string literals, multi-byte
  UTF-8 sequences pass through untouched (we never interpret them).
- **`String` allocation per token.** Host's `Ident(String)` /
  `Str(String)` allocate a fresh `String` for each token. Stage 1
  uses a single shared `StrBuf` "pool"; tokens carry `(offset,
  length)` into it. Saves N allocations and gives all
  identifier/string payloads cache-adjacent storage.

## Token shape (tagged struct)

Oxide has no enum-with-payload, so `Token` is a flat struct with a
discriminant + every possible payload field. Per-token waste is
~50 bytes; tolerable since one source file produces O(K) tokens at
compile time.

```rust
struct Token {
    kind: u8,             // TK_* constant — see below
    int_val:    u64,      // Int
    bool_val:   bool,     // Bool
    char_val:   u32,      // Char (Unicode scalar, fits in u32)
    str_off:    usize,    // Ident/Str: offset into pool
    str_len:    usize,    // Ident/Str: byte length in pool
    err_kind:   u8,       // Error (LE_* constant)
    err_byte:   u8,       // Error: associated byte for UnexpectedChar
    span_start: usize,    // byte offset in source
    span_end:   usize,    // byte offset in source (exclusive)
}
```

`Char` payload is `u32` (Unicode scalar value). Stage-0 stores
`char` (also u32-shaped). Source-level character literals are ASCII-
only in v0; the wider type lets `\x` escapes that produce full
ASCII (0..=127) flow through cleanly.

For `Ident` / `Str` tokens, `str_off` and `str_len` index into the
`Lexer.pool: StrBuf`. `Ident` payloads are the verbatim source
slice; `Str` payloads are the **decoded** byte sequence (after
escape processing).

## Token-kind tags

```rust
fn TK_EOF()      -> u8 {  0 }
fn TK_INT()      -> u8 {  1 }
fn TK_BOOL()     -> u8 {  2 }
fn TK_CHAR()     -> u8 {  3 }
fn TK_STR()      -> u8 {  4 }
fn TK_IDENT()    -> u8 {  5 }
fn TK_ERROR()    -> u8 {  6 }

// Keywords (10..=39 reserved range)
fn TK_KW_FN()       -> u8 { 10 }
fn TK_KW_LET()      -> u8 { 11 }
fn TK_KW_MUT()      -> u8 { 12 }
... // see lexer.ox for the full table
```

Discriminant ranges:
- `0..=9`     reserved for non-keyword variants (literals, ident,
              eof, error).
- `10..=39`   keywords.
- `40..=99`   punctuation + operators.

The Oxide source authoritative table is in `lexer.ox`; this spec
describes the shape, not the numbers.

## LexError tags

```rust
fn LE_UNEXPECTED_CHAR()         -> u8 { 0 }
fn LE_UNTERMINATED_BLK_COMMENT() -> u8 { 1 }
fn LE_UNTERMINATED_STRING()     -> u8 { 2 }
fn LE_UNTERMINATED_CHAR()       -> u8 { 3 }
fn LE_EMPTY_CHAR()              -> u8 { 4 }
fn LE_BAD_ESCAPE()              -> u8 { 5 }
fn LE_INT_OVERFLOW()            -> u8 { 6 }
fn LE_INVALID_DIGIT()           -> u8 { 7 }
```

## Module layout

```text
example-projects/oxide/lexer.ox    ← Token struct, tag constants,
                                      lex() entry point, scan_*
                                      helpers
example-projects/oxide/tests/m2_lexer.ox
                                   ← snapshot test
```

Public surface:

```rust
struct Lexer {
    src:     *const [u8],
    src_len: usize,
    pos:     usize,
    pool:    StrBuf,         // owned
    tokens:  Vec<Token>,     // owned
}

fn lex(src: *const [u8], src_len: usize) -> Lexer;
```

`lex` takes ownership of the source view (the caller keeps the
backing buffer alive for the lexer's lifetime), runs the full scan,
and returns the populated `Lexer` with `tokens` and `pool`.

The `Lexer` is consumed downstream — parser owns it next, walks
`tokens[i]`, and reads ident/string payloads via `pool` slicing.

## Subset of Rust constraint

The lexer doesn't have a Rust-syntax surface — it consumes Rust-
shaped source but the lexer itself is a port. The constraint that
matters is: **stage 1's lexer produces the same tokens stage 0's
lexer does, modulo UTF-16 LSP positions.**

We test this by lexing the same corpus through both lexers (in M2
acceptance below) and comparing `(kind, span_start, span_end,
payload)` per token.

## Acceptance

1. **Round-trip on a fixed corpus.** Stage-1 lexer applied to a
   curated set of Oxide programs (covering every TokenKind) produces
   tokens whose `(kind, start, end, payload)` exactly match the
   stage-0 lexer's. We export stage 0's tokens via `--emit lex` and
   compare.
2. **Self-lex.** Stage-1 lexer applied to its own source
   (`example-projects/oxide/lexer.ox`) produces zero `TK_ERROR`
   tokens.
3. **Snapshot test.** `tests/m2_lexer.ox` exercises every kind on a
   small inline source and prints `kind:span:payload` per token. The
   `.snap` file captures the printout.

## Edge cases worth listing

- **`>` joint vs split.** Stage 1 must reproduce the rule: `>`
  followed by whitespace/EOF emits `Gt`; otherwise emits `JointGt`.
  Without this the parser cannot disambiguate `Foo<Bar<T>>` from
  `1 > > 2`.
- **`>=` doesn't exist as a token.** Per spec/01, `>` is always one
  byte. The parser recombines `JointGt + Eq` for `>=`,
  `JointGt + Gt` for `>>`, etc.
- **Nested block comments.** `/* outer /* inner */ still outer */`
  must close cleanly. Tracked via depth counter.
- **`\x` escapes.** Two hex digits, must yield ASCII (0..=127).
  Higher-bit escapes are `BadEscape` even though the wider
  source-byte path supports raw UTF-8 in strings.
- **Empty `''`.** Distinct error from `'\\'` (unterminated). The
  empty form gets `EmptyChar`.
- **Multi-line strings.** `"foo\nbar"` (literal newline inside
  source) is `UnterminatedString`. Use `\n` escape.
- **3-char ops.** Only `<<=` and `...`. Everything else 2-char
  (`==`, `!=`, `<=`, `&&`, `||`, `<<`, `->`, `::`, `..`, `+=`,
  `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`).

## Out of scope

- LSP UTF-16 columns.
- Non-ASCII identifier characters.
- Raw string literals `r"..."` (host doesn't support them).
- Numeric suffixes (`1u64`, `2.0f32`). Host doesn't support; stage
  1 won't either.
- Float literals — no float tokens ever appear in stage-1 input
  (stage-1 source uses none).
