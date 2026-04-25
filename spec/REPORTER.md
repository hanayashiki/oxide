# Reporter Spec

The reporter renders compiler diagnostics — lex errors, parse errors, type
errors, etc. — to a terminal with source snippets, underlines, and colors.

## Crate choice

Use **`ariadne`** (https://crates.io/crates/ariadne).

- Multi-label diagnostics (point at several spans in one report).
- Multi-file reports (cross-file errors render correctly).
- Colored, Unicode underlines, no derive macros, small API surface.
- Same-author pairing with `chumsky` if we go that route for the parser.

Alternatives considered: `miette` (heavier, derive-based, fine but more
machinery than we need); `codespan-reporting` (older, less pretty);
`annotate-snippets` (what rustc uses, but lower-level).

## Diagnostic model

Compiler stages do **not** call `ariadne` directly. They emit a structured
`Diagnostic` value. The reporter is the only place that knows about
`ariadne`. This keeps stages testable and lets us later emit LSP
`Diagnostic` JSON from the same source of truth.

```rust
pub enum Severity { Error, Warning, Note, Help }

pub struct Label {
    pub span: Span,        // from spec/LEXER.md — has byte + LSP positions
    pub message: String,   // shown next to the underline; "" hides it
    pub primary: bool,     // exactly one primary label per diagnostic
}

pub struct Diagnostic {
    pub severity: Severity,
    pub code: Option<&'static str>,   // e.g. "E0001"; stable, greppable
    pub message: String,              // one-line headline
    pub labels: Vec<Label>,           // first primary label drives the location
    pub notes: Vec<String>,           // free-form trailing lines ("note: ...")
    pub helps: Vec<String>,           // suggestions ("help: try ...")
}
```

Conventions:

- `message` is sentence-case, no trailing period, < ~70 chars.
- `code` is optional but encouraged for errors users might search for.
- `labels[0]` (or the first `primary: true`) is what an editor jumps to.
- `notes` and `helps` render after the snippet.

## Source manager

`ariadne` needs a way to look up source text by file id. We give it one:

```rust
pub struct FileId(pub u32);

pub struct SourceMap {
    files: Vec<SourceFile>,
}

pub struct SourceFile {
    pub id: FileId,
    pub path: PathBuf,
    pub text: String,
}

impl SourceMap {
    pub fn add(&mut self, path: PathBuf, text: String) -> FileId;
    pub fn get(&self, id: FileId) -> &SourceFile;
}
```

`Span` gains a `file: FileId` field so diagnostics from any stage know
which file they belong to. (Update `spec/LEXER.md` accordingly when this
lands.)

## Rendering API

```rust
pub fn emit(diag: &Diagnostic, sources: &SourceMap, out: &mut dyn Write);
pub fn emit_all(diags: &[Diagnostic], sources: &SourceMap, out: &mut dyn Write);
```

`emit` translates `Diagnostic` → `ariadne::Report` and writes to `out`
(stderr in normal CLI use). Color is auto-detected via `ariadne`'s default
(TTY-aware); a `--color=always|never|auto` flag is wired in later.

## Mapping LexError → Diagnostic

Each `LexError` variant gets a fixed code, message, and (where useful) a
help line. Examples:

| LexError                    | code  | message                          | help |
|-----------------------------|-------|----------------------------------|------|
| `UnexpectedChar(c)`         | E0001 | `unexpected character '{c}'`     | —    |
| `UnterminatedBlockComment`  | E0002 | `unterminated block comment`     | `block comments nest; check for an unmatched /*` |
| `UnterminatedString`        | E0003 | `unterminated string literal`    | —    |
| `UnterminatedChar`          | E0004 | `unterminated char literal`      | —    |
| `EmptyChar`                 | E0005 | `empty char literal`             | `use '\\0' for the null byte` |
| `BadEscape`                 | E0006 | `invalid escape sequence`        | list valid escapes |
| `IntOverflow`               | E0007 | `integer literal overflows u64`  | —    |
| `InvalidDigit`              | E0008 | `invalid digit for numeric base` | —    |

The translation lives in `reporter::from_lex_error` so the lexer itself
stays free of presentation concerns.

## Example output

For input `let x = 'ab';`:

```
error[E0006]: invalid escape sequence
  ╭─[main.ox:1:9]
  │
1 │ let x = 'ab';
  │         ─┬─
  │          ╰── char literal must contain exactly one character
──╯
```

(Exact glyphs come from `ariadne`; this is illustrative.)

## Testing

Diagnostics are tested at the structured layer — assert on `Diagnostic`
fields, not rendered strings. A small set of golden snapshot tests cover
the rendering itself so we notice if `ariadne` output drifts.

## Out of scope (v0)

- JSON output mode (`--error-format=json`).
- LSP `Diagnostic` conversion (will live in a future `lsp` crate, reusing
  the same `Diagnostic` type).
- Fix-it / auto-applicable suggestions (`Help` is plain text for now).
- Diagnostic deduplication and ordering policies.
