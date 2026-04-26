use super::diagnostic::{Diagnostic, Label};
use super::source_map::FileId;
use crate::hir::HirError;

/// Map an HIR error into a structured diagnostic. HIR's errors are all
/// value-namespace resolution failures or lowering mishaps; type-name
/// errors live in typeck.
pub fn from_hir_error(err: &HirError, file: FileId) -> Diagnostic {
    match err {
        HirError::UnresolvedName { name, span } => {
            Diagnostic::error("E0201", format!("cannot find `{name}` in scope"))
                .with_label(Label::primary(file, span.clone(), "not found"))
        }
        HirError::DuplicateFn { name, first, dup } => {
            Diagnostic::error("E0202", format!("the function `{name}` is defined multiple times"))
                .with_label(Label::primary(file, dup.clone(), "duplicate definition"))
                .with_label(Label::secondary(file, first.clone(), "first defined here"))
        }
        HirError::CharOutOfRange { ch, span } => Diagnostic::error(
            "E0203",
            format!("char literal `{ch:?}` does not fit in a byte (`u8`)"),
        )
        .with_label(Label::primary(file, span.clone(), "value exceeds 0xFF"))
        .with_help("v0 char literals are bytes; multibyte characters aren't supported"),
        HirError::DuplicateAdt { name, first, dup } => Diagnostic::error(
            "E0204",
            format!("the type `{name}` is defined multiple times"),
        )
        .with_label(Label::primary(file, dup.clone(), "duplicate definition"))
        .with_label(Label::secondary(file, first.clone(), "first defined here")),
        HirError::DuplicateField {
            adt,
            name,
            first,
            dup,
        } => Diagnostic::error(
            "E0205",
            format!("field `{name}` is declared multiple times in `{adt}`"),
        )
        .with_label(Label::primary(file, dup.clone(), "duplicate field"))
        .with_label(Label::secondary(file, first.clone(), "first declared here")),
        HirError::UnresolvedAdt { name, span } => Diagnostic::error(
            "E0206",
            format!("cannot find type `{name}` in this scope"),
        )
        .with_label(Label::primary(file, span.clone(), "no struct with this name")),
        HirError::InvalidAssignTarget { span } => Diagnostic::error(
            "E0207",
            "left-hand side of assignment is not a place expression",
        )
        .with_label(Label::primary(file, span.clone(), "cannot assign to this"))
        .with_help(
            "assignment targets must be a local, a field of a place, or a deref \
             of a pointer; literals, calls, and struct literals produce values \
             without a stable location",
        ),
    }
}
