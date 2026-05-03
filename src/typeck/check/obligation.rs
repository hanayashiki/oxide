//! Check-only obligations.
//!
//! See spec/05_TYPE_CHECKER.md "Obligations" — Phase 2 of the type
//! checker. Obligations are *deferred validations* that the inference
//! walk records as it goes; discharge runs once the relevant types are
//! settled, then resolves each captured TyId and inspects it.
//!
//! **Crucial property: discharge is pure observation.** Obligations
//! never call `unify`, never bind Infer vars, never introduce new type
//! variables. They only read resolved types and emit diagnostics.
//! All inference happens in Phase 1 (eager `unify` during the AST walk).
//!
//! **Two queues, two timings:**
//!
//! - **Body-phase** obligations live in `Inferer.obligations` and
//!   discharge inside `Checker::finalize` while the Inferer is still
//!   alive — captured TyIds may carry Infer references that need
//!   resolution against this fn's bindings. Each fn cleans up its own.
//! - **Decl-phase** Sized obligations live in `Checker.decl_obligations`
//!   and discharge at the end of `check`. They carry concrete TyIds
//!   from the start (decl resolution never produces Infer), so no
//!   Inferer is needed.
//!
//! Both queues feed the same `Checker::discharge_obligation` handler;
//! the Inferer is passed as `Option<&Inferer>`.
//!
//! Two obligation kinds today:
//!
//! - **`Coerce`** — the directional `*mut → *const` mut-compat check.
//!   `unify` is permissive on outer Ptr mutability (discards mut bits
//!   and recurses on inner types — see check.rs `unify`); the `Coerce`
//!   obligation enforces the `mut ≤ const` outer rule and strict
//!   mutability equality at every inner position. Enqueued from every
//!   `coerce` call site (after the eager unify body runs).
//! - **`Sized`** — `TyKind::Array(_, None)` (the unsized form) is
//!   rejected at fn parameter, fn return, struct field, and
//!   let-binding positions per spec/09_ARRAY.md. Decl-phase positions
//!   resolve to concrete TyIds; the let-binding case can carry Infer
//!   that resolves to an unsized array via inference (e.g. once deref
//!   lands, `let b = *a` where `a: *const [T]` makes `b: [T]`), so
//!   that case requires deferral against the Inferer.
//!
//! Future generics: `Sized` will be enqueued at instantiation sites
//! once `<T>` lands. The check-only architecture extends without
//! redesign.

use crate::reporter::Span;

use super::super::error::SizedPos;
use super::super::ty::TyId;

#[derive(Clone, Debug)]
pub(super) enum Obligation {
    /// Directional `*mut → *const` mut-compat check on a coercion site.
    /// `actual` and `expected` are the same TyIds passed to `coerce` —
    /// the eager unify body (run at the call site) has already linked
    /// any Infer vars; discharge resolves both sides fully and runs the
    /// outer-subtype + inner-strict-equality walk.
    Coerce {
        actual: TyId,
        expected: TyId,
        span: Span,
    },
    /// `ty` must be sized at this value position. Discharge rejects
    /// `TyKind::Array(_, None)` with `UnsizedArrayAsValue`.
    Sized {
        ty: TyId,
        pos: SizedPos,
        span: Span,
    },
    /// `ty` is an argument in a C-variadic call slot; it must be a type
    /// that flows through C's default-argument-promotion rules.
    /// Deferred so int-flagged Infer vars (typical of integer literals)
    /// have a chance to default to `i32` before the check runs. See
    /// spec/15_VARIADIC.md.
    VariadicPromotable { ty: TyId, span: Span },
}
