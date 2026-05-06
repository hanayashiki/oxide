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
/// diagnostic context and the relation mode. Private to this module.
///
/// Span is *not* bundled here — it's `Clone` (not `Copy`) and gets
/// `.clone()`d at recursion sites; folding it into a `Copy` struct
/// would force unnecessary clones at every call.
///
/// The under-Ptr "pointee" flag formerly lived here, used to gate
/// array length erasure. That responsibility moved to
/// [`discharge_subtype`] (which carries its own `pointee` parameter)
/// when shape-error reporting was centralized in discharge — see
/// spec/05_TYPE_CHECKER.md §Obligations.
#[derive(Clone, Copy)]
struct UnifyContext {
    mismatch: MismatchCtx,
    mode: Mode,
}

impl UnifyContext {
    fn equate_default() -> Self {
        Self {
            mismatch: MismatchCtx::Default,
            mode: Mode::Equate,
        }
    }
    fn equate_with_mismatch(mismatch: MismatchCtx) -> Self {
        Self {
            mismatch,
            mode: Mode::Equate,
        }
    }
    fn subtype_default() -> Self {
        Self {
            mismatch: MismatchCtx::Default,
            mode: Mode::Subtype,
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
    relate_with_ctx(
        cx,
        inf,
        found,
        expected,
        span,
        UnifyContext::equate_default(),
    );
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
        (
            TyKind::Fn {
                params: params_f,
                ret: ret_f,
                is_extern_c: extern_f,
                c_variadic: var_f,
            },
            TyKind::Fn {
                params: params_e,
                ret: ret_e,
                is_extern_c: extern_e,
                c_variadic: var_e,
            },
        ) => {
            if params_f.len() != params_e.len()
                || extern_f != extern_e
                || var_f != var_e
            {
                // Equate mode emits eagerly; Subtype mode defers to
                // `discharge_subtype` so the same diagnostic doesn't
                // fire twice. See spec/05_TYPE_CHECKER.md §Obligations.
                if ctx.mode == Mode::Equate {
                    inf.errors
                        .push(build_mismatch(ctx.mismatch, expected, found, span));
                }
                return;
            }
            // Eager body keeps walking symmetrically — Infer-binding is
            // direction-agnostic. The directional variance check (params
            // contravariant, return covariant) lives in
            // `discharge_subtype_inner` per spec/19_FN_PTR.md §3.1.
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
        // Recurse with the same ctx — discharge handles the under-Ptr
        // length-erasure relaxation; eager just walks for Infer-binding.
        (TyKind::Ptr(fi, fm), TyKind::Ptr(ei, em)) => {
            if ctx.mode == Mode::Equate && fm != em {
                inf.errors
                    .push(build_mismatch(ctx.mismatch, expected, found, span));
                return;
            }
            relate_with_ctx(cx, inf, fi, ei, span, ctx);
        }
        // Adt-Adt: nominal identity (same `AdtId`) plus structural
        // recursion over args. The arity is fixed by AdtId, so length
        // mismatches are an internal invariant violation, not a user-
        // facing E0250. See spec/16_GENERIC.md §Typeck rules
        // (extension).
        (TyKind::Adt(af, args_f), TyKind::Adt(ae, args_e)) => {
            if af != ae {
                // Equate-only emit; Subtype defers to discharge.
                if ctx.mode == Mode::Equate {
                    inf.errors
                        .push(build_mismatch(ctx.mismatch, expected, found, span));
                }
                return;
            }
            debug_assert_eq!(
                args_f.len(),
                args_e.len(),
                "Adt arity invariant violated for AdtId {}",
                af.raw()
            );
            for (a_f, a_e) in args_f.iter().zip(&args_e) {
                relate_with_ctx(cx, inf, *a_f, *a_e, span.clone(), ctx);
            }
        }
        // Array-Array: recurse on elem; length is gated.
        // - Same length: OK.
        // - Different concrete lengths: E0265.
        // - Mixed Some/None: silent ONLY when `mode == Subtype`,
        //   `pointee == true`, AND forward direction (found `Some`,
        //   expected `None`). Otherwise rejected.
        (TyKind::Array(fe, fc), TyKind::Array(ee, ec)) => {
            relate_with_ctx(cx, inf, fe, ee, span.clone(), ctx);
            // Length errors: Equate emits eagerly; Subtype defers
            // to `discharge_subtype` (which knows about the
            // pointee gate too).
            if ctx.mode == Mode::Equate {
                match (fc, ec) {
                    (None, None) => {}
                    (Some(c1), Some(c2)) if c1 == c2 => {}
                    _ => {
                        inf.errors.push(TypeError::ArrayLengthMismatch {
                            expected,
                            found,
                            span,
                        });
                    }
                }
            }
        }
        _ => {
            // Catch-all cross-kind mismatch. Equate emits eagerly;
            // Subtype defers to `discharge_subtype`. See
            // spec/05_TYPE_CHECKER.md §Obligations.
            if ctx.mode == Mode::Equate {
                inf.errors
                    .push(build_mismatch(ctx.mismatch, expected, found, span));
            }
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
    let int_flagged = inf.bindings[id].int_default;
    if int_flagged {
        let allowed = match target_kind {
            TyKind::Prim(p) => p.is_integer(),
            TyKind::Infer(_) | TyKind::Error => true,
            _ => false,
        };
        if !allowed {
            // Equate emits eagerly; Subtype defers to
            // `discharge_subtype` so we don't double-report the same
            // mismatch (the Coerce obligation pushed by
            // `subtype()` will rediscover this once `α` is defaulted
            // to `i32`).
            if ctx.mode == Mode::Equate {
                inf.errors
                    .push(build_mismatch(ctx.mismatch, expected, found, span));
            }
            // Bind to i32 (the int-flagged var's natural default)
            // rather than `error`. The mismatch will surface from
            // discharge after defaulting; binding to i32 keeps
            // sibling expressions typed by this infer var coherent.
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
        if inf.bindings[id].int_default {
            inf.bindings[target_id].int_default = true;
        }
        if inf.bindings[id].unit_default {
            inf.bindings[target_id].unit_default = true;
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
        TyKind::Fn { params, ret, .. } => {
            params.iter().any(|p| occurs_in(cx, inf, id, *p)) || occurs_in(cx, inf, id, ret)
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
/// obligation. Single source of truth for subtype validation: walks
/// the structure, fires `PointerMutabilityMismatch` on outer Ptr
/// mut-direction violations, fires `ArrayLengthMismatch` on length
/// mismatches outside the Subtype-under-Ptr erasure case, and fires
/// `TypeMismatch` on Prim-Prim mismatches and cross-kind mismatches.
/// Inner Ptr / Fn positions defer to [`discharge_eq`] for strict
/// equality. See spec/07_POINTER.md / spec/05_TYPE_CHECKER.md.
pub(super) fn discharge_subtype(cx: &mut Checker, actual: TyId, expected: TyId, span: Span) {
    discharge_subtype_inner(cx, actual, expected, span, false);
}

fn discharge_subtype_inner(
    cx: &mut Checker,
    actual: TyId,
    expected: TyId,
    span: Span,
    pointee: bool,
) {
    if actual == expected {
        return;
    }
    let ka = cx.tys.kind(actual).clone();
    let ke = cx.tys.kind(expected).clone();
    // `Error` and (actual=)`Never` absorb without further reporting.
    if matches!(ka, TyKind::Error | TyKind::Never) || matches!(ke, TyKind::Error) {
        return;
    }
    match (ka, ke) {
        (TyKind::Ptr(a_pt, a_mut), TyKind::Ptr(e_pt, e_mut)) => {
            // Outer-Ptr (pointee=false): mut is directional —
            // `*mut → *const` allowed (`a_mut <= e_mut` after the
            // strict-greater rejection above). Inner-Ptr (pointee=true):
            // strict equality. Recurse with pointee=true so the inner
            // pointee continues the subtype walk under-Ptr semantics
            // (length erasure stays allowed; mut becomes strict via
            // the next iteration's `pointee=true` check).
            let mut_ok = if pointee { a_mut == e_mut } else { a_mut <= e_mut };
            if !mut_ok {
                cx.errors.push(TypeError::PointerMutabilityMismatch {
                    expected,
                    actual,
                    span: span.clone(),
                });
                return;
            }
            discharge_subtype_inner(cx, a_pt, e_pt, span, true);
        }
        (TyKind::Array(a_elem, a_len), TyKind::Array(e_elem, e_len)) => {
            match (a_len, e_len) {
                (Some(c1), Some(c2)) if c1 == c2 => {}
                (None, None) => {}
                // Forward erasure `[T; N] → [T]` allowed only under Ptr
                // (matches `relate_with_ctx`'s subtype-pointee rule).
                (Some(_), None) if pointee => {}
                _ => {
                    cx.errors.push(TypeError::ArrayLengthMismatch {
                        expected,
                        found: actual,
                        span: span.clone(),
                    });
                    return;
                }
            }
            // Reset `pointee` for the element recursion: forward
            // length erasure is gated to *directly* under a Ptr, not
            // "anywhere under a Ptr at any depth". Without the reset,
            // `*[[i32; 2]; 3]` would erroneously coerce to
            // `*[[i32]; 3]` (the outer array layer separates the
            // inner `[i32; 2]` from the Ptr, so erasure should be
            // forbidden). Spec/09_ARRAY.md "Forward erasure is a
            // Subtype-only relaxation, gated to under-Ptr position."
            discharge_subtype_inner(cx, a_elem, e_elem, span, false);
        }
        (
            TyKind::Fn {
                params: a_params,
                ret: a_ret,
                is_extern_c: a_extern,
                c_variadic: a_var,
            },
            TyKind::Fn {
                params: e_params,
                ret: e_ret,
                is_extern_c: e_extern,
                c_variadic: e_var,
            },
        ) => {
            // Invariant on arity, is_extern_c, c_variadic
            // (spec/19_FN_PTR.md §3.2/3.3).
            if a_params.len() != e_params.len()
                || a_extern != e_extern
                || a_var != e_var
            {
                cx.errors.push(TypeError::TypeMismatch {
                    expected,
                    found: actual,
                    span,
                });
                return;
            }
            // Contravariant on parameters: actual `Fn(A) -> _` is
            // acceptable as expected `Fn(B) -> _` iff B <: A. Note the
            // swapped order: subtype(expected_param, actual_param).
            // Reset pointee=false — under-Fn is *not* under-Ptr; the
            // length-erasure / mut-relax rules don't compose through Fn.
            for (ap, ep) in a_params.iter().zip(&e_params) {
                discharge_subtype_inner(cx, *ep, *ap, span.clone(), false);
            }
            // Covariant on return.
            discharge_subtype_inner(cx, a_ret, e_ret, span, false);
        }
        (TyKind::Adt(a_aid, a_args), TyKind::Adt(e_aid, e_args)) => {
            if a_aid != e_aid {
                cx.errors.push(TypeError::TypeMismatch {
                    expected,
                    found: actual,
                    span,
                });
                return;
            }
            debug_assert_eq!(a_args.len(), e_args.len());
            for (aa, ea) in a_args.iter().zip(&e_args) {
                discharge_eq(cx, *aa, *ea, span.clone());
            }
        }
        // Primitive mismatch (different `Prim` variants — equal
        // primitives intern to the same TyId and were caught by the
        // identity short-circuit at the top).
        (TyKind::Prim(_), TyKind::Prim(_)) => {
            cx.errors.push(TypeError::TypeMismatch {
                expected,
                found: actual,
                span,
            });
        }
        (TyKind::Unit, TyKind::Unit) => {}
        // Cross-kind catch-all: `Prim` vs `Ptr`, `Adt` vs `Ptr`, etc.
        _ => {
            cx.errors.push(TypeError::TypeMismatch {
                expected,
                found: actual,
                span,
            });
        }
    }
    let _ = pointee; // silence unused on the no-recurse arms
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
        (
            TyKind::Fn {
                params: a_params,
                ret: a_ret,
                ..
            },
            TyKind::Fn {
                params: b_params,
                ret: b_ret,
                ..
            },
        ) => {
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
/// rejected — closes the codegen ICE described in B008. The Ptr arm
/// implements the canonical DST-behind-pointer relaxation
/// (`*const [T]` is fine — the pointer carries the length at
/// runtime), but only one layer deep: a *sized* array pointee
/// (`*const [T; N]`) must still have a sized element type, otherwise
/// we'd have a sized container of unknown-stride elements (e.g.
/// `*const [[T]; 3]`) which is ill-formed. Doesn't descend into
/// `Adt` because each field carries its own decl-phase Sized
/// obligation; recursing here would double-report.
pub(super) fn discharge_sized(cx: &mut Checker, ty: TyId, pos: SizedPos, span: Span) {
    match cx.tys.kind(ty).clone() {
        TyKind::Array(_, None) => {
            cx.errors.push(TypeError::UnsizedArrayAsValue { pos, span });
        }
        TyKind::Array(elem, Some(_)) => {
            discharge_sized(cx, elem, pos, span);
        }
        TyKind::Ptr(pointee, _) => {
            // Allow `*const [T]` (DST). Reject `*const [[T]; N]`
            // by recursing only when the pointee is a *sized* array
            // — its element type must itself be sized. Other pointee
            // shapes (Prim, Ptr, Adt, …) are already sized; further
            // Ptr layers handle their own pointee at the next call.
            if let TyKind::Array(elem, Some(_)) = cx.tys.kind(pointee).clone() {
                discharge_sized(cx, elem, pos, span);
            }
        }
        TyKind::Fn { params, ret, .. } => {
            // spec/19_FN_PTR.md §4: a fn pointer's parameters and
            // return type must themselves be sized — same obligation
            // as the corresponding `fn_decl`. The outer Sized
            // obligation (Param / Return / Field / LetBinding /
            // Deref / TypeArg) covers the fn-pointer slot itself
            // (which is always pointer-sized); recurse to validate
            // inner positions.
            for p in params {
                discharge_sized(cx, p, pos, span.clone());
            }
            discharge_sized(cx, ret, pos, span);
        }
        // Everything else is sized in v0.
        _ => {}
    }
}
