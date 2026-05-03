use crate::reporter::Span;

use super::ty::TyId;

#[derive(Clone, Debug)]
pub enum TypeError {
    /// Two types fail to unify. `expected` and `found` are *resolved* TyIds
    /// (no `Infer` left in them). E0250.
    TypeMismatch {
        expected: TyId,
        found: TyId,
        span: Span,
    },
    /// Type-position name doesn't match any primitive (and v0 has no user
    /// types). E0251.
    UnknownType { name: String, span: Span },
    /// Callee in a `Call { callee, args }` doesn't have a function type.
    /// E0252.
    NotCallable { found: TyId, span: Span },
    /// Call arity mismatch. E0253.
    ///
    /// `at_least: true` switches the rendered wording from "expected N
    /// arguments, found M" to "expected at least N arguments, found M".
    /// Set by `infer_call` when the callee is a C-variadic fn (the
    /// fixed-arg count is the lower bound). See spec/15_VARIADIC.md.
    WrongArgCount {
        expected: usize,
        found: usize,
        at_least: bool,
        span: Span,
    },
    /// Indexing or field access — no array/struct support in v0. E0255.
    UnsupportedFeature { feature: &'static str, span: Span },
    /// Couldn't infer a type — escaped finalization without resolution. E0256.
    CannotInfer { span: Span },
    /// Inference would have to construct a self-referential type to make
    /// the program type-check (e.g. `let mut p = null; p = &mut p` forces
    /// `α := *mut α`). Caught by the occurs check at the Infer-binding
    /// site; without it, downstream walks (`resolve_fully`, `discharge_*`,
    /// renderer) would loop on the cyclic binding. E0271.
    CyclicType { span: Span },
    /// Pointer mutability subtype rule violated. The shapes match
    /// (caught earlier by unify) but a mutability tag would be silently
    /// upgraded — `*const T → *mut T`, or any inner-position mismatch.
    /// E0257.
    PointerMutabilityMismatch {
        expected: TyId,
        actual: TyId,
        span: Span,
    },
    /// Struct literal names a field that isn't declared on the ADT. E0258.
    StructLitUnknownField {
        field: String,
        adt: String,
        span: Span,
    },
    /// Struct literal omits a field that is declared on the ADT. E0259.
    StructLitMissingField {
        field: String,
        adt: String,
        lit_span: Span,
    },
    /// Struct literal names the same field twice. E0260.
    StructLitDuplicateField {
        field: String,
        first: Span,
        dup: Span,
    },
    /// `s.f` where `s` is a struct but `f` isn't a declared field of it. E0261.
    NoFieldOnAdt {
        field: String,
        adt: String,
        span: Span,
    },
    /// `e.f` where `e`'s type isn't an ADT (primitive, unit, fn, ptr...). E0262.
    TypeNotFieldable { ty: TyId, span: Span },
    /// `&mut place` or `place = rhs` where the root of `place` is an
    /// immutable Local (or, future, a `*const T` deref). The `op` field
    /// says which operation triggered it; the diagnostic message
    /// renders accordingly. See spec/10_ADDRESS_OF.md. E0263.
    MutateImmutable { op: MutateOp, span: Span },
    /// Unsized array `[T]` (`TyKind::Array(_, None)`) appearing at a
    /// value-type position — fn parameter, fn return, struct field, or
    /// let-binding. `[T]` is `[T; ∞]`-shaped; it has no statically known
    /// stride and therefore can't be allocated, copied, or passed by
    /// value. The fix is to use a pointer (`*const [T]` / `*mut [T]`)
    /// or a sized form (`[T; N]`). See spec/09_ARRAY.md.
    /// E0269.
    UnsizedArrayAsValue { pos: SizedPos, span: Span },

    /// Sized array `[T; N]` appearing by value at an `extern "C"` fn
    /// parameter or return slot. C has no calling convention for
    /// arrays-by-value (`int[10] f();` is a syntax error in C, and
    /// `void f(int arr[10])` silently decays to a pointer). Wrap the
    /// array in a pointer (`*const [T; N]` / `*mut [T; N]`) or use an
    /// unsized-array pointer (`*const [T]`). See spec/09_ARRAY.md.
    /// E0264.
    ArrayByValueAtExternC {
        which: ParamOrReturn,
        ty: TyId,
        span: Span,
    },

    /// Two array types with different lengths flowing into the same
    /// slot (e.g. `let a: [i32; 4] = [1, 2, 3]`, or passing a `[i32; 4]`
    /// to a parameter expecting `[i32; 3]`). Fired by `unify` when both
    /// sides are `Array(T, Some(_))` with mismatched length values.
    /// See spec/09_ARRAY.md. E0265.
    ArrayLengthMismatch {
        expected: TyId,
        found: TyId,
        span: Span,
    },

    /// `e[i]` where `e`'s type is not indexable — neither an array
    /// (`[T; N]` / `[T]`) nor a pointer to one. See spec/09_ARRAY.md.
    /// E0266.
    NotIndexable { ty: TyId, span: Span },

    /// `a[i]` where `i`'s type is not `usize`. The index slot is
    /// strict-`usize`; user must convert with `as usize` (or use an
    /// untyped int literal, which defaults to `usize` here via the
    /// int-flagged Infer path). See spec/09_ARRAY.md. E0267.
    IndexNotUsize { found: TyId, span: Span },

    /// An element in a list array literal `[e0, e1, ..., en]` has a
    /// type that doesn't unify with the first element's type.
    /// `i` is the 0-based index of the offending element (≥ 1, since
    /// element 0 establishes the type). See spec/09_ARRAY.md. E0268.
    ArrayLitElementMismatch {
        i: usize,
        expected: TyId,
        found: TyId,
        span: Span,
    },

    /// `*expr` where `expr` is not a pointer type. The operand's type
    /// (`found`) is fully resolved at the time of emission. See
    /// spec/07_POINTER.md "Deref operator". E0270.
    DerefNonPointer { found: TyId, span: Span },
    // E0271 (parse-time): `...` in a non-extern fn declaration. Reserved
    // here for cross-spec discoverability — emitted by the parser via
    // `ParseError::Custom`, not as a `TypeError`. See spec/15_VARIADIC.md.
    // Note: the existing `CyclicType` variant above also carries an
    // E0271 doc-comment; that's a code-collision the variadic spec
    // doesn't resolve and is left for follow-up.
    /// Argument passed in a C-variadic call slot has a type that's not
    /// promotable through C's default-argument-promotion rules.
    /// Accepted: integer primitives, `bool`, any pointer. Rejected:
    /// arrays, structs (by value), `()`, `!`, fn types, unresolved
    /// `Infer(_)`. `Error` is silently absorbed. See
    /// spec/15_VARIADIC.md "check_variadic_promotable". E0272.
    VariadicArgUnsupported { found: TyId, span: Span },

    /// ADT field types form a cycle without a `Ptr` indirection layer
    /// (`struct A { x: A }`, mutual `A { b: B } / B { a: A }`, or via
    /// sized arrays `struct A { xs: [A; 3] }`). Codegen would lower
    /// such an ADT to an LLVM struct of infinite size. The fix is to
    /// wrap the offending field in `*const T` / `*mut T`. Detected by
    /// the post-phase-0.5 tri-color DFS in `decl::check_recursive_adts`.
    /// See spec/08_ADT.md "Recursive type rejection". E0273.
    RecursiveAdt { adt: String, span: Span },
}

/// Discriminator on `ArrayByValueAtExternC` so the diagnostic can
/// name the offending position.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ParamOrReturn {
    Param,
    Return,
}

/// Discriminator on `MutateImmutable` so the diagnostic can phrase
/// the message appropriately for `&mut x` vs `x = v`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MutateOp {
    BorrowMut,
    Assign,
}

/// Discriminator on `UnsizedArrayAsValue` (and the `Sized` obligation
/// kind in `obligation.rs`) so the diagnostic can name the offending
/// position. Mirrors the value-type positions where an unsized type
/// is forbidden: fn parameter, fn return, struct field, let-binding,
/// and pointer dereference (per spec/07_POINTER.md — `*p` for
/// `p: *const [T]` would materialize an unsized value).
/// See spec/09_ARRAY.md.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SizedPos {
    Param,
    Return,
    Field,
    LetBinding,
    Deref,
}

impl TypeError {
    pub fn span(&self) -> &Span {
        match self {
            Self::TypeMismatch { span, .. }
            | Self::UnknownType { span, .. }
            | Self::NotCallable { span, .. }
            | Self::WrongArgCount { span, .. }
            | Self::UnsupportedFeature { span, .. }
            | Self::CannotInfer { span }
            | Self::CyclicType { span }
            | Self::PointerMutabilityMismatch { span, .. }
            | Self::StructLitUnknownField { span, .. }
            | Self::NoFieldOnAdt { span, .. }
            | Self::TypeNotFieldable { span, .. }
            | Self::MutateImmutable { span, .. }
            | Self::UnsizedArrayAsValue { span, .. }
            | Self::ArrayByValueAtExternC { span, .. }
            | Self::ArrayLengthMismatch { span, .. }
            | Self::NotIndexable { span, .. }
            | Self::IndexNotUsize { span, .. }
            | Self::ArrayLitElementMismatch { span, .. }
            | Self::DerefNonPointer { span, .. }
            | Self::VariadicArgUnsupported { span, .. }
            | Self::RecursiveAdt { span, .. } => span,
            Self::StructLitMissingField { lit_span, .. } => lit_span,
            Self::StructLitDuplicateField { dup, .. } => dup,
        }
    }
}
