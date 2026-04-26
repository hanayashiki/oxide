use super::diagnostic::{Diagnostic, Label};
use super::source_map::FileId;
use crate::typeck::{TyArena, TypeError};

/// Map a typeck error to a structured diagnostic. Needs the `TyArena` to
/// render type names (`expected: i32, found: bool`).
pub fn from_typeck_error(err: &TypeError, file: FileId, tys: &TyArena) -> Diagnostic {
    match err {
        TypeError::TypeMismatch { expected, found, span } => Diagnostic::error(
            "E0250",
            format!(
                "type mismatch: expected `{}`, found `{}`",
                tys.render(*expected),
                tys.render(*found)
            ),
        )
        .with_label(Label::primary(file, span.clone(), "type mismatch here")),

        TypeError::UnknownType { name, span } => {
            Diagnostic::error("E0251", format!("unknown type `{name}`"))
                .with_label(Label::primary(file, span.clone(), "not a known type"))
                .with_help("v0 supports only primitive types: i8..i64, u8..u64, bool, void, never")
        }

        TypeError::NotCallable { found, span } => Diagnostic::error(
            "E0252",
            format!(
                "expression of type `{}` is not callable",
                tys.render(*found)
            ),
        )
        .with_label(Label::primary(file, span.clone(), "not a function")),

        TypeError::WrongArgCount { expected, found, span } => Diagnostic::error(
            "E0253",
            format!("wrong number of arguments: expected {expected}, found {found}"),
        )
        .with_label(Label::primary(file, span.clone(), "called here")),

        TypeError::UnsupportedFeature { feature, span } => {
            Diagnostic::error("E0255", format!("unsupported in v0: {feature}"))
                .with_label(Label::primary(file, span.clone(), "not yet supported"))
        }

        TypeError::CannotInfer { span } => Diagnostic::error("E0256", "could not infer a type")
            .with_label(Label::primary(file, span.clone(), "ambiguous type"))
            .with_help("add a type annotation to disambiguate"),

        TypeError::PointerMutabilityMismatch { expected, actual, span } => Diagnostic::error(
            "E0257",
            format!(
                "pointer mutability mismatch: expected `{}`, found `{}`",
                tys.render(*expected),
                tys.render(*actual)
            ),
        )
        .with_label(Label::primary(file, span.clone(), "incompatible pointer"))
        .with_help(
            "only `*mut T` → `*const T` is allowed at the outer layer; \
             inner positions must match exactly",
        ),

        TypeError::StructLitUnknownField { field, adt, span } => Diagnostic::error(
            "E0258",
            format!("struct `{adt}` has no field `{field}`"),
        )
        .with_label(Label::primary(file, span.clone(), "unknown field")),

        TypeError::StructLitMissingField { field, adt, lit_span } => Diagnostic::error(
            "E0259",
            format!("missing field `{field}` in struct literal of `{adt}`"),
        )
        .with_label(Label::primary(file, lit_span.clone(), "field not provided")),

        TypeError::StructLitDuplicateField { field, first, dup } => Diagnostic::error(
            "E0260",
            format!("field `{field}` specified more than once"),
        )
        .with_label(Label::primary(file, dup.clone(), "duplicate"))
        .with_label(Label::secondary(file, first.clone(), "first specified here")),

        TypeError::NoFieldOnAdt { field, adt, span } => Diagnostic::error(
            "E0261",
            format!("no field `{field}` on type `{adt}`"),
        )
        .with_label(Label::primary(file, span.clone(), "unknown field")),

        TypeError::TypeNotFieldable { ty, span } => Diagnostic::error(
            "E0262",
            format!("type `{}` does not have fields", tys.render(*ty)),
        )
        .with_label(Label::primary(file, span.clone(), "field access not allowed here")),
    }
}
