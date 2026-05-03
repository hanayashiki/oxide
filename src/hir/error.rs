use crate::reporter::{FileId, Span};

#[derive(Clone, Debug)]
pub enum HirError {
    /// Value-namespace lookup failed.
    UnresolvedName { name: String, span: Span },
    /// Two `fn`s in one module share a name (per module, checking both imported and local items).
    DuplicateFn {
        name: String,
        first: Span,
        dup: Span,
    },
    /// Given a build root, more than two items share the same name.
    DuplicateGlobalSymbol {
        name: String,
        first: Span,
        dup: Span,
        root: FileId,
    },
    /// `'\xHH'` whose value exceeds `u8::MAX`, or a multibyte char literal.
    CharOutOfRange { ch: char, span: Span },
    /// Two ADTs (struct/enum/union) in one module share a name.
    DuplicateAdt {
        name: String,
        first: Span,
        dup: Span,
    },
    /// Two fields in one ADT share a name.
    DuplicateField {
        adt: String,
        name: String,
        first: Span,
        dup: Span,
    },
    /// Type-namespace lookup failed in a struct-literal position.
    UnresolvedAdt { name: String, span: Span },
    /// Left-hand side of `=` (or compound assign) is not a place
    /// expression. See spec/08_ADT.md "Place expressions and `is_place`".
    InvalidAssignTarget { span: Span },
    /// Operand of `&` / `&mut` is not a place expression. See
    /// spec/10_ADDRESS_OF.md "Place rule". Span points at the operand.
    AddrOfNonPlace { span: Span },
    /// `break` outside any enclosing loop. Span points at the `break`
    /// keyword's expression. See spec/13_LOOPS.md.
    BreakOutsideLoop { span: Span },
    /// `continue` outside any enclosing loop. Span points at the
    /// `continue` keyword's expression. See spec/13_LOOPS.md.
    ContinueOutsideLoop { span: Span },
    /// `fn name(...);` at module scope (no body). v0 only allows
    /// bodyless fns inside `extern "C" { ... }`.
    BodylessFnOutsideExtern { name: String, span: Span },
    /// `fn name(...) { body }` inside an `extern "C"` block. Foreign
    /// fn declarations must be bodyless.
    ExternFnHasBody { name: String, span: Span },
    /// Non-fn item inside an `extern "C"` block (struct, nested
    /// extern block, import). v0 only accepts foreign `fn`
    /// declarations there. `kind` is the item's source-form label
    /// (`"struct"`, `"extern block"`, `"import"`).
    UnsupportedExternItem { kind: String, span: Span },
}

impl HirError {
    pub fn span(&self) -> &Span {
        match self {
            Self::UnresolvedName { span, .. }
            | Self::CharOutOfRange { span, .. }
            | Self::UnresolvedAdt { span, .. }
            | Self::InvalidAssignTarget { span }
            | Self::AddrOfNonPlace { span }
            | Self::BreakOutsideLoop { span }
            | Self::ContinueOutsideLoop { span }
            | Self::BodylessFnOutsideExtern { span, .. }
            | Self::ExternFnHasBody { span, .. }
            | Self::UnsupportedExternItem { span, .. } => span,
            Self::DuplicateFn { dup, .. }
            | Self::DuplicateAdt { dup, .. }
            | Self::DuplicateField { dup, .. }
            | Self::DuplicateGlobalSymbol { dup, .. } => dup,
        }
    }
}
