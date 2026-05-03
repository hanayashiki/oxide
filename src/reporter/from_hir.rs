use super::diagnostic::{Diagnostic, Label};
use crate::hir::HirError;

/// Map an HIR error into a structured diagnostic. HIR's errors are all
/// value-namespace resolution failures or lowering mishaps; type-name
/// errors live in typeck.
///
/// Each label takes its `FileId` from its own span, so cross-file errors
/// (e.g. `DuplicateGlobalSymbol`, where the `first` and `dup` definitions
/// can live in different files) attribute each label to the correct file.
pub fn from_hir_error(err: &HirError) -> Diagnostic {
    match err {
        HirError::UnresolvedName { name, span } => {
            Diagnostic::error("E0201", format!("cannot find `{name}` in scope"))
                .with_label(Label::primary(span.file, span.clone(), "not found"))
        }
        HirError::DuplicateFn { name, first, dup } => {
            Diagnostic::error("E0202", format!("the function `{name}` is defined multiple times"))
                .with_label(Label::primary(dup.file, dup.clone(), "duplicate definition"))
                .with_label(Label::secondary(first.file, first.clone(), "first defined here"))
        }
        HirError::CharOutOfRange { ch, span } => Diagnostic::error(
            "E0203",
            format!("char literal `{ch:?}` does not fit in a byte (`u8`)"),
        )
        .with_label(Label::primary(span.file, span.clone(), "value exceeds 0xFF"))
        .with_help("v0 char literals are bytes; multibyte characters aren't supported"),
        HirError::DuplicateAdt { name, first, dup } => Diagnostic::error(
            "E0204",
            format!("the type `{name}` is defined multiple times"),
        )
        .with_label(Label::primary(dup.file, dup.clone(), "duplicate definition"))
        .with_label(Label::secondary(first.file, first.clone(), "first defined here")),
        HirError::DuplicateGlobalSymbol {
            name,
            first,
            dup,
            root: _,
        } => Diagnostic::error(
            "E0272",
            format!("the symbol `{name}` is defined more than once across imported files"),
        )
        .with_label(Label::primary(dup.file, dup.clone(), "duplicate definition"))
        .with_label(Label::secondary(first.file, first.clone(), "first defined here")),
        HirError::DuplicateField {
            adt,
            name,
            first,
            dup,
        } => Diagnostic::error(
            "E0205",
            format!("field `{name}` is declared multiple times in `{adt}`"),
        )
        .with_label(Label::primary(dup.file, dup.clone(), "duplicate field"))
        .with_label(Label::secondary(first.file, first.clone(), "first declared here")),
        HirError::UnresolvedAdt { name, span } => Diagnostic::error(
            "E0206",
            format!("cannot find type `{name}` in this scope"),
        )
        .with_label(Label::primary(span.file, span.clone(), "no struct with this name")),
        HirError::InvalidAssignTarget { span } => Diagnostic::error(
            "E0207",
            "left-hand side of assignment is not a place expression",
        )
        .with_label(Label::primary(span.file, span.clone(), "cannot assign to this"))
        .with_help(
            "assignment targets must be a local, a field of a place, or a deref \
             of a pointer; literals, calls, and struct literals produce values \
             without a stable location",
        ),
        HirError::AddrOfNonPlace { span } => Diagnostic::error(
            "E0208",
            "cannot take the address of a non-place expression",
        )
        .with_label(Label::primary(span.file, span.clone(), "not addressable"))
        .with_help(
            "only places — locals, fields of places, and pointer derefs — \
             have a stable memory location to take the address of",
        ),
        HirError::BreakOutsideLoop { span } => {
            Diagnostic::error("E0263", "`break` outside of a loop")
                .with_label(Label::primary(span.file, span.clone(), "cannot `break` here"))
                .with_help("`break` is only valid inside `while`, `loop`, or `for`")
        }
        HirError::ContinueOutsideLoop { span } => {
            Diagnostic::error("E0264", "`continue` outside of a loop")
                .with_label(Label::primary(span.file, span.clone(), "cannot `continue` here"))
                .with_help("`continue` is only valid inside `while`, `loop`, or `for`")
        }
        HirError::BodylessFnOutsideExtern { name, span } => Diagnostic::error(
            "E0209",
            format!("bodyless `fn {name}` must be inside an `extern \"C\" {{ ... }}` block"),
        )
        .with_label(Label::primary(span.file, span.clone(), "missing function body"))
        .with_help(
            "module-level fns require `{ ... }`; `;` is reserved for foreign \
             declarations inside `extern \"C\"`",
        ),
        HirError::ExternFnHasBody { name, span } => Diagnostic::error(
            "E0210",
            format!("extern \"C\" fn `{name}` must not have a body"),
        )
        .with_label(Label::primary(span.file, span.clone(), "remove the body"))
        .with_help(
            "items inside `extern \"C\" {{ ... }}` are foreign declarations; \
             they end with `;` and the implementation is provided by the linker",
        ),
        HirError::UnsupportedExternItem { kind, span } => Diagnostic::error(
            "E0211",
            format!("`{kind}` items are not supported inside `extern \"C\"` blocks"),
        )
        .with_label(Label::primary(span.file, span.clone(), "not allowed here"))
        .with_help("v0 only accepts foreign `fn` declarations inside extern blocks"),
    }
}
