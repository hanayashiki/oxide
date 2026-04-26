use crate::lexer::Span;

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
    WrongArgCount {
        expected: usize,
        found: usize,
        span: Span,
    },
    /// Indexing or field access — no array/struct support in v0. E0255.
    UnsupportedFeature { feature: &'static str, span: Span },
    /// Couldn't infer a type — escaped finalization without resolution. E0256.
    CannotInfer { span: Span },
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
            | Self::PointerMutabilityMismatch { span, .. }
            | Self::StructLitUnknownField { span, .. }
            | Self::NoFieldOnAdt { span, .. }
            | Self::TypeNotFieldable { span, .. } => span,
            Self::StructLitMissingField { lit_span, .. } => lit_span,
            Self::StructLitDuplicateField { dup, .. } => dup,
        }
    }
}
