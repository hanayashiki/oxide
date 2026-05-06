use super::diagnostic::{Diagnostic, Label};
use super::source_map::FileId;
use crate::parser::ast::{BinOp, Mutability, UnOp};
use crate::typeck::{
    MutateOp, ParamOrReturn, PrimTy, PrimitiveSite, SizedPos, TyArena, TyId, TyKind, TypeError,
};

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
                SizedPos::TypeArg => (
                    "unsized array `[T]` cannot appear as a generic type argument",
                    "unsized type argument",
                    "generic functions assume T to be sized; pass a \
                     pointer (`*const [T]` / `*mut [T]`) or a sized array \
                     (`[T; N]`) instead.",
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

        TypeError::GenericArityMismatch {
            expected,
            found,
            span,
        } => Diagnostic::error(
            "E0275",
            format!("wrong number of type arguments: expected {expected}, found {found}"),
        )
        .with_label(Label::primary(
            file,
            span.clone(),
            format!("expected {expected} type arguments, found {found}"),
        ))
        .with_help(
            "the callee's `<T, U, ...>` list determines the type-argument arity; \
             non-generic fns expect zero.",
        ),

        TypeError::InvalidCast { src, dst, span } => Diagnostic::error(
            "E0274",
            format!(
                "cannot cast `{}` as `{}`",
                tys.render(*src),
                tys.render(*dst)
            ),
        )
        .with_label(Label::primary(file, span.clone(), "invalid cast"))
        .with_help(invalid_cast_help(tys, *src, *dst)),

        TypeError::PointerComparison { op, span } => {
            let (msg, help) = match op {
                BinOp::Eq | BinOp::Ne => (
                    "cannot compare raw pointers with `==` / `!=`",
                    "use `ox_ptr_eq` for pointer equality (see `stdlib/mem.ox`)",
                ),
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => (
                    "cannot order raw pointers",
                    "pointer ordering is undefined in v0; use `ox_ptr_eq` for equality",
                ),
                _ => unreachable!(
                    "PointerComparison only fires for cmp ops; got {:?}",
                    op
                ),
            };
            Diagnostic::error("E0279", msg)
                .with_label(Label::primary(file, span.clone(), "pointer comparison"))
                .with_help(help)
        }

        TypeError::NonIntegerOperand { site, found, span } => {
            let found_str = tys.render(*found);
            let found_kind = tys.kind(*found);
            let (label, help) = non_integer_help(*site, found_kind);
            Diagnostic::error(
                "E0280",
                format!("expected an integer operand, found `{found_str}`"),
            )
            .with_label(Label::primary(file, span.clone(), label))
            .with_help(help)
        }

        // E0281 — intrinsic-as-value. Intrinsics (`ox_size_of`,
        // `ox_transmute`, …) synthesize values rather than calling a
        // function, so a pointer to one is meaningless. Ordinary
        // generic fns are now allowed as values per spec/19's F1
        // lift; only intrinsics are rejected.
        TypeError::IntrinsicAsValue { name, span } => Diagnostic::error(
            "E0281",
            format!("cannot take the value of intrinsic fn `{name}`"),
        )
        .with_label(Label::primary(
            file,
            span.clone(),
            "intrinsic used as a value",
        ))
        .with_help(format!(
            "`{name}` is a compiler intrinsic, not a regular function; \
             intrinsics cannot be referenced as values — call them directly"
        )),
    }
}

/// Help text for E0274 `InvalidCast`. Per spec/12_AS.md §"Errors":
/// case-specific suggestions for the four canonical rejection
/// shapes (mut-launder, int↔bool, ADT cast, ptr reinterpret).
fn invalid_cast_help(tys: &TyArena, src: TyId, dst: TyId) -> &'static str {
    match (tys.kind(src), tys.kind(dst)) {
        (TyKind::Ptr(_, Mutability::Const), TyKind::Ptr(_, Mutability::Mut)) => {
            "raw `*const` → `*mut` is not allowed; declare the source as `*mut T` from the start, \
             or pass through an `extern` signature that takes `*mut T`"
        }
        (TyKind::Ptr(_, _), TyKind::Ptr(_, _)) => {
            "different pointee types are not interchangeable in v0; bind the pointer with the \
             intended `*const U` / `*mut U` declaration directly"
        }
        (TyKind::Prim(_), TyKind::Prim(PrimTy::Bool)) => {
            "compare against zero (e.g. `x != 0`) — booleans are not produced by `as`"
        }
        (TyKind::Adt(..), _) | (_, TyKind::Adt(..)) => {
            "struct / enum values cannot be cast to other types; access a field or write a conversion fn"
        }
        _ => "this combination of source and destination types is not a permitted cast in v0",
    }
}

/// Help text for E0280 `NonIntegerOperand`. Renders site- and
/// kind-aware advice. `Bin(Eq | Ne)` on `bool` is admitted at discharge
/// (see `discharge_primitive`), so this function is never called for
/// that combination — only ordering ops on bool need a hint here.
/// See spec/05_TYPE_CHECKER.md §Obligations.
fn non_integer_help(site: PrimitiveSite, found_kind: &TyKind) -> (&'static str, &'static str) {
    let label = "expected integer here";
    let help = match (site, found_kind) {
        // Bool — recommend logical-op replacements per op family.
        // `Eq` / `Ne` on bool no longer reach this function (admitted
        // at discharge); the ordering ops still error and get a
        // dedicated hint below.
        (
            PrimitiveSite::Bin(BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge),
            TyKind::Prim(PrimTy::Bool),
        ) => {
            "ordering comparisons aren't defined on `bool`; \
             branch on the value or convert via `b as i32` if you really need an order"
        }
        (PrimitiveSite::Bin(_), TyKind::Prim(PrimTy::Bool)) => {
            "boolean arithmetic / bitwise / shift isn't defined; \
             use `&&`, `||`, `!` for logical combinations"
        }
        (PrimitiveSite::Un(UnOp::Neg), TyKind::Prim(PrimTy::Bool)) => {
            "negation is integer-only; flip booleans with `!`"
        }
        (PrimitiveSite::Un(UnOp::BitNot), TyKind::Prim(PrimTy::Bool)) => {
            "bitwise NOT is integer-only; logical NOT is `!`"
        }
        (PrimitiveSite::Assign(_), TyKind::Prim(PrimTy::Bool)) => {
            "compound assignment is integer-only; booleans use `&&`, `||`, `!`"
        }

        // Pointer — uniform "deferred" help (cmp on Ptr is split into E0279).
        (_, TyKind::Ptr(..)) => {
            "pointer arithmetic / unary / compound assignment is not supported in v0; \
             the method form (`ptr.add`, etc.) is deferred per spec/07_POINTER.md"
        }

        // Adt / Fn / Array / Unit / Never / Param — generic.
        _ => "this expression's operand types must be integer primitives \
              (`i8`..`i64`, `u8`..`u64`, `usize`, `isize`)",
    };
    (label, help)
}
