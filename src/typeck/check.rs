//! The type checker. Three logical phases (the first three live in the
//! child `decl` submodule):
//!   0    — alloc `AdtDef` stubs (`partial: true`), pre-intern
//!          `TyKind::Adt(aid)` for each HIR adt.
//!   0.5  — backfill ADT field types now that every `AdtId` is known.
//!   1    — resolve fn signatures from source annotations.
//!   2    — check each fn body in isolation with a fresh `Inferer`.
//!
//! State split:
//! - `Checker<'hir>` borrows the HIR for its whole lifetime and owns the
//!   module-scope outputs (`tys`, `adts`, `fn_sigs`, `local_tys`,
//!   `expr_tys`, `errors`) that survive past any single fn.
//! - `Inferer` is constructed fresh on the stack per fn body and owns
//!   everything that's only meaningful while that body is being inferred:
//!   the unification table, int-default flags, in-flight errors, and the
//!   declared return type. It's threaded into helpers as `&mut Inferer`.
//!
//! Errors emitted during inference accumulate on the `Inferer` (carrying
//! potentially unresolved `Infer` TyIds); at finalize time we resolve
//! those TyIds — once int-defaults have been applied — and flush them
//! into `Checker.errors`. The renderer always sees concrete types,
//! never raw `?Tn` placeholders.

mod decl;
mod obligation;

use index_vec::IndexVec;

use crate::hir::{
    FnId, HBlockId, HElseArm, HExprId, HirArrayLit, HirConst, HirExpr, HirExprKind, HirLocal,
    HirModule, HirStructLitField, HirTy, HirTyKind, LocalId, VariantIdx,
};
use crate::lexer::Span;
use crate::parser::ast::{AssignOp, BinOp, Mutability, UnOp};

use self::obligation::Obligation;
use super::error::{MutateOp, SizedPos, TypeError};
use super::ty::{AdtDef, AdtId, FnSig, InferId, TyArena, TyId, TyKind};

/// Context for diagnostic construction at a `unify` mismatch site.
/// Passed through `unify_with` so the same recursive `unify` body can
/// produce different `TypeError` variants depending on the calling
/// context (array-literal-element check, index-must-be-usize, etc.)
/// without duplicating the `unify` body or doing post-hoc error swap.
///
/// Recursive `unify` calls inside the body propagate the same `ctx`,
/// so structured types (Fn-Fn, Ptr-Ptr, Array-Array) terminating in a
/// primitive mismatch still emit the contextualized error. Slightly
/// imprecise for nested types (e.g. `[fn() -> i32, fn() -> u8]` reports
/// `ArrayLitElementMismatch` with the inner i32-vs-u8 pair), but the
/// alternative — resetting `ctx` at recursion boundaries — would lose
/// the array-element framing entirely on a generic `TypeMismatch`.
#[derive(Clone, Copy)]
pub(super) enum MismatchCtx {
    Default,
    ArrayLitElement { i: usize },
    IndexNotUsize,
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

/// Threaded through `unify_with`. Bundles the diagnostic context and
/// the structural-relaxation flag.
///
/// `pointee` is sticky: set to `true` when the Ptr-Ptr arm recurses
/// into pointer inners; all deeper recursions inherit it. Length
/// erasure (`Array(T, Some(_)) ~ Array(T, None)`) is silent only
/// when `pointee == true` and only in the forward direction
/// (found `Some`, expected `None`). Top-level (`pointee == false`)
/// requires strict structural equality on length.
///
/// Span is *not* bundled here — it's `Clone` (not `Copy`) and gets
/// `.clone()`d at recursion sites; folding it into a `Copy` struct
/// would force unnecessary clones at every call.
#[derive(Clone, Copy)]
pub(super) struct UnifyContext {
    pub(super) mismatch: MismatchCtx,
    pub(super) pointee: bool,
}

impl UnifyContext {
    fn default_ctx() -> Self {
        UnifyContext {
            mismatch: MismatchCtx::Default,
            pointee: false,
        }
    }
    fn from_mismatch(mismatch: MismatchCtx) -> Self {
        UnifyContext {
            mismatch,
            pointee: false,
        }
    }
    fn under_ptr(self) -> Self {
        UnifyContext {
            pointee: true,
            ..self
        }
    }
}

#[derive(Clone, Debug)]
pub struct TypeckResults {
    pub tys: TyArena,
    pub adts: IndexVec<AdtId, AdtDef>,
    pub fn_sigs: IndexVec<FnId, FnSig>,
    pub local_tys: IndexVec<LocalId, TyId>,
    pub expr_tys: IndexVec<HExprId, TyId>,
}

/// Query-style API surface. Internally these are O(1) lookups into the
/// `IndexVec` side-tables (computed eagerly during `check`); the methods
/// exist so callers (codegen, IDE features, future incremental work)
/// don't have to reach into the field structure directly.
impl TypeckResults {
    pub fn type_of_expr(&self, eid: HExprId) -> TyId {
        self.expr_tys[eid]
    }
    pub fn type_of_local(&self, lid: LocalId) -> TyId {
        self.local_tys[lid]
    }
    pub fn fn_sig(&self, fid: FnId) -> &FnSig {
        let sig = &self.fn_sigs[fid];
        debug_assert!(!sig.partial, "fn_sig({fid:?}) read while partial");
        sig
    }
    pub fn adt_def(&self, aid: AdtId) -> &AdtDef {
        let adt = &self.adts[aid];
        debug_assert!(!adt.partial, "adt_def({aid:?}) read while partial");
        adt
    }
    pub fn tys(&self) -> &TyArena {
        &self.tys
    }
}

pub fn check(hir: &HirModule) -> (TypeckResults, Vec<TypeError>) {
    let mut cx = Checker::new(hir);
    decl::resolve_decls(&mut cx);
    for (fid, _) in hir.fns.iter_enumerated() {
        cx.check_fn(fid);
    }
    // Body-phase obligations have already been discharged per-fn inside
    // `Checker::finalize`. Decl-phase Sized obligations carry concrete
    // TyIds (no Inferer needed); drain them here.
    let pending = std::mem::take(&mut cx.decl_obligations);
    for obl in pending {
        cx.discharge_obligation(obl, None);
    }
    cx.finish()
}

struct Checker<'hir> {
    hir: &'hir HirModule,
    tys: TyArena,
    adts: IndexVec<AdtId, AdtDef>,
    fn_sigs: IndexVec<FnId, FnSig>,
    local_tys: IndexVec<LocalId, TyId>,
    expr_tys: IndexVec<HExprId, TyId>,
    errors: Vec<TypeError>,
    /// Decl-phase Sized obligations (param / return / struct field).
    /// All have concrete TyIds — `resolve_ty` never produces `Infer`.
    /// Drained once at the end of `check`. Body-phase obligations live
    /// in `Inferer.obligations` and discharge per-fn at `finalize`;
    /// they never enter this queue.
    decl_obligations: Vec<Obligation>,
}

struct Inferer {
    bindings: IndexVec<InferId, Option<TyId>>,
    int_default: IndexVec<InferId, bool>,
    /// Parallel to `int_default`. Set on the elem var of an empty array
    /// literal `[]` so finalize defaults the unbound var to `unit`
    /// (yielding `[(); 0]`) instead of `error`. Honest default for
    /// "an empty container of nothing" — zero runtime cost (`[(); 0]`
    /// is zero bytes; length is 0). Intentional v0 deviation from
    /// Rust's E0282; see spec/09_ARRAY.md.
    unit_default: IndexVec<InferId, bool>,
    /// Errors emitted while this fn body was being inferred. TyId fields
    /// inside may still point at unresolved `Infer` vars; `Checker::finalize`
    /// resolves them post-defaulting before flushing into `Checker.errors`.
    errors: Vec<TypeError>,
    /// Check-only obligations enqueued during this fn body's inference.
    /// Drained at `Checker::finalize` after int-defaulting and side-table
    /// resolution have settled — see `obligation.rs` for the discipline.
    /// TyIds inside captured obligations may reference Infer vars at push
    /// time; `discharge` resolves them through the Inferer before reading.
    obligations: Vec<Obligation>,
    /// Expected return type of the fn whose body is being inferred. Read
    /// by the `Return` arm of `infer_expr`; doesn't change for the
    /// lifetime of this Inferer.
    cur_ret: TyId,
    /// Stack of "expected type of the enclosing loop expression," one
    /// frame per loop currently being checked. Pushed before
    /// `infer_block(body)` and popped after; the innermost frame is
    /// `Break`'s coerce target. Always non-empty inside `Break` /
    /// `Continue` arms — HIR-lower already filed E0263/E0264 for stray
    /// uses, so the typeck-side `last()` is an invariant assertion.
    /// See spec/13_LOOPS.md "Type rule".
    loop_tys: Vec<TyId>,
}

impl Inferer {
    fn new(cur_ret: TyId) -> Self {
        Self {
            bindings: IndexVec::new(),
            int_default: IndexVec::new(),
            unit_default: IndexVec::new(),
            errors: Vec::new(),
            obligations: Vec::new(),
            cur_ret,
            loop_tys: Vec::new(),
        }
    }
    fn new_var(&mut self, int_default: bool) -> InferId {
        let id = self.bindings.push(None);
        let _ = self.int_default.push(int_default);
        let _ = self.unit_default.push(false);
        id
    }
    /// Allocate a fresh infer var that defaults to `()` if unbound at
    /// finalize time (vs. `i32` for int-flagged vars and `error` for
    /// plain ones). Used by the empty-`[]` arm of `infer_array_lit`.
    fn new_var_unit_default(&mut self) -> InferId {
        let id = self.bindings.push(None);
        let _ = self.int_default.push(false);
        let _ = self.unit_default.push(true);
        id
    }
    fn bind(&mut self, id: InferId, ty: TyId) {
        self.bindings[id] = Some(ty);
    }
}

impl<'hir> Checker<'hir> {
    fn new(hir: &'hir HirModule) -> Self {
        let tys = TyArena::new();
        let placeholder = tys.error;
        let local_tys: IndexVec<LocalId, TyId> =
            (0..hir.locals.len()).map(|_| placeholder).collect();
        let expr_tys: IndexVec<HExprId, TyId> = (0..hir.exprs.len()).map(|_| placeholder).collect();
        let fn_sigs: IndexVec<FnId, FnSig> = (0..hir.fns.len())
            .map(|_| FnSig {
                params: Vec::new(),
                ret: placeholder,
                partial: true,
            })
            .collect();
        Self {
            hir,
            tys,
            adts: IndexVec::new(),
            fn_sigs,
            local_tys,
            expr_tys,
            errors: Vec::new(),
            decl_obligations: Vec::new(),
        }
    }

    /// Phase 2. Each fn body gets a fresh stack-owned Inferer; finalize
    /// defaults its bindings, resolves this fn's contributions to
    /// `expr_tys`/`local_tys`, and flushes any inferer-carried errors
    /// (resolving their TyIds first) into `Checker.errors`. Foreign fns
    /// (`body == None`) have nothing to infer — we skip them.
    fn check_fn(&mut self, fid: FnId) {
        let Some(body_id) = self.hir.fns[fid].body else {
            return;
        };
        let mut inf = Inferer::new(self.fn_sigs[fid].ret);
        let body_ty = self.infer_block(&mut inf, body_id);
        let body_span = self.hir.blocks[body_id].span.clone();
        let cur_ret = inf.cur_ret;
        self.coerce(&mut inf, body_ty, cur_ret, body_span);
        self.finalize(inf);
    }

    fn finalize(&mut self, mut inf: Inferer) {
        // Default unconstrained vars per their flagged-default precedence:
        // int_default → i32 (most numeric literals); unit_default → ()
        // (empty `[]` elem, see spec/09_ARRAY.md); else → error (silent
        // — implicit "{error}" propagation; cascading mismatches were
        // already reported).
        let i32_id = self.tys.i32;
        let unit_id = self.tys.unit;
        let error_id = self.tys.error;
        for raw in 0..inf.bindings.len() {
            let id = InferId::from_raw(raw as u32);
            if inf.bindings[id].is_none() {
                inf.bindings[id] = Some(if inf.int_default[id] {
                    i32_id
                } else if inf.unit_default[id] {
                    unit_id
                } else {
                    error_id
                });
            }
        }

        // Resolve any Infer-typed entries in this fn's contributions.
        for raw in 0..self.expr_tys.len() {
            let id = HExprId::from_raw(raw as u32);
            let resolved = self.resolve_fully(&inf, self.expr_tys[id]);
            self.expr_tys[id] = resolved;
        }
        for raw in 0..self.local_tys.len() {
            let id = LocalId::from_raw(raw as u32);
            let resolved = self.resolve_fully(&inf, self.local_tys[id]);
            self.local_tys[id] = resolved;
        }

        // Flush this fn's errors. TyId fields inside may still reference
        // Infer vars captured mid-inference; resolve them now (after
        // int-default has run) so the renderer prints concrete types.
        let pending = std::mem::take(&mut inf.errors);
        for mut err in pending {
            self.resolve_error_tys(&inf, &mut err);
            self.errors.push(err);
        }

        // Per-fn discharge: the Inferer is still alive, so obligations
        // resolve their captured TyIds against this fn's bindings
        // (post int-default) before observing. Body-phase obligations
        // never escape into the Checker queue — each fn cleans up its
        // own. See spec/05_TYPE_CHECKER.md "Obligations".
        let pending_obls = std::mem::take(&mut inf.obligations);
        for obl in pending_obls {
            self.discharge_obligation(obl, Some(&inf));
        }
    }

    /// Run one obligation against the (now frozen) type universe.
    /// `inf` is `Some` for body-phase discharge — captured TyIds may
    /// contain Infer vars that need resolution through that fn's
    /// bindings — and `None` for decl-phase discharge where TyIds are
    /// already concrete. Pure observation: never unifies, never binds.
    fn discharge_obligation(&mut self, obl: Obligation, inf: Option<&Inferer>) {
        match obl {
            Obligation::Coerce {
                actual,
                expected,
                span,
            } => {
                let a = match inf {
                    Some(i) => self.resolve_fully(i, actual),
                    None => actual,
                };
                let e = match inf {
                    Some(i) => self.resolve_fully(i, expected),
                    None => expected,
                };
                self.discharge_coerce(a, e, span);
            }
            Obligation::Sized { ty, pos, span } => {
                let t = match inf {
                    Some(i) => self.resolve_fully(i, ty),
                    None => ty,
                };
                if let TyKind::Array(_, None) = self.tys.kind(t) {
                    self.errors
                        .push(TypeError::UnsizedArrayAsValue { pos, span });
                }
            }
        }
    }

    /// Pointer-mutability validation for a coercion site. Top-level
    /// rule: `actual_mut ≤ expected_mut` (`*mut → *const` allowed,
    /// reverse rejected). Inner positions: strict mutability equality
    /// (recursive). Non-Ptr-Ptr inputs are no-ops — `unify`'s
    /// shape-mismatch diagnostic has already fired during the eager
    /// half of `coerce`. See spec/07_POINTER.md.
    ///
    /// Array length erasure / shape relaxation: now enforced eagerly
    /// in `unify_with_ctx`'s Array-Array arm under the `pointee` flag
    /// (gated to forward `Some → None` direction). Discharge no longer
    /// needs to validate array direction — by the time it runs, any
    /// invalid mixed-`Some/None` pair has already been rejected by
    /// the eager unify. See spec/09_ARRAY.md "Coercions".
    fn discharge_coerce(&mut self, actual: TyId, expected: TyId, span: Span) {
        let (a_pt, a_mut, e_pt, e_mut) = match (self.tys.kind(actual), self.tys.kind(expected)) {
            (&TyKind::Ptr(ap, am), &TyKind::Ptr(ep, em)) => (ap, am, ep, em),
            _ => return,
        };
        if a_mut > e_mut {
            self.errors.push(TypeError::PointerMutabilityMismatch {
                expected,
                actual,
                span,
            });
            return;
        }
        self.discharge_ptr_inner_eq(a_pt, e_pt, span);
    }

    /// Recursive strict-mutability equality at every inner pointer
    /// position. Shape mismatches (including array-direction) have
    /// already been caught by the eager `unify` body of `coerce`;
    /// this only emits errors for mutability divergence at inner
    /// pointer layers.
    fn discharge_ptr_inner_eq(&mut self, a: TyId, b: TyId, span: Span) {
        if let (&TyKind::Ptr(a_pt, a_mut), &TyKind::Ptr(b_pt, b_mut)) =
            (self.tys.kind(a), self.tys.kind(b))
        {
            if a_mut != b_mut {
                self.errors.push(TypeError::PointerMutabilityMismatch {
                    expected: b,
                    actual: a,
                    span,
                });
                return;
            }
            self.discharge_ptr_inner_eq(a_pt, b_pt, span);
        }
    }

    fn resolve_error_tys(&mut self, inf: &Inferer, err: &mut TypeError) {
        match err {
            TypeError::TypeMismatch {
                expected, found, ..
            } => {
                *expected = self.resolve_fully(inf, *expected);
                *found = self.resolve_fully(inf, *found);
            }
            TypeError::NotCallable { found, .. } => {
                *found = self.resolve_fully(inf, *found);
            }
            TypeError::PointerMutabilityMismatch {
                expected, actual, ..
            } => {
                *expected = self.resolve_fully(inf, *expected);
                *actual = self.resolve_fully(inf, *actual);
            }
            TypeError::TypeNotFieldable { ty, .. } => {
                *ty = self.resolve_fully(inf, *ty);
            }
            TypeError::ArrayByValueAtExternC { ty, .. } => {
                *ty = self.resolve_fully(inf, *ty);
            }
            TypeError::ArrayLengthMismatch {
                expected, found, ..
            } => {
                *expected = self.resolve_fully(inf, *expected);
                *found = self.resolve_fully(inf, *found);
            }
            TypeError::NotIndexable { ty, .. } => {
                *ty = self.resolve_fully(inf, *ty);
            }
            TypeError::IndexNotUsize { found, .. } => {
                *found = self.resolve_fully(inf, *found);
            }
            TypeError::ArrayLitElementMismatch {
                expected, found, ..
            } => {
                *expected = self.resolve_fully(inf, *expected);
                *found = self.resolve_fully(inf, *found);
            }
            TypeError::DerefNonPointer { found, .. } => {
                *found = self.resolve_fully(inf, *found);
            }
            TypeError::UnknownType { .. }
            | TypeError::WrongArgCount { .. }
            | TypeError::UnsupportedFeature { .. }
            | TypeError::CannotInfer { .. }
            | TypeError::StructLitUnknownField { .. }
            | TypeError::StructLitMissingField { .. }
            | TypeError::StructLitDuplicateField { .. }
            | TypeError::NoFieldOnAdt { .. }
            | TypeError::MutateImmutable { .. }
            | TypeError::UnsizedArrayAsValue { .. } => {}
        }
    }

    fn finish(self) -> (TypeckResults, Vec<TypeError>) {
        debug_assert!(
            self.fn_sigs.iter().all(|s| !s.partial),
            "Checker::finish: at least one FnSig still partial"
        );
        debug_assert!(
            self.adts.iter().all(|a| !a.partial),
            "Checker::finish: at least one AdtDef still partial"
        );
        (
            TypeckResults {
                tys: self.tys,
                adts: self.adts,
                fn_sigs: self.fn_sigs,
                local_tys: self.local_tys,
                expr_tys: self.expr_tys,
            },
            self.errors,
        )
    }

    // ---------- type lookup helpers ----------

    /// Associated fn rather than a method: callers have to pass the
    /// arena and error sink explicitly so the same routine can serve
    /// both phases — sig phase points `errors` at `Checker.errors`,
    /// body phase points it at the active `Inferer.errors`.
    fn resolve_ty(tys: &mut TyArena, errors: &mut Vec<TypeError>, ty: &HirTy) -> TyId {
        match &ty.kind {
            HirTyKind::Named(name) => match tys.from_prim_name(name) {
                Some(id) => id,
                None => {
                    errors.push(TypeError::UnknownType {
                        name: name.clone(),
                        span: ty.span.clone(),
                    });
                    tys.error
                }
            },
            HirTyKind::Adt(haid) => {
                // 1:1 HAdtId → AdtId today. Phase 0 in `decl::resolve_decls`
                // pre-allocated the AdtDef stub and pre-interned this
                // identity, so the intern is a hit; partial state of the
                // AdtDef itself is irrelevant here — `TyKind::Adt(_)`
                // only carries the identity.
                let aid = AdtId::from_raw(haid.raw());
                tys.intern(TyKind::Adt(aid))
            }
            HirTyKind::Ptr {
                mutability,
                pointee,
            } => {
                let pointee = Self::resolve_ty(tys, errors, pointee);
                tys.intern(TyKind::Ptr(pointee, *mutability))
            }
            HirTyKind::Array(elem, hconst_opt) => {
                let elem_id = Self::resolve_ty(tys, errors, elem);
                let len_opt = hconst_opt.as_ref().map(|hc| match hc {
                    HirConst::Lit(n) => *n,
                    HirConst::Error => unreachable!(
                        "parser+lower guarantee Lit; HirConst::Error reserved for future const-eval"
                    ),
                });
                tys.intern(TyKind::Array(elem_id, len_opt))
            }
            HirTyKind::Error => tys.error,
        }
    }

    /// Annotation lookup for params and let-bindings during sig resolution.
    /// Currently this is just a `Some` shortcut; let-binding `None`s are
    /// handled in `infer_let` (Phase 2) where fresh Infer vars are allowed.
    fn resolve_annotation(
        tys: &mut TyArena,
        errors: &mut Vec<TypeError>,
        ty: Option<&HirTy>,
        _span: &Span,
    ) -> TyId {
        match ty {
            Some(t) => Self::resolve_ty(tys, errors, t),
            None => tys.error,
        }
    }

    // ---------- inference primitives ----------

    fn fresh_infer(&mut self, inf: &mut Inferer, int_default: bool) -> TyId {
        let id = inf.new_var(int_default);
        self.tys.intern(TyKind::Infer(id))
    }

    /// Like `fresh_infer(false)` but flags the var to default to `()`
    /// at finalize if unbound — used for the elem of an empty array
    /// literal `[]`. See `Inferer.unit_default`.
    fn fresh_infer_with_unit_default(&mut self, inf: &mut Inferer) -> TyId {
        let id = inf.new_var_unit_default();
        self.tys.intern(TyKind::Infer(id))
    }

    /// Walk `Infer` chains until we hit a concrete kind or an unbound var.
    fn resolve(&self, inf: &Inferer, ty: TyId) -> TyId {
        let mut cur = ty;
        loop {
            match self.tys.kind(cur) {
                TyKind::Infer(id) => match inf.bindings.get(*id).copied().flatten() {
                    Some(bound) => cur = bound,
                    None => return cur,
                },
                _ => return cur,
            }
        }
    }

    /// Walk through outer `Ptr` layers (auto-deref) until we hit a
    /// non-`Ptr` type. Returns `(peeled_ty, innermost_ptr_mut)`.
    /// `innermost_ptr_mut` is `None` when the input was already a
    /// non-`Ptr` (no auto-deref happened), `Some(m)` when at least
    /// one `Ptr` was peeled — `m` is the mut of the *innermost* `Ptr`
    /// (the one directly above the underlying type). The innermost
    /// pointer is the one that addresses the actual storage; its mut
    /// determines whether the resulting place is writable.
    ///
    /// Used by Index typing, Field access, and `place_mutability` to
    /// enable `p[i]` / `s.a` for `p: *const [T; N]` / `s: *mut Struct`
    /// (and arbitrarily-deep nestings like `*const *mut [T; N]`).
    ///
    /// Explicit `*p` deref is also available now per spec/07_POINTER.md
    /// §Deref operator — `(*p)[i]` / `(*p).a` are valid alternatives
    /// to the auto-deref forms. The two coexist: auto-deref keeps
    /// `p.x` / `p[i]` ergonomic, while explicit `*p` is the canonical
    /// rvalue/lvalue form. The longer-term plan (spec/07 §Pre-existing
    /// codegen gap) is a HIR-rewrite pass inserting explicit `Deref`
    /// nodes, after which this helper retires; not in scope here.
    /// See spec/09_ARRAY.md.
    fn auto_deref_ptr(&self, ty: TyId) -> (TyId, Option<Mutability>) {
        let mut cur = ty;
        let mut innermost_mut: Option<Mutability> = None;
        loop {
            match self.tys.kind(cur) {
                TyKind::Ptr(pointee, m) => {
                    innermost_mut = Some(*m);
                    cur = *pointee;
                }
                _ => return (cur, innermost_mut),
            }
        }
    }

    /// After finalize defaults all Infer vars, fully substitute through
    /// the type tree so no `Infer(_)` leaks into the result tables.
    fn resolve_fully(&mut self, inf: &Inferer, ty: TyId) -> TyId {
        let resolved = self.resolve(inf, ty);
        match self.tys.kind(resolved).clone() {
            TyKind::Infer(_) => self.tys.error, // shouldn't happen post-finalize
            TyKind::Fn(params, ret) => {
                let params: Vec<_> = params.iter().map(|&p| self.resolve_fully(inf, p)).collect();
                let ret = self.resolve_fully(inf, ret);
                self.tys.intern(TyKind::Fn(params, ret))
            }
            TyKind::Ptr(inner, m) => {
                let inner = self.resolve_fully(inf, inner);
                self.tys.intern(TyKind::Ptr(inner, m))
            }
            TyKind::Array(elem, len) => {
                let elem = self.resolve_fully(inf, elem);
                self.tys.intern(TyKind::Array(elem, len))
            }
            // Adt is identity-only — nothing to substitute.
            TyKind::Adt(_) => resolved,
            _ => resolved,
        }
    }

    /// Symmetric Hindley-Milner unification. The two type arguments are
    /// algebraically interchangeable — there is no subtyping. We retain
    /// the parameter names `found` / `expected` only because the emitted
    /// `TypeMismatch` diagnostic renders them with those labels; at most
    /// call sites the labels are a presentation choice with no semantic
    /// weight. For sites where direction *does* matter (Never absorbs,
    /// pointer-mutability subtype `*mut → *const`), use `coerce` instead.
    ///
    /// Concretely: `Never` unifies only with `Never` here. Anything else
    /// against `Never` is a mismatch. The "expression-of-type-`!`-can-flow-
    /// anywhere" rule lives in `coerce`, not in `unify`.
    fn unify(&mut self, inf: &mut Inferer, found: TyId, expected: TyId, span: Span) {
        self.unify_with_ctx(inf, found, expected, span, UnifyContext::default_ctx());
    }

    /// Single-body unify with a `MismatchCtx` that controls how the
    /// terminal-mismatch diagnostic is built. See `MismatchCtx`. Top-level
    /// entry point — `pointee` defaults to `false`.
    fn unify_with(
        &mut self,
        inf: &mut Inferer,
        found: TyId,
        expected: TyId,
        span: Span,
        ctx: MismatchCtx,
    ) {
        self.unify_with_ctx(inf, found, expected, span, UnifyContext::from_mismatch(ctx));
    }

    /// Internal unify body that threads a full `UnifyContext`. Direct
    /// callers should use `unify` or `unify_with`; the only place the
    /// pointee flag flips on is the Ptr-Ptr arm inside this body.
    /// See `UnifyContext` and spec/07_POINTER.md / spec/09_ARRAY.md
    /// "Coercions".
    fn unify_with_ctx(
        &mut self,
        inf: &mut Inferer,
        found: TyId,
        expected: TyId,
        span: Span,
        ctx: UnifyContext,
    ) {
        let found = self.resolve(inf, found);
        let expected = self.resolve(inf, expected);
        if found == expected {
            return;
        }
        let kf = self.tys.kind(found).clone();
        let ke = self.tys.kind(expected).clone();
        match (kf, ke) {
            (TyKind::Error, _) | (_, TyKind::Error) => {}
            (TyKind::Never, TyKind::Never) => {}
            (TyKind::Infer(id), other) => self.bind_infer_checked(inf, id, expected, &other, span),
            (other, TyKind::Infer(id)) => self.bind_infer_checked(inf, id, found, &other, span),
            (TyKind::Prim(p), TyKind::Prim(q)) if p == q => {}
            (TyKind::Unit, TyKind::Unit) => {}
            (TyKind::Fn(params_f, ret_f), TyKind::Fn(params_e, ret_e)) => {
                if params_f.len() != params_e.len() {
                    inf.errors
                        .push(build_mismatch(ctx.mismatch, expected, found, span));
                    return;
                }
                for (pf, pe) in params_f.iter().zip(&params_e) {
                    self.unify_with_ctx(inf, *pf, *pe, span.clone(), ctx);
                }
                self.unify_with_ctx(inf, ret_f, ret_e, span, ctx);
            }
            // Loose on mutability — unify is shape-only on mut (per
            // spec/07_POINTER.md §3). The mutability subtype rule
            // (`*mut → *const` outer, exact match inner) is enforced by
            // `coerce`'s discharge at use sites. The Ptr-Ptr arm sets
            // `pointee=true` on the recursion: this is the ONLY place
            // it flips on; from here it's sticky through deeper
            // recursions, enabling the gated length-erasure relaxation
            // in the Array-Array arm below.
            (TyKind::Ptr(fi, _), TyKind::Ptr(ei, _)) => {
                self.unify_with_ctx(inf, fi, ei, span, ctx.under_ptr());
            }
            // Array-Array: recurse on elem (strict HM); length is gated.
            // - Same length: OK.
            // - Different concrete lengths: E0265.
            // - Mixed Some/None: silent ONLY when `pointee=true` AND
            //   forward direction (found `Some`, expected `None` — the
            //   sound length-erasure direction). Top-level mixed and
            //   reverse-direction mixed both error eagerly here, which
            //   closes the mixed-direction silent-pass at `unify_arms`
            //   sites and gives a sharper diagnostic at `coerce` sites
            //   (vs. today's discharge-time `TypeMismatch`).
            //   See spec/09_ARRAY.md "Coercions" for the rule.
            (TyKind::Array(fe, fc), TyKind::Array(ee, ec)) => {
                self.unify_with_ctx(inf, fe, ee, span.clone(), ctx);
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
                    // Length erasure forward: silent only behind a pointer.
                    (Some(_), None) if ctx.pointee => {}
                    // Top-level Some↔None or reverse direction (None→Some).
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
                // Catch-all mismatch. Includes Adt-vs-Adt with unequal `AdtId`
                // (ADTs unify by pure nominal identity — see spec/08_ADT.md
                // "Unification"; equal ADTs are absorbed by the `found == expected`
                // short-circuit above).
                inf.errors
                    .push(build_mismatch(ctx.mismatch, expected, found, span));
            }
        }
    }

    /// Use-site coercion. Splits into two halves:
    ///
    /// 1. **Eager unify body.** Runs `unify(actual, expected)` immediately
    ///    so type information propagates through the union-find as the
    ///    walk continues — `unify` is permissive on outer Ptr mut bits
    ///    (see check.rs:405) so it computes structural equivalence even
    ///    when mutabilities differ. `Never`/`Error` actuals absorb here
    ///    without engaging unify (`unify(!, T)` would mismatch).
    /// 2. **Deferred check obligation.** Enqueues `Obligation::Coerce`
    ///    so the directional `*mut → *const` rule and inner strict
    ///    mut-equality fire at finalize, against fully-resolved types.
    ///    See spec/05_TYPE_CHECKER.md "Obligations" and
    ///    spec/07_POINTER.md.
    ///
    /// `expect_unit` used to live nearby — that role is now subsumed by
    /// `coerce(ty, Unit)`: there's no Ptr-Ptr branch to fire when the
    /// expected side is `Unit`, so the obligation discharge is a no-op
    /// and the eager unify enforces the constraint.
    fn coerce(&mut self, inf: &mut Inferer, actual: TyId, expected: TyId, span: Span) {
        let resolved_a = self.resolve(inf, actual);
        if let TyKind::Never | TyKind::Error = self.tys.kind(resolved_a) {
            return;
        }
        self.unify(inf, actual, expected, span.clone());
        // Skip the obligation when neither side can participate in a
        // directional coercion check (today only Ptr-Ptr mut-compat;
        // future variance rules would extend this predicate). Common
        // skip case: `coerce(_, Unit)` from former `expect_unit` sites
        // and primitive-targeted let-init / call-arg paths.
        if !self.is_coercible(inf, actual) || !self.is_coercible(inf, expected) {
            return;
        }
        inf.obligations.push(Obligation::Coerce {
            actual,
            expected,
            span,
        });
    }

    /// Is `ty` a kind that could participate in a non-trivial coercion
    /// (one that requires a directional check beyond what `unify`
    /// already enforces)? Today: pointers (mut-compat at outer; strict
    /// equality at inner). `Infer` is included because it might still
    /// resolve to a pointer.
    fn is_coercible(&self, inf: &Inferer, ty: TyId) -> bool {
        matches!(
            self.tys.kind(self.resolve(inf, ty)),
            TyKind::Ptr(_, _) | TyKind::Infer(_)
        )
    }

    /// Bind an Infer var to a concrete type, but reject if doing so would
    /// violate the var's `int_default` constraint (i.e., int-flagged var
    /// being unified with a non-integer concrete type).
    fn bind_infer_checked(
        &mut self,
        inf: &mut Inferer,
        id: InferId,
        target: TyId,
        target_kind: &TyKind,
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
                let infer_ty = self.tys.intern(TyKind::Infer(id));
                inf.errors.push(TypeError::TypeMismatch {
                    expected: target,
                    found: infer_ty,
                    span,
                });
                // Bind to i32 (the int-flagged var's natural default)
                // rather than `error`. The mismatch error has already
                // been pushed; binding to the default lets the captured
                // `found: Infer(id)` resolve to `i32` for the renderer
                // and lets sibling expressions typed by this infer var
                // surface as `i32` in the types table — which matches
                // what the user wrote.
                inf.bind(id, self.tys.i32);
                return;
            }
        }
        inf.bind(id, target);
    }

    // ---------- walk ----------

    /// Block typing — two pieces:
    ///
    /// 1. **Mid-block `;`-enforcement.** A non-last item with
    ///    `has_semi == false` must coerce to `()`. Unify against unit:
    ///    `()` matches, `!` is absorbed by the Never arm, anything else
    ///    fires E0250 on the missing-`;` expression.
    ///
    ///  - `{ { 1 } 'a' }` → won't work because `{ 1 }` is `i32`, not `()`.
    ///  - `{ { return 1 } 'a' }` → works because `{ return 1 }` is `!`, coerces vacuously to `()`.
    ///
    /// 2. **Block's value type.** The last item's expression type *wins*
    ///    when either:
    ///    - the user wrote it as a tail (`has_semi == false`), OR
    ///    - the expression itself is divergent (`!`). A trailing `;`
    ///      cannot "discard" a divergent expression — there's no
    ///      implicit unit to reach.
    ///
    ///    Otherwise the block's value type is `()` (the implicit unit
    ///    after the trailing `;`).
    ///
    /// The fn-return check (`check_fn`) coerces this value type against
    /// the declared return type. The Never arm of `unify` makes
    /// divergent bodies vacuously match any declared return — no CFG,
    /// no diverges flag, no propagation from sub-blocks. Concretely:
    ///
    /// - `{ return 1; }` → last expr `return 1` is `!`, value = `!`,
    ///   coerce vacuous.
    /// - `{ g(); }` where `g(): !` → same shape, value = `!`.
    /// - `{ 1; }` → last expr `1` is `i32` (not `!`), value = `()`,
    ///   coerce(`()`, declared) errors for non-unit returns
    /// - `{ { return 1 } "a" }` → last expr `"a"` is `*const [u8; 2]`,
    ///   value = `*const [u8; 2]`, coerce against `i32` errors.
    fn infer_block(&mut self, inf: &mut Inferer, bid: HBlockId) -> TyId {
        let block = self.hir.blocks[bid].clone();
        let last_idx = block.items.len().checked_sub(1);
        for (i, item) in block.items.iter().enumerate() {
            let ty = self.infer_expr(inf, item.expr);
            if Some(i) != last_idx && !item.has_semi {
                let span = self.hir.exprs[item.expr].span.clone();
                let unit = self.tys.unit;
                self.coerce(inf, ty, unit, span);
            }
        }

        match block.items.last() {
            Some(it) => {
                let expr_ty = self.expr_tys[it.expr];
                let resolved = self.resolve(inf, expr_ty);
                let is_never = matches!(self.tys.kind(resolved), TyKind::Never);
                if !it.has_semi || is_never {
                    expr_ty
                } else {
                    self.tys.unit
                }
            }
            None => self.tys.unit,
        }
    }

    fn infer_expr(&mut self, inf: &mut Inferer, eid: HExprId) -> TyId {
        let expr: &HirExpr = &self.hir.exprs[eid];
        let span = expr.span.clone();
        let ty = match expr.kind.clone() {
            HirExprKind::IntLit(_) => self.fresh_infer(inf, true),
            HirExprKind::BoolLit(_) => self.tys.bool,
            HirExprKind::CharLit(_) => self.tys.u8,
            HirExprKind::StrLit(s) => {
                // C-style string literal: `*const [u8; N]` where
                // `N = byte_len + 1` (the trailing NUL is appended by
                // codegen and counted in the type, matching C
                // `char[N]` for `"hello"` → `char[6]`). Pointer-to-
                // sized-array form encodes immutability structurally
                // via the outer `*const` (a bare `[u8; N]` place
                // would let `let mut s = "hi";` mutate). See
                // spec/07_POINTER.md §4.
                let n = (s.as_bytes().len() + 1) as u64;
                let u8_ty = self.tys.u8;
                let arr_ty = self.tys.intern(TyKind::Array(u8_ty, Some(n)));
                self.tys.intern(TyKind::Ptr(arr_ty, Mutability::Const))
            }
            HirExprKind::Null => {
                // Per spec/07_POINTER.md §Null literal "Typeck changes":
                // fresh α (per `null` expression), wrap as `*mut α`. The
                // outer `Mut` is load-bearing — coerce permits `*mut →
                // *const` at the outer layer, so this lets `null` flow
                // freely into both `*const T` and `*mut T` slots. α
                // gets pinned by the use site via the existing
                // loose-unify rule.
                let alpha = self.fresh_infer(inf, false);
                self.tys.intern(TyKind::Ptr(alpha, Mutability::Mut))
            }
            HirExprKind::Local(lid) => self.local_tys[lid],
            HirExprKind::Fn(fid) => {
                let sig = self.fn_sigs[fid].clone();
                self.tys.intern(TyKind::Fn(sig.params, sig.ret))
            }
            HirExprKind::Unresolved(_) => self.tys.error,
            HirExprKind::Unary { op, expr: inner } => self.infer_unary(inf, op, inner, &span),
            HirExprKind::Binary { op, lhs, rhs } => self.infer_binary(inf, op, lhs, rhs, &span),
            HirExprKind::Assign { op, target, rhs } => {
                self.infer_assign(inf, op, target, rhs, &span)
            }
            HirExprKind::Call { callee, args } => self.infer_call(inf, callee, args, &span),
            HirExprKind::Index { base, index } => {
                let base_ty = self.infer_expr(inf, base);
                let idx_ty = self.infer_expr(inf, index);

                // Strict-usize for index. Eager unify_with binds Infer
                // int-flagged vars to usize via bind_infer_checked
                // (default IntLit indices type cleanly), and emits
                // E0267 directly on a concrete-non-usize-int mismatch
                // instead of the generic E0250.
                let usize_ty = self.tys.usize;
                self.unify_with(
                    inf,
                    idx_ty,
                    usize_ty,
                    span.clone(),
                    MismatchCtx::IndexNotUsize,
                );

                let base_resolved = self.resolve(inf, base_ty);
                let (peeled, _ptr_mut) = self.auto_deref_ptr(base_resolved);
                match self.tys.kind(peeled).clone() {
                    TyKind::Array(elem, _) => elem,
                    TyKind::Error | TyKind::Infer(_) => self.tys.error,
                    _ => {
                        inf.errors.push(TypeError::NotIndexable {
                            ty: base_resolved,
                            span: span.clone(),
                        });
                        self.tys.error
                    }
                }
            }
            HirExprKind::Field { base, name } => self.infer_field(inf, base, &name, &span),
            HirExprKind::StructLit { adt, fields } => {
                let aid = AdtId::from_raw(adt.raw());
                self.infer_struct_lit(inf, aid, &fields, &span)
            }
            HirExprKind::Cast { expr: inner, ty } => {
                let _ = self.infer_expr(inf, inner);
                Self::resolve_ty(&mut self.tys, &mut inf.errors, &ty)
            }
            HirExprKind::AddrOf {
                mutability,
                expr: inner,
            } => self.infer_addr_of(inf, mutability, inner),
            HirExprKind::If {
                cond,
                then_block,
                else_arm,
            } => self.infer_if(inf, cond, then_block, else_arm, &span),
            HirExprKind::Block(bid) => self.infer_block(inf, bid),
            HirExprKind::Return(val) => {
                let cur_ret = inf.cur_ret;
                if let Some(v) = val {
                    let v_ty = self.infer_expr(inf, v);
                    let v_span = self.hir.exprs[v].span.clone();
                    self.coerce(inf, v_ty, cur_ret, v_span);
                } else {
                    let unit = self.tys.unit;
                    self.coerce(inf, unit, cur_ret, span.clone());
                }
                self.tys.never
            }
            HirExprKind::Let { local, init } => self.infer_let(inf, local, init, &span),
            HirExprKind::Poison => self.tys.error,
            HirExprKind::ArrayLit(lit) => self.infer_array_lit(inf, lit, &span),
            // `source` is destructured but ignored — the typing rule is
            // structural (driven by `cond.is_some()` and `has_break`),
            // not source-driven. See spec/13_LOOPS.md.
            HirExprKind::Loop {
                init,
                cond,
                update,
                body,
                has_break,
                source: _,
            } => self.infer_loop(inf, init, cond, update, body, has_break),
            HirExprKind::Break { expr } => self.infer_break_expr(inf, expr, &span),
            HirExprKind::Continue => self.infer_continue_expr(inf, &span),
        };
        self.expr_tys[eid] = ty;
        ty
    }

    /// Type a struct literal `Foo { f1: e1, f2: e2, ... }`. Per spec/08_ADT.md
    /// "TBD-T6": validate the field set (no unknown / no duplicate / nothing
    /// missing) and unify each value with the declared field type. Result
    /// is `Adt(aid)` regardless of any per-field errors so cascades stay
    /// typed (`Error` absorbs at the field level).
    fn infer_struct_lit(
        &mut self,
        inf: &mut Inferer,
        aid: AdtId,
        fields: &[HirStructLitField],
        lit_span: &Span,
    ) -> TyId {
        let result_ty = self.tys.intern(TyKind::Adt(aid));

        // Snapshot the declared fields so we don't hold a borrow on
        // `self.adts` while inferring sub-expressions (which may mutably
        // touch `self.tys`/`self.errors`).
        let adt_def = self.adts[aid].clone();
        let declared = &adt_def.variants[VariantIdx::from_raw(0)].fields;

        // Track first occurrences for duplicate-detection and to exclude
        // already-seen names from the missing-field check.
        let mut seen: std::collections::HashMap<String, Span> = std::collections::HashMap::new();

        for provided in fields {
            // Type-check the value first so inner errors still surface
            // even if the field-set check fails.
            let value_ty = self.infer_expr(inf, provided.value);
            let value_span = self.hir.exprs[provided.value].span.clone();

            if let Some(first_span) = seen.get(&provided.name) {
                inf.errors.push(TypeError::StructLitDuplicateField {
                    field: provided.name.clone(),
                    first: first_span.clone(),
                    dup: provided.span.clone(),
                });
                // Don't unify the second occurrence — its target slot is
                // already accounted for; treat the second as a free-floating
                // expression for diagnostic purposes only.
                continue;
            }
            seen.insert(provided.name.clone(), provided.span.clone());

            match declared.iter().find(|f| f.name == provided.name) {
                Some(field_def) => {
                    self.coerce(inf, value_ty, field_def.ty, value_span);
                }
                None => {
                    inf.errors.push(TypeError::StructLitUnknownField {
                        field: provided.name.clone(),
                        adt: adt_def.name.clone(),
                        span: provided.span.clone(),
                    });
                }
            }
        }

        for declared_field in declared.iter() {
            if !seen.contains_key(&declared_field.name) {
                inf.errors.push(TypeError::StructLitMissingField {
                    field: declared_field.name.clone(),
                    adt: adt_def.name.clone(),
                    lit_span: lit_span.clone(),
                });
            }
        }

        result_ty
    }

    /// Type `base.name` as a value (rvalue). Place-vs-value distinction is
    /// already in HIR (`HirExpr::is_place`); this rule only inspects the
    /// type of `base`. Per spec/08_ADT.md "TBD-T6" + spec/09_ARRAY.md
    /// auto-deref:
    ///
    ///   - `base` auto-derefs through any number of outer `Ptr` layers
    ///     (`s.a` works for `s: *const Struct`, `*const *mut Struct`, etc.).
    ///   - After auto-deref, `Adt(aid)` — look up the field, return its
    ///     type. Unknown name → `NoFieldOnAdt`, return `error`.
    ///   - After auto-deref, `Infer(_)` — receiver type unresolved.
    ///     `CannotInfer`, return `error`.
    ///   - `base: Never` — propagate `Never`.
    ///   - `base: Error` — propagate `Error` silently.
    ///   - anything else (Prim/Unit/Fn/Array/...) — `TypeNotFieldable`.
    fn infer_field(&mut self, inf: &mut Inferer, base: HExprId, name: &str, span: &Span) -> TyId {
        let base_ty = self.infer_expr(inf, base);
        let resolved = self.resolve(inf, base_ty);
        let (peeled, _ptr_mut) = self.auto_deref_ptr(resolved);
        match self.tys.kind(peeled).clone() {
            TyKind::Adt(aid) => {
                let adt_def = &self.adts[aid];
                match adt_def.variants[VariantIdx::from_raw(0)]
                    .fields
                    .iter()
                    .find(|f| f.name == name)
                {
                    Some(field_def) => field_def.ty,
                    None => {
                        inf.errors.push(TypeError::NoFieldOnAdt {
                            field: name.to_string(),
                            adt: adt_def.name.clone(),
                            span: span.clone(),
                        });
                        self.tys.error
                    }
                }
            }
            TyKind::Infer(_) => {
                inf.errors
                    .push(TypeError::CannotInfer { span: span.clone() });
                self.tys.error
            }
            TyKind::Never => self.tys.never,
            TyKind::Error => self.tys.error,
            TyKind::Prim(_) | TyKind::Unit | TyKind::Fn(_, _) | TyKind::Array(_, _) => {
                inf.errors.push(TypeError::TypeNotFieldable {
                    ty: resolved,
                    span: span.clone(),
                });
                self.tys.error
            }
            // After auto_deref_ptr, `peeled` is never Ptr — but the
            // exhaustiveness checker insists on the arm.
            TyKind::Ptr(_, _) => unreachable!("auto_deref_ptr drains Ptr layers"),
        }
    }

    /// Type an array literal expression. See spec/09_ARRAY.md
    /// "Array literal typing rule".
    ///
    /// - Empty `[]` — fresh elem var (unit-defaulted; see
    ///   `fresh_infer_with_unit_default`); length `0`. Context (e.g.
    ///   `let a: [i32; 0] = []`) binds `?T → i32`; without context,
    ///   finalize defaults `?T → ()` for `[(); 0]`.
    /// - Elems list — first element's type is the canonical elem
    ///   type; subsequent elements unify against it via
    ///   `MismatchCtx::ArrayLitElement` so a mismatch reports E0268
    ///   instead of the generic E0250.
    /// - Repeat `[init; N]` — elem type is `init`'s type; length is
    ///   the parser-extracted `HirConst::Lit(n)` (Error variant is
    ///   unreachable in v0).
    fn infer_array_lit(&mut self, inf: &mut Inferer, lit: HirArrayLit, _span: &Span) -> TyId {
        match lit {
            HirArrayLit::Elems(es) if es.is_empty() => {
                let elem = self.fresh_infer_with_unit_default(inf);
                self.tys.intern(TyKind::Array(elem, Some(0)))
            }
            HirArrayLit::Elems(es) => {
                let n = es.len() as u64;
                let t0 = self.infer_expr(inf, es[0]);
                for (i, &eid) in es.iter().enumerate().skip(1) {
                    let ti = self.infer_expr(inf, eid);
                    let elem_span = self.hir.exprs[eid].span.clone();
                    self.unify_with(inf, ti, t0, elem_span, MismatchCtx::ArrayLitElement { i });
                }
                self.tys.intern(TyKind::Array(t0, Some(n)))
            }
            HirArrayLit::Repeat { init, len } => {
                let t = self.infer_expr(inf, init);
                let n = match len {
                    HirConst::Lit(n) => n,
                    HirConst::Error => unreachable!(
                        "parser+lower guarantee Lit; HirConst::Error reserved for future const-eval"
                    ),
                };
                self.tys.intern(TyKind::Array(t, Some(n)))
            }
        }
    }

    fn infer_unary(&mut self, inf: &mut Inferer, op: UnOp, inner: HExprId, _span: &Span) -> TyId {
        let t = self.infer_expr(inf, inner);
        match op {
            UnOp::Neg | UnOp::BitNot => t, // numeric / integer (typeck v0 trusts; codegen checks)
            UnOp::Not => {
                let span = self.hir.exprs[inner].span.clone();
                let bool_ty = self.tys.bool;
                self.unify(inf, t, bool_ty, span);
                bool_ty
            }
            UnOp::Deref => {
                // Per spec/07_POINTER.md §Deref operator "Typeck changes":
                // operand must resolve to `Ptr(T, _)`; result is the
                // pointee. Sized-pointee check is handled via the
                // existing `Sized` obligation queue (E0269 with
                // `SizedPos::Deref`), so `*p` on `*const [T]` rejects
                // at finalize through the same path as fn-param /
                // let-binding sites — including infer-flowed cases
                // where pointee α only resolves to `Array(_, None)`
                // later. Non-pointer operand fires E0270 immediately
                // since the result type is poison-bounded.
                // Span is the operand's, matching `infer_field`'s
                // precedent — the type that's wrong sits there.
                let resolved = self.resolve(inf, t);
                match self.tys.kind(resolved).clone() {
                    TyKind::Ptr(pointee, _) => {
                        let span = self.hir.exprs[inner].span.clone();
                        inf.obligations.push(Obligation::Sized {
                            ty: pointee,
                            pos: SizedPos::Deref,
                            span,
                        });
                        pointee
                    }
                    TyKind::Error => self.tys.error,
                    TyKind::Infer(_) => {
                        let span = self.hir.exprs[inner].span.clone();
                        inf.errors.push(TypeError::CannotInfer { span });
                        self.tys.error
                    }
                    _ => {
                        let span = self.hir.exprs[inner].span.clone();
                        inf.errors.push(TypeError::DerefNonPointer {
                            found: resolved,
                            span,
                        });
                        self.tys.error
                    }
                }
            }
        }
    }

    fn infer_binary(
        &mut self,
        inf: &mut Inferer,
        op: BinOp,
        lhs: HExprId,
        rhs: HExprId,
        span: &Span,
    ) -> TyId {
        let lt = self.infer_expr(inf, lhs);
        let rt = self.infer_expr(inf, rhs);
        let bool_ty = self.tys.bool;
        match op {
            // Arithmetic + bitwise: same type both sides; result = that type.
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Rem
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor => {
                self.unify(inf, lt, rt, span.clone());
                lt
            }
            // Comparisons: same type both sides; result = bool.
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                self.unify(inf, lt, rt, span.clone());
                bool_ty
            }
            // Logical: both sides bool; result = bool.
            BinOp::And | BinOp::Or => {
                self.unify(inf, lt, bool_ty, self.hir.exprs[lhs].span.clone());
                self.unify(inf, rt, bool_ty, self.hir.exprs[rhs].span.clone());
                bool_ty
            }
            // Shifts: lhs's type is the result; rhs is any integer (loosely).
            BinOp::Shl | BinOp::Shr => lt,
        }
    }

    fn infer_assign(
        &mut self,
        inf: &mut Inferer,
        _op: AssignOp,
        target: HExprId,
        rhs: HExprId,
        span: &Span,
    ) -> TyId {
        let t = self.infer_expr(inf, target);
        let r = self.infer_expr(inf, rhs);
        // RHS coerces *to* the LHS slot — direction matters for pointer
        // mutability (`*mut → *const` OK, reverse is not).
        self.coerce(inf, r, t, span.clone());
        // Mutability of the target. `None` means "not a place" — HIR has
        // already filed `InvalidAssignTarget`, so we don't double-report.
        // See spec/10_ADDRESS_OF.md "Mutability check for `&mut`".
        if let Some(Mutability::Const) = self.place_mutability(target) {
            let target_span = self.hir.exprs[target].span.clone();
            inf.errors.push(TypeError::MutateImmutable {
                op: MutateOp::Assign,
                span: target_span,
            });
        }
        self.tys.unit
    }

    /// `&expr` / `&mut expr`. The operand was already validated to be a
    /// place at HIR-lower time (`AddrOfNonPlace` if not). Here we only
    /// need to type the operand and, for `&mut`, ensure the place is
    /// mutable. See spec/10_ADDRESS_OF.md "Type rule" / "Mutability check".
    fn infer_addr_of(&mut self, inf: &mut Inferer, mutability: Mutability, expr: HExprId) -> TyId {
        let inner_ty = self.infer_expr(inf, expr);
        if let Mutability::Mut = mutability {
            // None ⇒ HIR already filed `AddrOfNonPlace`; suppress.
            if let Some(Mutability::Const) = self.place_mutability(expr) {
                let span = self.hir.exprs[expr].span.clone();
                inf.errors.push(TypeError::MutateImmutable {
                    op: MutateOp::BorrowMut,
                    span,
                });
            }
        }
        self.tys.intern(TyKind::Ptr(inner_ty, mutability))
    }

    /// Walk the place expression tree, returning the root's mutability.
    /// `None` for non-places — typeck callers treat `None` as "no error
    /// here, HIR already reported InvalidAssignTarget / AddrOfNonPlace."
    /// `Some(Mut)` / `Some(Const)` for places, where:
    ///   - `Local(lid)` → the local's `mutable` flag.
    ///   - `Field { base, _ }` / `Index { base, _ }` → if `base`'s type
    ///     auto-derefs through at least one `Ptr` (i.e. `s.a` /  `p[i]`
    ///     for `s: *mut Struct` / `p: *const [T; N]`), the **innermost**
    ///     pointer's mut wins (it's the one that addresses the actual
    ///     storage). Otherwise (bare ADT / Array place), inherit from
    ///     `base` recursively.
    ///   - everything else → `None`.
    ///
    /// `Unary { Deref, _ }` joins the place producers under
    /// 07_POINTER §5; its mutability comes from the pointer's type
    /// (`*mut T` → Mut, `*const T` → Const).
    fn place_mutability(&self, eid: HExprId) -> Option<Mutability> {
        match &self.hir.exprs[eid].kind {
            HirExprKind::Local(lid) => Some(if self.hir.locals[*lid].mutable {
                Mutability::Mut
            } else {
                Mutability::Const
            }),
            HirExprKind::Field { base, .. } | HirExprKind::Index { base, .. } => {
                let base_ty = self.expr_tys[*base];
                let (_, ptr_mut) = self.auto_deref_ptr(base_ty);
                match ptr_mut {
                    // ≥1 Ptr peeled: the innermost ptr-mut governs the
                    // resulting place. The base binding's mut is
                    // irrelevant — `let p: *mut [i32; 3]` (immutable
                    // binding, mut pointer) makes `p[i] = x` allowed.
                    Some(m) => Some(m),
                    // Bare ADT / Array place: inherit from base.
                    None => self.place_mutability(*base),
                }
            }
            // `*p` — outer mut of the operand pointer governs.
            // Deliberately ONE peel, not recursive auto_deref_ptr:
            // writing to `*p` modifies the location `p` addresses, so
            // the *outer* mut is what matters. (Compose with Field /
            // Index recursion above and the rules stay consistent —
            // see spec/07_POINTER.md §Subtlety.)
            HirExprKind::Unary {
                op: UnOp::Deref,
                expr: inner,
            } => {
                let inner_ty = self.expr_tys[*inner];
                match self.tys.kind(inner_ty) {
                    TyKind::Ptr(_, m) => Some(*m),
                    // Non-pointer operand — `infer_unary` already
                    // emitted E0270; suppress here.
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn infer_call(
        &mut self,
        inf: &mut Inferer,
        callee: HExprId,
        args: Vec<HExprId>,
        span: &Span,
    ) -> TyId {
        let callee_ty = self.infer_expr(inf, callee);
        let callee_resolved = self.resolve(inf, callee_ty);
        let arg_tys: Vec<TyId> = args.iter().map(|&a| self.infer_expr(inf, a)).collect();
        match self.tys.kind(callee_resolved).clone() {
            TyKind::Fn(param_tys, ret_ty) => {
                if param_tys.len() != args.len() {
                    inf.errors.push(TypeError::WrongArgCount {
                        expected: param_tys.len(),
                        found: args.len(),
                        span: span.clone(),
                    });
                    return ret_ty;
                }
                for ((&aid, &pty), &aty) in args.iter().zip(&param_tys).zip(&arg_tys) {
                    let arg_span = self.hir.exprs[aid].span.clone();
                    self.coerce(inf, aty, pty, arg_span);
                }
                ret_ty
            }
            TyKind::Error => self.tys.error,
            _ => {
                inf.errors.push(TypeError::NotCallable {
                    found: callee_resolved,
                    span: span.clone(),
                });
                self.tys.error
            }
        }
    }

    fn infer_if(
        &mut self,
        inf: &mut Inferer,
        cond: HExprId,
        then_block: HBlockId,
        else_arm: Option<HElseArm>,
        _span: &Span,
    ) -> TyId {
        let cond_ty = self.infer_expr(inf, cond);
        let cond_span = self.hir.exprs[cond].span.clone();
        let bool_ty = self.tys.bool;
        self.unify(inf, cond_ty, bool_ty, cond_span);
        let then_ty = self.infer_block(inf, then_block);
        match else_arm {
            None => {
                // No else: then-arm is in tail-discard position — must be
                // `()` (or `!`/error). Routed through `coerce` (the
                // unit-position rule is `coerce(_, Unit)`), which handles
                // Never absorption and unifies the int-flagged Infer case
                // through `bind_infer_checked`.
                let span = self.hir.blocks[then_block].span.clone();
                let unit = self.tys.unit;
                self.coerce(inf, then_ty, unit, span);
                self.tys.unit
            }
            Some(HElseArm::Block(bid)) => {
                let else_ty = self.infer_block(inf, bid);
                let span = self.hir.blocks[bid].span.clone();
                self.unify_arms(inf, then_ty, else_ty, span);
                self.join_never(inf, then_ty, else_ty)
            }
            Some(HElseArm::If(eid)) => {
                let else_ty = self.infer_expr(inf, eid);
                let span = self.hir.exprs[eid].span.clone();
                self.unify_arms(inf, then_ty, else_ty, span);
                self.join_never(inf, then_ty, else_ty)
            }
        }
    }

    /// Unified `while` / `loop` / `for` typing. Headers (`init`, `cond`,
    /// `update`) are typed for side-effects; only `cond` carries a
    /// constraint (must coerce to `bool`). The loop expression's value
    /// type is decided **structurally** — by whether `cond` is present
    /// and whether the body holds a `break` — not by the `LoopSource`
    /// tag. Per spec/13_LOOPS.md "Typing rule is structural".
    fn infer_loop(
        &mut self,
        inf: &mut Inferer,
        init: Option<HExprId>,
        cond: Option<HExprId>,
        update: Option<HExprId>,
        body: HBlockId,
        has_break: bool,
    ) -> TyId {
        // Header slots are typed for side-effects (and so any errors
        // inside them surface), but their values are discarded — `init`
        // is typically `Let`, `update` typically `Assign`, both `()`.
        // `cond` is the one with a real constraint.
        if let Some(i) = init {
            let _ = self.infer_expr(inf, i);
        }
        if let Some(c) = cond {
            let cond_ty = self.infer_expr(inf, c);
            let cond_span = self.hir.exprs[c].span.clone();
            let bool_ty = self.tys.bool;
            self.unify(inf, cond_ty, bool_ty, cond_span);
        }
        if let Some(u) = update {
            let _ = self.infer_expr(inf, u);
        }

        // Structural rule:
        //   cond.is_some()  ⇒ unit       (loop can fall out the bottom)
        //   no cond, no break ⇒ never    (truly infinite)
        //   no cond, has break ⇒ fresh   (break-driven; binds to first
        //                                 valued break, then unifies)
        let target = if cond.is_some() {
            self.tys.unit
        } else if has_break {
            self.fresh_infer(inf, false)
        } else {
            self.tys.never
        };

        inf.loop_tys.push(target);
        let body_ty = self.infer_block(inf, body);
        inf.loop_tys.pop();

        // Body must be statement-shaped — same constraint
        // `if`-without-`else` enforces on its then-arm. `coerce` handles
        // `!` short-circuit; an `i32` tail emits TypeMismatch (E0250).
        let body_span = self.hir.blocks[body].span.clone();
        let unit = self.tys.unit;
        self.coerce(inf, body_ty, unit, body_span);

        target
    }

    /// `break expr?` — coerce the operand (or `()` if elided) into the
    /// innermost loop's target slot, return `!`. Mirrors `Return`'s
    /// shape. The operand's span is the coerce site so type-mismatch
    /// errors point at the value, not the `break` keyword.
    fn infer_break_expr(&mut self, inf: &mut Inferer, expr: Option<HExprId>, span: &Span) -> TyId {
        let target = *inf
            .loop_tys
            .last()
            .expect("HIR enforced break is inside a loop");
        let (operand_ty, operand_span) = match expr {
            Some(e) => (self.infer_expr(inf, e), self.hir.exprs[e].span.clone()),
            None => (self.tys.unit, span.clone()),
        };
        self.coerce(inf, operand_ty, target, operand_span);
        self.tys.never
    }

    /// `continue` — diverges, no operand. The `last()` check turns a
    /// silent stack-discipline bug into a loud panic; HIR-lower already
    /// filed E0264 for `continue` outside any loop.
    fn infer_continue_expr(&mut self, inf: &mut Inferer, _span: &Span) -> TyId {
        let _ = inf
            .loop_tys
            .last()
            .expect("HIR enforced continue is inside a loop");
        self.tys.never
    }

    /// Unify two `if`-arm types. Symmetric — neither arm is "the
    /// expected." Special-case for `Never`: if either arm diverges,
    /// skip unification entirely. The non-divergent arm decides the
    /// if-expr's type via `join_never`; the divergent arm contributes
    /// no usable type, so demanding equality with it would spuriously
    /// reject `if c { return 1 } else { 0 }`. (The Never-absorbs rule
    /// belongs in `coerce`, but this is the symmetric-join analogue.)
    fn unify_arms(&mut self, inf: &mut Inferer, a: TyId, b: TyId, span: Span) {
        let ar = self.resolve(inf, a);
        let br = self.resolve(inf, b);
        let a_never = matches!(self.tys.kind(ar), TyKind::Never);
        let b_never = matches!(self.tys.kind(br), TyKind::Never);
        if a_never || b_never {
            return;
        }
        self.unify(inf, a, b, span);
    }

    /// After unifying two arm types, pick the one that *isn't* `!`.
    /// `unify_arms` skips Never sides, so they remain distinct from the
    /// non-divergent arm. The if-expr's actual type is the non-divergent
    /// arm's type (Never absorbs). Without this, an
    /// `if c { return 1 } else { 0 }` would be typed `!` if the then
    /// arm came first, instead of `i32`.
    fn join_never(&self, inf: &Inferer, a: TyId, b: TyId) -> TyId {
        let ar = self.resolve(inf, a);
        if let TyKind::Never = self.tys.kind(ar) {
            self.resolve(inf, b)
        } else {
            a
        }
    }

    fn infer_let(
        &mut self,
        inf: &mut Inferer,
        local: LocalId,
        init: Option<HExprId>,
        _span: &Span,
    ) -> TyId {
        let local_data: &HirLocal = &self.hir.locals[local];
        let (local_ty, sized_span) = match &local_data.ty {
            Some(t) => {
                let ty_span = t.span.clone();
                (Self::resolve_ty(&mut self.tys, &mut inf.errors, t), ty_span)
            }
            None => (self.fresh_infer(inf, false), local_data.span.clone()),
        };
        self.local_tys[local] = local_ty;
        // Sized check at let-binding position. Even when the type comes
        // from a fresh Infer (no annotation), enqueue the obligation —
        // discharge resolves through the Inferer at fn finalize. See
        // spec/09_ARRAY.md "E0261" and spec/05_TYPE_CHECKER.md
        // "Obligations".
        inf.obligations.push(Obligation::Sized {
            ty: local_ty,
            pos: SizedPos::LetBinding,
            span: sized_span,
        });
        if let Some(init_id) = init {
            let init_ty = self.infer_expr(inf, init_id);
            let init_span = self.hir.exprs[init_id].span.clone();
            self.coerce(inf, init_ty, local_ty, init_span);
        }
        self.tys.unit
    }
}
