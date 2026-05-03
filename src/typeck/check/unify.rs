//! Type-relating: the API surface that Checker uses to relate two
//! types.
//!
//! Two flavors of eager relating, decided by the call site:
//! - [`equate`] — symmetric, strict at every layer. Pointer mut
//!   equality required, array length equality required. No
//!   relaxation, no obligation push.
//! - [`subtype`] — directional `actual → expected`. Loose on outer
//!   Ptr mut, forward length erasure under Ptr. Always enqueues
//!   `Obligation::Coerce` so the directional check fires at
//!   discharge against fully-resolved types.
//!
//! Discharge handlers (run after inference settles) live here too —
//! they share invariants with the eager body and are written
//! together so a future change can't update one and forget the
//! other. See `Checker::discharge_obligation` for the dispatcher;
//! the per-kind handlers below are pub(super) entry points.
//!
//! **Privacy is the soundness boundary.** The mode flag, the
//! parametric `relate_with_ctx` body, and the Infer-binding helper
//! are all private to this module — there's no way for a Checker
//! method to call into the recursion with the wrong mode or skip
//! the obligation push. The only entry points are `equate`,
//! `equate_with`, `subtype`, `discharge_subtype`, and
//! `discharge_sized`. See spec/05_TYPE_CHECKER.md.

use crate::reporter::Span;

use super::super::error::{SizedPos, TypeError};
use super::super::ty::{InferId, TyId, TyKind};
use super::obligation::Obligation;
use super::{Checker, Inferer};

/// Diagnostic context for the terminal mismatch in an `equate_with`
/// call. Drives which `TypeError` variant is produced when the walk
/// bottoms out on incompatible kinds. Recursive calls inside the body
/// propagate the same `ctx`, so structured types (Fn-Fn, Ptr-Ptr,
/// Array-Array) terminating in a primitive mismatch still emit the
/// contextualized error. Slightly imprecise for nested types (e.g.
/// `[fn() -> i32, fn() -> u8]` reports `ArrayLitElementMismatch` with
/// the inner i32-vs-u8 pair), but the alternative — resetting at
/// recursion boundaries — would lose the outer framing entirely.
#[derive(Clone, Copy)]
pub(super) enum MismatchCtx {
    Default,
    ArrayLitElement { i: usize },
    IndexNotUsize,
}

/// Which relation the shared structural walk is enforcing. Private —
/// only the three eager entry points (`equate`, `equate_with`,
/// `subtype`) construct a context with a `Mode`, so a misuse like
/// "calling the parametric body with the wrong mode" is impossible
/// from outside this module.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Equate,
    Subtype,
}

/// Threaded through the shared `relate_with_ctx` body. Bundles the
/// diagnostic context, the structural-relaxation flag, and the
/// relation mode. Private to this module.
///
/// `pointee` is sticky: set to `true` when the Ptr-Ptr arm recurses
/// into pointer inners. Length erasure (`Array(T, Some(_)) ~ Array(T,
/// None)`) is silent only when `pointee == true`, `mode == Subtype`,
/// and only in the forward direction (found `Some`, expected `None`).
///
/// Span is *not* bundled here — it's `Clone` (not `Copy`) and gets
/// `.clone()`d at recursion sites; folding it into a `Copy` struct
/// would force unnecessary clones at every call.
#[derive(Clone, Copy)]
struct UnifyContext {
    mismatch: MismatchCtx,
    pointee: bool,
    mode: Mode,
}

impl UnifyContext {
    fn equate_default() -> Self {
        Self {
            mismatch: MismatchCtx::Default,
            pointee: false,
            mode: Mode::Equate,
        }
    }
    fn equate_with_mismatch(mismatch: MismatchCtx) -> Self {
        Self {
            mismatch,
            pointee: false,
            mode: Mode::Equate,
        }
    }
    fn subtype_default() -> Self {
        Self {
            mismatch: MismatchCtx::Default,
            pointee: false,
            mode: Mode::Subtype,
        }
    }
    fn under_ptr(self) -> Self {
        Self {
            pointee: true,
            ..self
        }
    }
}

fn build_mismatch(ctx: MismatchCtx, expected: TyId, found: TyId, span: Span) -> TypeError {
    match ctx {
        MismatchCtx::Default => TypeError::TypeMismatch {
            expected,
            found,
            span,
        },
        MismatchCtx::ArrayLitElement { i } => TypeError::ArrayLitElementMismatch {
            i,
            expected,
            found,
            span,
        },
        MismatchCtx::IndexNotUsize => TypeError::IndexNotUsize { found, span },
    }
}

// ===== Eager API =====

/// Symmetric Hindley-Milner equation. Strict at every layer:
/// pointer mut equality required, array length equality required.
/// No relaxation, no obligation push.
///
/// We retain the parameter names `found` / `expected` only because
/// the emitted `TypeMismatch` diagnostic renders them with those
/// labels; at most call sites the labels are a presentation choice
/// with no semantic weight. For directional sites, use [`subtype`].
///
/// Concretely: `Never` equates only with `Never`. Anything else
/// against `Never` is a mismatch — the "`!` flows into any context"
/// rule lives in [`subtype`], not here.
pub(super) fn equate(cx: &mut Checker, inf: &mut Inferer, found: TyId, expected: TyId, span: Span) {
    relate_with_ctx(cx, inf, found, expected, span, UnifyContext::equate_default());
}

/// `equate` with a [`MismatchCtx`] that controls how the terminal-
/// mismatch diagnostic is built. Top-level entry — `pointee` is
/// always `false` here.
pub(super) fn equate_with(
    cx: &mut Checker,
    inf: &mut Inferer,
    found: TyId,
    expected: TyId,
    span: Span,
    ctx: MismatchCtx,
) {
    relate_with_ctx(
        cx,
        inf,
        found,
        expected,
        span,
        UnifyContext::equate_with_mismatch(ctx),
    );
}

/// Use-site directional relation `actual → expected`. Two halves:
///
/// 1. **Eager structural walk** in `Mode::Subtype`. Loose on outer
///    Ptr mut (discharge enforces); forward length erasure under
///    Ptr. `Never` / `Error` actuals absorb at top.
/// 2. **Deferred check obligation.** Always enqueues
///    `Obligation::Coerce` (no `is_coercible` pruning) — discharge
///    is the soundness backstop against future variance / generic
///    Infer chains.
///
/// `expect_unit` is subsumed: `subtype(ty, Unit)` works because the
/// Ptr-Ptr branch never fires for Unit, so discharge is a no-op and
/// the eager body enforces the constraint.
pub(super) fn subtype(
    cx: &mut Checker,
    inf: &mut Inferer,
    actual: TyId,
    expected: TyId,
    span: Span,
) {
    let resolved_a = cx.resolve(inf, actual);
    if let TyKind::Never | TyKind::Error = cx.tys.kind(resolved_a) {
        return;
    }
    relate_with_ctx(
        cx,
        inf,
        actual,
        expected,
        span.clone(),
        UnifyContext::subtype_default(),
    );
    inf.obligations.push(Obligation::Coerce {
        actual,
        expected,
        span,
    });
}

// ===== Internal walk =====

/// Shared structural walk for `equate` and `subtype`. Private — the
/// only way to drive this body is through the three public entry
/// points, which fix the `Mode` (and thus the relaxation rules) at
/// construction time.
fn relate_with_ctx(
    cx: &mut Checker,
    inf: &mut Inferer,
    found: TyId,
    expected: TyId,
    span: Span,
    ctx: UnifyContext,
) {
    let found = cx.resolve(inf, found);
    let expected = cx.resolve(inf, expected);
    if found == expected {
        return;
    }
    let kf = cx.tys.kind(found).clone();
    let ke = cx.tys.kind(expected).clone();
    match (kf, ke) {
        (TyKind::Error, _) | (_, TyKind::Error) => {}
        (TyKind::Never, TyKind::Never) => {}
        (TyKind::Infer(id), other) => {
            bind_infer_checked(cx, inf, id, expected, &other, expected, found, ctx, span)
        }
        (other, TyKind::Infer(id)) => {
            bind_infer_checked(cx, inf, id, found, &other, expected, found, ctx, span)
        }
        (TyKind::Prim(p), TyKind::Prim(q)) if p == q => {}
        (TyKind::Unit, TyKind::Unit) => {}
        (TyKind::Fn(params_f, ret_f, var_f), TyKind::Fn(params_e, ret_e, var_e)) => {
            if params_f.len() != params_e.len() || var_f != var_e {
                inf.errors
                    .push(build_mismatch(ctx.mismatch, expected, found, span));
                return;
            }
            for (pf, pe) in params_f.iter().zip(&params_e) {
                relate_with_ctx(cx, inf, *pf, *pe, span.clone(), ctx);
            }
            relate_with_ctx(cx, inf, ret_f, ret_e, span, ctx);
        }
        // Ptr-Ptr arm: behavior depends on `mode`.
        //
        // - `Equate`: strict mut equality. Mismatch fires the
        //   contextualized diagnostic eagerly.
        // - `Subtype`: loose on outer Ptr mut (per spec/07 §3 — the
        //   directional rule fires at discharge over fully-resolved
        //   types via `discharge_subtype`).
        //
        // Recurse with `under_ptr`. In Equate mode the pointee flag
        // is meaningless but cheap to set; the Array-Array arm gates
        // length erasure on mode regardless.
        (TyKind::Ptr(fi, fm), TyKind::Ptr(ei, em)) => {
            if ctx.mode == Mode::Equate && fm != em {
                inf.errors
                    .push(build_mismatch(ctx.mismatch, expected, found, span));
                return;
            }
            relate_with_ctx(cx, inf, fi, ei, span, ctx.under_ptr());
        }
        // Array-Array: recurse on elem; length is gated.
        // - Same length: OK.
        // - Different concrete lengths: E0265.
        // - Mixed Some/None: silent ONLY when `mode == Subtype`,
        //   `pointee == true`, AND forward direction (found `Some`,
        //   expected `None`). Otherwise rejected.
        (TyKind::Array(fe, fc), TyKind::Array(ee, ec)) => {
            relate_with_ctx(cx, inf, fe, ee, span.clone(), ctx);
            match (fc, ec) {
                (None, None) => {}
                (Some(c1), Some(c2)) if c1 == c2 => {}
                (Some(_), Some(_)) => {
                    inf.errors.push(TypeError::ArrayLengthMismatch {
                        expected,
                        found,
                        span,
                    });
                }
                // Forward erasure is a Subtype-only relaxation,
                // gated to under-Ptr position.
                (Some(_), None) if ctx.pointee && ctx.mode == Mode::Subtype => {}
                (Some(_), None) | (None, Some(_)) => {
                    inf.errors.push(TypeError::ArrayLengthMismatch {
                        expected,
                        found,
                        span,
                    });
                }
            }
        }
        _ => {
            // Catch-all mismatch. Includes Adt-vs-Adt with unequal
            // `AdtId` (ADTs equate by pure nominal identity — see
            // spec/08_ADT.md "Unification"; equal ADTs are absorbed
            // by the `found == expected` short-circuit above).
            inf.errors
                .push(build_mismatch(ctx.mismatch, expected, found, span));
        }
    }
}

/// Bind an Infer var to a concrete type, rejecting int-flagged vars
/// being bound to non-integer concrete types. Private — only the
/// `relate_with_ctx` body binds; callers cannot bypass the
/// equate/subtype machinery to bind directly.
///
/// `expected` and `found` carry the original (post-resolve) sides so
/// that the int-flagged mismatch error labels them correctly regardless
/// of which arm of `relate_with_ctx` made the call: the int-flagged
/// `Infer(id)` may sit on either side, and the diagnostic's
/// `expected`/`found` must reflect the user's directional intent (e.g.
/// inside `subtype(value, target)`).
#[allow(clippy::too_many_arguments)]
fn bind_infer_checked(
    cx: &mut Checker,
    inf: &mut Inferer,
    id: InferId,
    target: TyId,
    target_kind: &TyKind,

    expected: TyId,
    found: TyId,
    
    ctx: UnifyContext,
    span: Span,
) {
    let int_flagged = inf.int_default[id];
    if int_flagged {
        let allowed = match target_kind {
            TyKind::Prim(p) => p.is_integer(),
            TyKind::Infer(_) | TyKind::Error => true,
            _ => false,
        };
        if !allowed {
            inf.errors
                .push(build_mismatch(ctx.mismatch, expected, found, span));
            // Bind to i32 (the int-flagged var's natural default)
            // rather than `error`. The mismatch is already pushed;
            // binding to the default lets the captured
            // `Infer(id)` (still in `expected`/`found` of the pushed
            // error) resolve to `i32` for the renderer, and lets
            // sibling expressions typed by this infer var surface as
            // `i32` in the types table — which matches what the user
            // wrote.
            inf.bind(id, cx.tys.i32);
            return;
        }
    }
    if occurs_in(cx, inf, id, target) {
        // Self-referential bind would create a cyclic type (e.g.
        // `α := *mut α` from `let mut p = null; p = &mut p`). Allowing
        // it would break the bindings-form-a-DAG invariant that
        // `resolve_fully` and the discharge walks rely on.
        inf.errors.push(TypeError::CyclicType { span });
        inf.bind(id, cx.tys.error);
        return;
    }
    // Propagate defaulting flags across an Infer-to-Infer bind so the
    // surviving (still-unbound) var keeps the original literal-default
    // intent at finalize. Without this, `let n = 1;` flags the literal's
    // var int_default but binds it to the unflagged local var, and the
    // local falls through to `error` at finalize.
    if let TyKind::Infer(target_id) = *target_kind {
        if inf.int_default[id] {
            inf.int_default[target_id] = true;
        }
        if inf.unit_default[id] {
            inf.unit_default[target_id] = true;
        }
    }
    inf.bind(id, target);
}

/// Walk `ty` (resolving Infer chains as we go) checking whether
/// `Infer(id)` appears anywhere inside. Private — used only by
/// `bind_infer_checked` to guard the one site that mutates
/// `inf.bindings`.
///
/// Termination: precondition is that `inf.bindings` currently forms a
/// DAG. Every prior `bind_infer_checked` enforced this same invariant,
/// so the resolved tree at this call is finite, and ordinary
/// structural induction terminates.
fn occurs_in(cx: &Checker, inf: &Inferer, id: InferId, ty: TyId) -> bool {
    let resolved = cx.resolve(inf, ty);
    match cx.tys.kind(resolved).clone() {
        TyKind::Infer(other) => other == id,
        TyKind::Ptr(pointee, _) => occurs_in(cx, inf, id, pointee),
        TyKind::Array(elem, _) => occurs_in(cx, inf, id, elem),
        TyKind::Fn(params, ret, _) => {
            params.iter().any(|p| occurs_in(cx, inf, id, *p))
                || occurs_in(cx, inf, id, ret)
        }
        // Adt is nominal-leaf (identity-only); Prim/Unit/Never/Error
        // are leaves with no Infer inside.
        _ => false,
    }
}

// ===== Discharge API =====
//
// Run after inference and integer-defaulting have settled. Inputs
// are fully resolved by `Checker::discharge_obligation` before being
// passed in. Pure observation: never unifies, never binds.

/// Recursive structural discharge of a `subtype(actual, expected)`
/// obligation. Top-level Ptr-Ptr allows `Mut ≤ Const` outer; inner
/// positions and aggregate-buried Ptrs require strict mut equality
/// via [`discharge_eq`]. Walks into Array elements and Fn params/ret
/// so a Ptr buried in an aggregate gets the same soundness treatment
/// as a top-level one (e.g. `[*const T; 3]` flowing into
/// `[*mut T; 3]`). See spec/07_POINTER.md / spec/05_TYPE_CHECKER.md.
///
/// Length mismatches and shape mismatches were already filed by the
/// eager body — this walk only emits mut-direction errors.
pub(super) fn discharge_subtype(
    cx: &mut Checker,
    actual: TyId,
    expected: TyId,
    span: Span,
) {
    let ka = cx.tys.kind(actual).clone();
    let ke = cx.tys.kind(expected).clone();
    match (ka, ke) {
        (TyKind::Ptr(a_pt, a_mut), TyKind::Ptr(e_pt, e_mut)) => {
            if a_mut > e_mut {
                cx.errors.push(TypeError::PointerMutabilityMismatch {
                    expected,
                    actual,
                    span: span.clone(),
                });
                return;
            }
            discharge_eq(cx, a_pt, e_pt, span);
        }
        (TyKind::Array(a_elem, _), TyKind::Array(e_elem, _)) => {
            discharge_subtype(cx, a_elem, e_elem, span);
        }
        (TyKind::Fn(a_params, a_ret, _), TyKind::Fn(e_params, e_ret, _)) => {
            if a_params.len() == e_params.len() {
                for (ap, ep) in a_params.iter().zip(&e_params) {
                    discharge_eq(cx, *ap, *ep, span.clone());
                }
            }
            discharge_eq(cx, a_ret, e_ret, span);
        }
        _ => {}
    }
}

/// Strict-equality counterpart to [`discharge_subtype`]. Used at
/// inner positions of a Ptr (where mut equality is required) and at
/// every position inside `Fn` (no variance in v0). Same structural
/// walk; never relaxes mut.
fn discharge_eq(cx: &mut Checker, a: TyId, b: TyId, span: Span) {
    let ka = cx.tys.kind(a).clone();
    let kb = cx.tys.kind(b).clone();
    match (ka, kb) {
        (TyKind::Ptr(a_pt, a_mut), TyKind::Ptr(b_pt, b_mut)) => {
            if a_mut != b_mut {
                cx.errors.push(TypeError::PointerMutabilityMismatch {
                    expected: b,
                    actual: a,
                    span: span.clone(),
                });
                return;
            }
            discharge_eq(cx, a_pt, b_pt, span);
        }
        (TyKind::Array(a_elem, _), TyKind::Array(b_elem, _)) => {
            discharge_eq(cx, a_elem, b_elem, span);
        }
        (TyKind::Fn(a_params, a_ret, _), TyKind::Fn(b_params, b_ret, _)) => {
            if a_params.len() == b_params.len() {
                for (ap, bp) in a_params.iter().zip(&b_params) {
                    discharge_eq(cx, *ap, *bp, span.clone());
                }
            }
            discharge_eq(cx, a_ret, b_ret, span);
        }
        _ => {}
    }
}

/// Recursive Sized check. Walks `Array(_, Some(_))` element types so
/// nested unsized inside a sized outer (e.g. `[[u8]; 3]`) is
/// rejected — closes the codegen ICE described in B008. Stops at
/// `Ptr` (the pointer is sized; the pointee can be unsized — that's
/// the canonical DST-behind-pointer shape). Doesn't descend into
/// `Adt` because each field carries its own decl-phase Sized
/// obligation; recursing here would double-report.
pub(super) fn discharge_sized(cx: &mut Checker, ty: TyId, pos: SizedPos, span: Span) {
    match cx.tys.kind(ty).clone() {
        TyKind::Array(_, None) => {
            cx.errors
                .push(TypeError::UnsizedArrayAsValue { pos, span });
        }
        TyKind::Array(elem, Some(_)) => {
            discharge_sized(cx, elem, pos, span);
        }
        // Ptr stops descent; everything else is sized in v0.
        _ => {}
    }
}

