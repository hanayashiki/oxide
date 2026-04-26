//! The type checker. Two phases:
//!   1. Resolve every fn's signature from source annotations (no inference).
//!   2. Check each fn body in isolation with a fresh `Inferer`.
//!
//! State split:
//! - `Checker<'hir>` borrows the HIR for its whole lifetime and owns the
//!   module-scope outputs (`tys`, `fn_sigs`, `local_tys`, `expr_tys`,
//!   `errors`) that survive past any single fn.
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
use index_vec::IndexVec;

use crate::hir::{
    FnId, HBlockId, HElseArm, HExprId, HirBlock, HirExpr, HirExprKind, HirFn, HirLocal, HirModule,
    HirTy, HirTyKind, LocalId,
};
use crate::lexer::Span;
use crate::parser::ast::{AssignOp, BinOp, Mutability, UnOp};

use super::error::TypeError;
use super::ty::{FnSig, InferId, TyArena, TyId, TyKind};

#[derive(Clone, Debug)]
pub struct TypeckResults {
    pub tys: TyArena,
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
        &self.fn_sigs[fid]
    }
    pub fn tys(&self) -> &TyArena {
        &self.tys
    }
}

pub fn check(hir: &HirModule) -> (TypeckResults, Vec<TypeError>) {
    let mut cx = Checker::new(hir);
    cx.resolve_signatures();
    for (fid, _) in hir.fns.iter_enumerated() {
        cx.check_fn(fid);
    }
    cx.finish()
}

struct Checker<'hir> {
    hir: &'hir HirModule,
    tys: TyArena,
    fn_sigs: IndexVec<FnId, FnSig>,
    local_tys: IndexVec<LocalId, TyId>,
    expr_tys: IndexVec<HExprId, TyId>,
    errors: Vec<TypeError>,
}

struct Inferer {
    bindings: IndexVec<InferId, Option<TyId>>,
    int_default: IndexVec<InferId, bool>,
    /// Errors emitted while this fn body was being inferred. TyId fields
    /// inside may still point at unresolved `Infer` vars; `Checker::finalize`
    /// resolves them post-defaulting before flushing into `Checker.errors`.
    errors: Vec<TypeError>,
    /// Expected return type of the fn whose body is being inferred. Read
    /// by the `Return` arm of `infer_expr`; doesn't change for the
    /// lifetime of this Inferer.
    cur_ret: TyId,
}

impl Inferer {
    fn new(cur_ret: TyId) -> Self {
        Self {
            bindings: IndexVec::new(),
            int_default: IndexVec::new(),
            errors: Vec::new(),
            cur_ret,
        }
    }
    fn new_var(&mut self, int_default: bool) -> InferId {
        let id = self.bindings.push(None);
        let _ = self.int_default.push(int_default);
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
            })
            .collect();
        Self {
            hir,
            tys,
            fn_sigs,
            local_tys,
            expr_tys,
            errors: Vec::new(),
        }
    }

    /// Phase 1. Sigs are entirely source-driven — no inference, errors
    /// land directly on `Checker.errors`.
    fn resolve_signatures(&mut self) {
        for (fid, hir_fn) in self.hir.fns.iter_enumerated() {
            let mut params = Vec::with_capacity(hir_fn.params.len());
            for &lid in &hir_fn.params {
                let local = &self.hir.locals[lid];
                let ty = Self::resolve_annotation(
                    &mut self.tys,
                    &mut self.errors,
                    local.ty.as_ref(),
                    &local.span,
                );
                self.local_tys[lid] = ty;
                params.push(ty);
            }
            let ret = match &hir_fn.ret_ty {
                Some(t) => Self::resolve_named_ty(&mut self.tys, &mut self.errors, t),
                None => self.tys.unit, // Rust-style: implicit unit
            };
            self.fn_sigs[fid] = FnSig { params, ret };
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
        // Default unconstrained int vars to i32; bind anything else still
        // unresolved to error (silent — we get implicit "{error}" propagation
        // and any cascading mismatches will already have been reported).
        let i32_id = self.tys.i32;
        let error_id = self.tys.error;
        for raw in 0..inf.bindings.len() {
            let id = InferId::from_raw(raw as u32);
            if inf.bindings[id].is_none() {
                inf.bindings[id] = Some(if inf.int_default[id] {
                    i32_id
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
            TypeError::UnknownType { .. }
            | TypeError::WrongArgCount { .. }
            | TypeError::UnsupportedFeature { .. }
            | TypeError::CannotInfer { .. } => {}
        }
    }

    fn finish(self) -> (TypeckResults, Vec<TypeError>) {
        (
            TypeckResults {
                tys: self.tys,
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
    fn resolve_named_ty(tys: &mut TyArena, errors: &mut Vec<TypeError>, ty: &HirTy) -> TyId {
        // TODO: resolve custom types
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
            HirTyKind::Ptr {
                mutability,
                pointee,
            } => {
                let pointee = Self::resolve_named_ty(tys, errors, pointee);
                tys.intern(TyKind::Ptr(pointee, *mutability))
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
            Some(t) => Self::resolve_named_ty(tys, errors, t),
            None => tys.error,
        }
    }

    // ---------- inference primitives ----------

    fn fresh_infer(&mut self, inf: &mut Inferer, int_default: bool) -> TyId {
        let id = inf.new_var(int_default);
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
            _ => resolved,
        }
    }

    /// Unifier. Convention: `unify(found, expected, span)` — diagnostic
    /// reports `expected: <expected>, found: <found>`.
    fn unify(&mut self, inf: &mut Inferer, found: TyId, expected: TyId, span: Span) {
        let found = self.resolve(inf, found);
        let expected = self.resolve(inf, expected);
        if found == expected {
            return;
        }
        let kf = self.tys.kind(found).clone();
        let ke = self.tys.kind(expected).clone();
        match (kf, ke) {
            (TyKind::Error, _) | (_, TyKind::Error) => {}
            (TyKind::Never, _) | (_, TyKind::Never) => {}
            (TyKind::Infer(id), other) => self.bind_infer_checked(inf, id, expected, &other, span),
            (other, TyKind::Infer(id)) => self.bind_infer_checked(inf, id, found, &other, span),
            (TyKind::Prim(p), TyKind::Prim(q)) if p == q => {}
            (TyKind::Unit, TyKind::Unit) => {}
            (TyKind::Fn(params_f, ret_f), TyKind::Fn(params_e, ret_e)) => {
                if params_f.len() != params_e.len() {
                    inf.errors.push(TypeError::TypeMismatch {
                        expected,
                        found,
                        span,
                    });
                    return;
                }
                for (pf, pe) in params_f.iter().zip(&params_e) {
                    self.unify(inf, *pf, *pe, span.clone());
                }
                self.unify(inf, ret_f, ret_e, span);
            }
            // Loose on mutability — unify is shape-only (per spec/07_POINTER.md).
            // The mutability subtype rule (`*mut → *const` OK at the outer
            // layer, exact match below) is enforced by `coerce` at use sites.
            (TyKind::Ptr(fi, _), TyKind::Ptr(ei, _)) => self.unify(inf, fi, ei, span),
            _ => {
                inf.errors.push(TypeError::TypeMismatch {
                    expected,
                    found,
                    span,
                });
            }
        }
    }

    /// Use-site coercion check. Runs `unify` (shape + inference), then
    /// validates pointer mutability per the subtype rule:
    ///   - outermost: `mut → const` allowed; `const → mut` rejected.
    ///   - every inner position: exact mutability match.
    /// All non-pointer types fall through to plain unify.
    fn coerce(&mut self, inf: &mut Inferer, actual: TyId, expected: TyId, span: Span) {
        self.unify(inf, actual, expected, span.clone());
        self.check_ptr_outer_compat(inf, actual, expected, span);
    }

    /// Outer-layer coercion check. If both sides are pointers, verify
    /// `actual_mut ≤ expected_mut`, then recurse into the pointee with
    /// **strict** mutability equality (`check_ptr_inner_eq`).
    fn check_ptr_outer_compat(
        &mut self,
        inf: &mut Inferer,
        actual: TyId,
        expected: TyId,
        span: Span,
    ) {
        let actual = self.resolve(inf, actual);
        let expected = self.resolve(inf, expected);
        let (a_pt, a_mut, e_pt, e_mut) = match (
            self.tys.kind(actual).clone(),
            self.tys.kind(expected).clone(),
        ) {
            (TyKind::Ptr(ap, am), TyKind::Ptr(ep, em)) => (ap, am, ep, em),
            _ => return,
        };
        if !mut_le(a_mut, e_mut) {
            inf.errors.push(TypeError::PointerMutabilityMismatch {
                expected,
                actual,
                span: span.clone(),
            });
            return;
        }
        self.check_ptr_inner_eq(inf, a_pt, e_pt, span);
    }

    /// Inner positions must match mutability exactly. Used by coerce after
    /// the outer layer has been checked. Shape mismatches under here are
    /// already caught by `unify`, so this method only emits errors for
    /// mutability divergence.
    fn check_ptr_inner_eq(&mut self, inf: &mut Inferer, a: TyId, b: TyId, span: Span) {
        let a = self.resolve(inf, a);
        let b = self.resolve(inf, b);
        if let (TyKind::Ptr(a_pt, a_mut), TyKind::Ptr(b_pt, b_mut)) =
            (self.tys.kind(a).clone(), self.tys.kind(b).clone())
        {
            if a_mut != b_mut {
                inf.errors.push(TypeError::PointerMutabilityMismatch {
                    expected: b,
                    actual: a,
                    span: span.clone(),
                });
                return;
            }
            self.check_ptr_inner_eq(inf, a_pt, b_pt, span);
        }
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
                TyKind::Infer(_) | TyKind::Error | TyKind::Never => true,
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

    /// One-way "this expression's type must be unit at this use site"
    /// check. Resolves and reports a mismatch if the type isn't
    /// `()`/`!`/error. Crucially does **not** unify against unit —
    /// that would bind any leading int-infer var to unit, poisoning
    /// literals inside the expression (`1 + 2` collapsing to `{error}`
    /// instead of `i32`). Unbound int infers stay free here and get
    /// defaulted to `i32` at fn finalize, which is also where the
    /// captured `found` TyId in this error gets resolved for the
    /// renderer.
    ///
    /// Used at: mid-block non-tail items without `;`, and the then-arm
    /// of an else-less `if`. Both are positions where the expression's
    /// value is discarded and the surrounding context demands `()`,
    /// with no two-way flow into inference.
    fn expect_unit(&mut self, inf: &mut Inferer, ty: TyId, span: Span) {
        let resolved = self.resolve(inf, ty);
        match self.tys.kind(resolved) {
            TyKind::Unit | TyKind::Never | TyKind::Error => {}
            _ => {
                let unit = self.tys.unit;
                inf.errors.push(TypeError::TypeMismatch {
                    expected: unit,
                    found: resolved,
                    span,
                });
            }
        }
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
    /// - `{ { return 1 } "a" }` → last expr `"a"` is `*const u8`,
    ///   value = `*const u8`, coerce against `i32` errors.
    fn infer_block(&mut self, inf: &mut Inferer, bid: HBlockId) -> TyId {
        let block = self.hir.blocks[bid].clone();
        let last_idx = block.items.len().checked_sub(1);
        for (i, item) in block.items.iter().enumerate() {
            let ty = self.infer_expr(inf, item.expr);
            if Some(i) != last_idx && !item.has_semi {
                let span = self.hir.exprs[item.expr].span.clone();
                self.expect_unit(inf, ty, span);
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
            HirExprKind::StrLit(_) => {
                // String literals are C-style: `*const u8`, NUL-terminator
                // appended at codegen. See spec/07_POINTER.md.
                let u8_ty = self.tys.u8;
                self.tys.intern(TyKind::Ptr(u8_ty, Mutability::Const))
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
                let _ = self.infer_expr(inf, base);
                let _ = self.infer_expr(inf, index);
                inf.errors.push(TypeError::UnsupportedFeature {
                    feature: "indexing",
                    span: span.clone(),
                });
                self.tys.error
            }
            HirExprKind::Field { base, name: _ } => {
                let _ = self.infer_expr(inf, base);
                inf.errors.push(TypeError::UnsupportedFeature {
                    feature: "field access",
                    span: span.clone(),
                });
                self.tys.error
            }
            HirExprKind::Cast { expr: inner, ty } => {
                let _ = self.infer_expr(inf, inner);
                Self::resolve_named_ty(&mut self.tys, &mut inf.errors, &ty)
            }
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
        };
        self.expr_tys[eid] = ty;
        ty
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
        self.tys.unit
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
                // `()` (or `!`/error). One-way check, no two-way flow.
                let span = self.hir.blocks[then_block].span.clone();
                self.expect_unit(inf, then_ty, span);
                self.tys.unit
            }
            Some(HElseArm::Block(bid)) => {
                let else_ty = self.infer_block(inf, bid);
                let span = self.hir.blocks[bid].span.clone();
                self.unify(inf, then_ty, else_ty, span);
                self.join_never(inf, then_ty, else_ty)
            }
            Some(HElseArm::If(eid)) => {
                let else_ty = self.infer_expr(inf, eid);
                let span = self.hir.exprs[eid].span.clone();
                self.unify(inf, then_ty, else_ty, span);
                self.join_never(inf, then_ty, else_ty)
            }
        }
    }

    /// After unifying two arm types, pick the one that *isn't* `!`.
    /// `unify`'s Never arm is a no-op, so the two TyIds remain distinct
    /// when one side is `!`. The if-expr's actual type is the
    /// non-divergent arm's type (Never absorbs). Without this, an
    /// `if c { return 1 } else { 0 }` would be typed `!` if the then
    /// arm came first, instead of `i32`.
    fn join_never(&self, inf: &Inferer, a: TyId, b: TyId) -> TyId {
        let ar = self.resolve(inf, a);
        if matches!(self.tys.kind(ar), TyKind::Never) {
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
        let local_ty = match &local_data.ty {
            Some(t) => Self::resolve_named_ty(&mut self.tys, &mut inf.errors, t),
            None => self.fresh_infer(inf, false),
        };
        self.local_tys[local] = local_ty;
        if let Some(init_id) = init {
            let init_ty = self.infer_expr(inf, init_id);
            let init_span = self.hir.exprs[init_id].span.clone();
            self.coerce(inf, init_ty, local_ty, init_span);
        }
        self.tys.unit
    }
}

/// Pointer mutability subtype: `mut ≤ const`, `mut ≤ mut`, `const ≤ const`,
/// `const ≰ mut`. Dropping write permission (`mut → const`) is safe; gaining
/// it (`const → mut`) is not.
fn mut_le(actual: Mutability, expected: Mutability) -> bool {
    use Mutability::*;
    matches!((actual, expected), (Mut, _) | (Const, Const))
}

// Suppress dead-code warnings for currently-unused helpers/fields that
// future passes (codegen) will pick up.
#[allow(dead_code)]
fn _force_use_hir_fn(_: &HirFn) {}
#[allow(dead_code)]
fn _force_use_hir_block(_: &HirBlock) {}
