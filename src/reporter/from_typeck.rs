use super::diagnostic::{Diagnostic, Label};
use super::source_map::FileId;
use crate::typeck::{MutateOp, ParamOrReturn, SizedPos, TyArena, TypeError};

/// Map a typeck error to a structured diagnostic. Needs the `TyArena` to
/// render type names (`expected: i32, found: bool`).
pub fn from_typeck_error(err: &TypeError, file: FileId, tys: &TyArena) -> Diagnostic {
    match err {
        TypeError::TypeMismatch {
            expected,
            found,
            span,
        } => Diagnostic::error(
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

        TypeError::WrongArgCount {
            expected,
            found,
            at_least,
            span,
        } => {
            let qualifier = if *at_least { "at least " } else { "" };
            Diagnostic::error(
                "E0253",
                format!(
                    "wrong number of arguments: expected {qualifier}{expected}, found {found}"
                ),
            )
            .with_label(Label::primary(file, span.clone(), "called here"))
        }

        TypeError::UnsupportedFeature { feature, span } => {
            Diagnostic::error("E0255", format!("unsupported in v0: {feature}"))
                .with_label(Label::primary(file, span.clone(), "not yet supported"))
        }

        TypeError::CannotInfer { span } => Diagnostic::error("E0256", "could not infer a type")
            .with_label(Label::primary(file, span.clone(), "ambiguous type"))
            .with_help("add a type annotation to disambiguate"),

        TypeError::CyclicType { span } => Diagnostic::error("E0271", "cannot infer cyclic type")
            .with_label(Label::primary(
                file,
                span.clone(),
                "type would refer to itself",
            ))
            .with_help(
                "the inferred type for this expression would have to contain itself \
                 (e.g. `α := *mut α`); add a type annotation to break the cycle",
            ),

        TypeError::PointerMutabilityMismatch {
            expected,
            actual,
            span,
        } => Diagnostic::error(
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

        TypeError::StructLitUnknownField { field, adt, span } => {
            Diagnostic::error("E0258", format!("struct `{adt}` has no field `{field}`"))
                .with_label(Label::primary(file, span.clone(), "unknown field"))
        }

        TypeError::StructLitMissingField {
            field,
            adt,
            lit_span,
        } => Diagnostic::error(
            "E0259",
            format!("missing field `{field}` in struct literal of `{adt}`"),
        )
        .with_label(Label::primary(file, lit_span.clone(), "field not provided")),

        TypeError::StructLitDuplicateField { field, first, dup } => {
            Diagnostic::error("E0260", format!("field `{field}` specified more than once"))
                .with_label(Label::primary(file, dup.clone(), "duplicate"))
                .with_label(Label::secondary(
                    file,
                    first.clone(),
                    "first specified here",
                ))
        }

        TypeError::NoFieldOnAdt { field, adt, span } => {
            Diagnostic::error("E0261", format!("no field `{field}` on type `{adt}`"))
                .with_label(Label::primary(file, span.clone(), "unknown field"))
        }

        TypeError::TypeNotFieldable { ty, span } => Diagnostic::error(
            "E0262",
            format!("type `{}` does not have fields", tys.render(*ty)),
        )
        .with_label(Label::primary(
            file,
            span.clone(),
            "field access not allowed here",
        )),

        TypeError::MutateImmutable { op, span } => {
            let (msg, label, help) = match op {
                MutateOp::BorrowMut => (
                    "cannot take a mutable pointer to an immutable place",
                    "cannot borrow as `&mut`",
                    "declare the binding `mut` (e.g. `let mut x` or `fn f(mut x: T)`) to allow `&mut` borrows",
                ),
                MutateOp::Assign => (
                    "cannot assign to an immutable place",
                    "cannot assign to this",
                    "declare the binding `mut` (e.g. `let mut x` or `fn f(mut x: T)`) to allow assignment",
                ),
            };
            Diagnostic::error("E0263", msg)
                .with_label(Label::primary(file, span.clone(), label))
                .with_help(help)
        }

        TypeError::ArrayByValueAtExternC { which, ty, span } => {
            let where_str = match which {
                ParamOrReturn::Param => "parameter",
                ParamOrReturn::Return => "return",
            };
            Diagnostic::error(
                "E0264",
                format!(
                    "sized array `{}` cannot appear by value at an `extern \"C\"` {where_str}",
                    tys.render(*ty)
                ),
            )
            .with_label(Label::primary(
                file,
                span.clone(),
                "array-by-value at C boundary",
            ))
            .with_help(
                "C has no calling convention for arrays-by-value; wrap in a pointer \
                 (`*const [T; N]` / `*mut [T; N]`) or use an unsized-array pointer \
                 (`*const [T]`)",
            )
        }

        TypeError::ArrayLengthMismatch {
            expected,
            found,
            span,
        } => Diagnostic::error(
            "E0265",
            format!(
                "array length mismatch: expected `{}`, found `{}`",
                tys.render(*expected),
                tys.render(*found)
            ),
        )
        .with_label(Label::primary(file, span.clone(), "length mismatch")),

        TypeError::NotIndexable { ty, span } => Diagnostic::error(
            "E0266",
            format!("type `{}` cannot be indexed", tys.render(*ty)),
        )
        .with_label(Label::primary(file, span.clone(), "not indexable"))
        .with_help(
            "indexing requires an array type `[T; N]` / `[T]` or a pointer to one \
             (`*const [T; N]`, `*mut [T]`, etc.)",
        ),

        TypeError::IndexNotUsize { found, span } => Diagnostic::error(
            "E0267",
            format!(
                "array index must be `usize`, found `{}`",
                tys.render(*found)
            ),
        )
        .with_label(Label::primary(file, span.clone(), "index not `usize`"))
        .with_help("convert with `as usize` if you have an integer of another type"),

        TypeError::ArrayLitElementMismatch {
            i,
            expected,
            found,
            span,
        } => Diagnostic::error(
            "E0268",
            format!(
                "array literal element {i} has type `{}`, expected `{}`",
                tys.render(*found),
                tys.render(*expected)
            ),
        )
        .with_label(Label::primary(
            file,
            span.clone(),
            "element type differs from element 0",
        )),

        TypeError::UnsizedArrayAsValue { pos, span } => {
            let (msg, label, help) = match pos {
                SizedPos::Param => (
                    "unsized array `[T]` cannot appear by value at a function parameter",
                    "unsized type at value position",
                    "use a pointer (`*const [T]` / `*mut [T]`) for runtime-sized buffers, \
                     or a sized array `[T; N]` if the length is known at compile time",
                ),
                SizedPos::Return => (
                    "unsized array `[T]` cannot appear by value at a function return",
                    "unsized type at value position",
                    "use a pointer (`*const [T]` / `*mut [T]`) for runtime-sized buffers, \
                     or a sized array `[T; N]` if the length is known at compile time",
                ),
                SizedPos::Field => (
                    "unsized array `[T]` cannot appear by value at a struct field",
                    "unsized type at value position",
                    "use a pointer (`*const [T]` / `*mut [T]`) for runtime-sized buffers, \
                     or a sized array `[T; N]` if the length is known at compile time",
                ),
                SizedPos::LetBinding => (
                    "unsized array `[T]` cannot appear by value at a let-binding",
                    "unsized type at value position",
                    "use a pointer (`*const [T]` / `*mut [T]`) for runtime-sized buffers, \
                     or a sized array `[T; N]` if the length is known at compile time",
                ),
                SizedPos::Deref => (
                    "cannot dereference pointer to unsized array `[T]`",
                    "unsized pointee",
                    "dereferencing `*const [T]` / `*mut [T]` would materialize an \
                     unsized value; use `p[i]` to index through the pointer instead",
                ),
            };
            Diagnostic::error("E0269", msg)
                .with_label(Label::primary(file, span.clone(), label))
                .with_help(help)
        }

        TypeError::DerefNonPointer { found, span } => Diagnostic::error(
            "E0270",
            format!(
                "cannot dereference value of type `{}`",
                tys.render(*found)
            ),
        )
        .with_label(Label::primary(file, span.clone(), "not a pointer"))
        .with_help("`*` requires a pointer operand (`*const T` or `*mut T`)"),

        TypeError::VariadicArgUnsupported { found, span } => Diagnostic::error(
            "E0272",
            format!(
                "cannot pass `{}` through a variadic parameter",
                tys.render(*found)
            ),
        )
        .with_label(Label::primary(file, span.clone(), "unsupported variadic arg"))
        .with_help(
            "variadic args must be an integer, pointer, or `bool` — wrap structs/arrays \
             in a `*const T` if you mean to pass by reference.",
        ),

        TypeError::RecursiveAdt { adt, span } => Diagnostic::error(
            "E0273",
            format!("recursive type `{adt}` has infinite size"),
        )
        .with_label(Label::primary(
            file,
            span.clone(),
            "recursive without indirection",
        ))
        .with_help(
            "wrap the field in a pointer (`*const T` / `*mut T`) so the cycle \
             goes through an indirection",
        ),
    }
}
