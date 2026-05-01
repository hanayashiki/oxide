# B003 — Parser error structure: `ParseError::Custom` overloads E0107

## Status

Open — error-UX wart, not a soundness issue. Discovered while
specifying `09_ARRAY.md`'s empty-`[]` rejection. Independent of any
ongoing feature work; pick up as part of a broader error-UX pass.

## The smell

Today's parser-error pipeline:

```
combinator emit → chumsky::Rich → rich_to_parse_error() → ParseError → Diagnostic
```

`ParseError::Custom { message: String, span }` is a **bucket
variant** for any structured rejection a combinator emits via
`Rich::custom(span, "message")`. The reporter
(`src/reporter/from_parse.rs:36-37`) hard-codes a single diagnostic
code for every `Custom`:

```rust
ParseError::Custom { message, span } => {
    Diagnostic::error("E0107", message.clone())
}
```

So **E0107 is the catch-all "structured parser-level rejection"
code**, not a specific error. Three semantically-distinct sites
all share it:

| Site | Custom message |
|---|---|
| `syntax.rs:454` | `"bodyless `fn {name}` must be inside an `extern \"C\" { ... }` block"` |
| `syntax.rs:476`/`:480` | `"only \"C\" ABI is supported, got \"{s}\""` / `"expected ABI string \"C\", got {tok:?}"` |
| `syntax.rs:489` | `"extern \"C\" fn `{name}` must not have a body"` |

Three problems:

1. **Diagnostic-code aliasing.** Four independent error categories
   share a single code. A user filing a bug "I hit E0107" gives no
   useful signal about which rule fired.
2. **Message style drift.** Each combinator hand-writes its English
   message. Phrasing, capitalization, hint shape diverge across
   sites — exactly what a centralized reporter is supposed to
   prevent.
3. **Variant identity erased into a String.** The structural
   information about *which kind* of error happened is lost the
   moment the message is generated; downstream consumers
   (snapshot tests, future IDE integration) can only match on the
   message text.

## Why it's structured this way

Chumsky's combinator API for emitting errors during validation
takes a `Rich` payload. There is no first-class side-channel for
"please record a structured error of variant X" from inside a
parser. The only way to get a typed payload through is via
`Rich::custom(span, msg)`, where `msg` is a `String`. Our
`rich_to_parse_error` adapter then creates `ParseError::Custom`
because there's nothing else it can do with an opaque
custom-string Rich.

So the smell is partly chumsky's API shape and partly our adapter
swallowing the smell rather than working around it.

## Refactor sketch — tag-via-Rich

The trick: encode a discriminator in the custom message string,
parse it back out in the adapter, and dispatch to a typed variant.
Conceptually like passing a stringly-typed enum:

```rust
// New: a typed enum for combinator-emitted semantic rejections.
pub enum ParseSemError {
    BodyOutsideExtern,
    NonCAbiString { got: String },
    ExternFnWithBody,
    StructLitInCondPos,
    // ...
}

impl ParseSemError {
    fn tag(&self) -> &'static str {
        // unique, parse-friendly identifier per variant
        match self {
            Self::BodyOutsideExtern   => "body-outside-extern",
            Self::NonCAbiString { .. } => "non-c-abi",
            Self::ExternFnWithBody    => "extern-fn-with-body",
            Self::StructLitInCondPos  => "struct-lit-in-cond",
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::BodyOutsideExtern   => "E0108",
            Self::NonCAbiString { .. } => "E0109",
            Self::ExternFnWithBody    => "E0110",
            Self::StructLitInCondPos  => "E0111",
        }
    }

    fn message(&self) -> String {
        match self {
            Self::BodyOutsideExtern => "bodyless `fn` must be inside an `extern \"C\" { ... }` block".into(),
            Self::NonCAbiString { got } => format!("only \"C\" ABI is supported, got \"{got}\""),
            // ...
        }
    }

    fn from_tag(tag: &str, _payload: &str) -> Option<Self> { /* parse */ }
}

// New variant on ParseError:
pub enum ParseError {
    UnexpectedToken { ... },
    UnexpectedEof { ... },
    BadStatement { span: Span },
    Semantic(ParseSemError, Span),                // <-- new
    Custom { message: String, span: Span },       // <-- legacy fallback
    ReservedKeyword { ... },
    LexErrorToken { ... },
}

// Combinator emit (callsite):
emitter.emit(Rich::custom(
    span,
    serialize(ParseSemError::StructLitInCondPos),
));

// Adapter (rich.rs):
RichReason::Custom(msg) => match deserialize::<ParseSemError>(msg) {
    Some(sem) => ParseError::Semantic(sem, span),
    None => ParseError::Custom { message: msg.clone(), span },  // graceful fallback
},

// Reporter (from_parse.rs):
ParseError::Semantic(sem, span) => Diagnostic::error(sem.code(), sem.message()),
ParseError::Custom { message, span } => Diagnostic::error("E0107", message.clone()),
```

The encoding is just a wire format inside the `String` payload —
e.g., `"#[oxide:non-c-abi]got=\"D\""` — chosen so it's unambiguous,
escapable, and hard for any user-emitted message to collide with.
The serialize/deserialize pair handles the round-trip. Variants
with payloads (like `NonCAbiString { got }`) carry their data
through the wire format.

After the refactor:

- **Each rule gets its own code.** No more E0107 aliasing.
- **One source of message text per variant.** Centralized in
  `ParseSemError::message`.
- **Snapshot tests match on variant.** The string parsing only
  happens in the adapter; tests see structured `ParseError::Semantic`.

The legacy `Custom` variant stays as a graceful fallback, so
ad-hoc `Rich::custom(span, "...")` calls in tests or experimental
code still work via the existing path.

## Alternative: side-channel emit

Combinators take `&mut ModuleBuilder` (already do — see
`builder.errors`). They could push directly to that without
routing through Rich. Two reasons we don't:

- The `validate` combinator runs *during* chumsky's parse loop and
  doesn't have ergonomic access to the builder; the closure
  signature only sees the in-flight value and the emitter.
- A side-channel would lose the natural ordering of errors as they
  appear in the source. Going through Rich keeps them
  span-ordered alongside chumsky's own errors.

Leaving Rich in the loop and stuffing typed payloads through it
(the tag-via-Rich approach above) keeps both wins.

## Effort

Mechanical: ~150 LOC for the new enum + serialize/deserialize +
adapter dispatch + reporter arms + a few test snapshots updated.
Touches:

- `src/parser/error.rs` — new variant.
- `src/parser/parse/rich.rs` — adapter dispatch.
- `src/parser/parse/syntax.rs` — change three callsites.
- `src/reporter/from_parse.rs` — new arms per variant.
- All affected snapshot tests in `tests/snapshots/parser/`.

## When to do this

Not for the array spec. The spec adds one more E0107 caller; that
exacerbates the smell but doesn't block functionality. Land
inside a deliberate "parser-error UX" pass — analogous to B002 for
typeck — when several pain points are batched together.
