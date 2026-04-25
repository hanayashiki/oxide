//! The type checker. Two phases:
//!   1. Resolve every fn's signature from source annotations (no inference).
//!   2. Check each fn body in isolation with a fresh `Inferer`.
//!
//! Inference state is per-fn: unification variables don't leak across
//! function boundaries (matches Rust's `typeck`).

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
    cx.resolve_signatures(hir);
    for (fid, _) in hir.fns.iter_enumerated() {
        cx.check_fn(hir, fid);
    }
    cx.finish()
}

struct Checker {
    tys: TyArena,
    fn_sigs: IndexVec<FnId, FnSig>,
    local_tys: IndexVec<LocalId, TyId>,
    expr_tys: IndexVec<HExprId, TyId>,
    errors: Vec<TypeError>,
    /// Per-fn inference state. `Some` while checking a body; `None` between fns.
    inferer: Option<Inferer>,
    /// Expected return type of the function currently being checked.
    cur_ret: TyId,
}

#[derive(Default)]
struct Inferer {
    bindings: IndexVec<InferId, Option<TyId>>,
    int_default: IndexVec<InferId, bool>,
}

impl Inferer {
    fn new_var(&mut self, int_default: bool) -> InferId {
        let id = self.bindings.push(None);
        let _ = self.int_default.push(int_default);
        id
    }
}

impl Checker {
    fn new(hir: &HirModule) -> Self {
        let tys = TyArena::new();
        let placeholder = tys.error;
        let local_tys: IndexVec<LocalId, TyId> =
            (0..hir.locals.len()).map(|_| placeholder).collect();
        let expr_tys: IndexVec<HExprId, TyId> =
            (0..hir.exprs.len()).map(|_| placeholder).collect();
        let fn_sigs: IndexVec<FnId, FnSig> = (0..hir.fns.len())
            .map(|_| FnSig {
                params: Vec::new(),
                ret: placeholder,
            })
            .collect();
        Self {
            tys,
            fn_sigs,
            local_tys,
            expr_tys,
            errors: Vec::new(),
            inferer: None,
            cur_ret: placeholder,
        }
    }

    /// Phase 1. Sigs are entirely source-driven — no inference.
    fn resolve_signatures(&mut self, hir: &HirModule) {
        for (fid, hir_fn) in hir.fns.iter_enumerated() {
            let mut params = Vec::with_capacity(hir_fn.params.len());
            for &lid in &hir_fn.params {
                let local = &hir.locals[lid];
                let ty = self.resolve_annotation(local.ty.as_ref(), &local.span);
                self.local_tys[lid] = ty;
                params.push(ty);
            }
            let ret = match &hir_fn.ret_ty {
                Some(t) => self.resolve_named_ty(t),
                None => self.tys.unit, // Rust-style: implicit unit
            };
            self.fn_sigs[fid] = FnSig { params, ret };
        }
    }

    /// Phase 2. Each fn body gets a fresh Inferer; finalize replaces any
    /// Infer vars left in this fn's contributions to expr_tys/local_tys.
    /// Foreign fns (`body == None`) have nothing to infer — we skip them.
    fn check_fn(&mut self, hir: &HirModule, fid: FnId) {
        let Some(body_id) = hir.fns[fid].body else {
            return;
        };
        self.inferer = Some(Inferer::default());
        self.cur_ret = self.fn_sigs[fid].ret;
        let body_ty = self.infer_block(hir, body_id);
        let body_span = hir.blocks[body_id].span.clone();
        self.coerce(body_ty, self.cur_ret, body_span);
        self.finalize_fn();
    }

    fn finalize_fn(&mut self) {
        // Default unconstrained int vars to i32; bind anything else still
        // unresolved to error (silent — we get implicit "{error}" propagation
        // and any cascading mismatches will already have been reported).
        let i32_id = self.tys.i32;
        let error_id = self.tys.error;
        let inf = self.inferer.as_mut().expect("Inferer present in fn body");
        for raw in 0..inf.bindings.len() {
            let id = InferId::from_raw(raw as u32);
            if inf.bindings[id].is_none() {
                inf.bindings[id] = Some(if inf.int_default[id] { i32_id } else { error_id });
            }
        }

        // Resolve any Infer-typed entries in this fn's contributions.
        for raw in 0..self.expr_tys.len() {
            let id = HExprId::from_raw(raw as u32);
            let resolved = self.resolve_fully(self.expr_tys[id]);
            self.expr_tys[id] = resolved;
        }
        for raw in 0..self.local_tys.len() {
            let id = LocalId::from_raw(raw as u32);
            let resolved = self.resolve_fully(self.local_tys[id]);
            self.local_tys[id] = resolved;
        }

        self.inferer = None;
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

    fn resolve_named_ty(&mut self, ty: &HirTy) -> TyId {
        // TODO: resolve custom types
        match &ty.kind {
            HirTyKind::Named(name) => match self.tys.from_prim_name(name) {
                Some(id) => id,
                None => {
                    self.errors.push(TypeError::UnknownType {
                        name: name.clone(),
                        span: ty.span.clone(),
                    });
                    self.tys.error
                }
            },
            HirTyKind::Ptr { mutability, pointee } => {
                let pointee = self.resolve_named_ty(pointee);
                self.tys.intern(TyKind::Ptr(pointee, *mutability))
            }
            HirTyKind::Error => self.tys.error,
        }
    }

    /// Annotation lookup for params and let-bindings during sig resolution.
    /// Currently this is just a `Some` shortcut; let-binding `None`s are
    /// handled in `infer_let` (Phase 2) where fresh Infer vars are allowed.
    fn resolve_annotation(&mut self, ty: Option<&HirTy>, _span: &Span) -> TyId {
        match ty {
            Some(t) => self.resolve_named_ty(t),
            None => self.tys.error,
        }
    }

    // ---------- inference primitives ----------

    fn fresh_infer(&mut self, int_default: bool) -> TyId {
        let inf = self.inferer.as_mut().expect("fresh_infer outside fn body");
        let id = inf.new_var(int_default);
        self.tys.intern(TyKind::Infer(id))
    }

    /// Walk one level of `Infer` chains (path-compression-free; trivial here).
    fn resolve(&self, ty: TyId) -> TyId {
        let mut cur = ty;
        loop {
            match self.tys.kind(cur) {
                TyKind::Infer(id) => match self
                    .inferer
                    .as_ref()
                    .and_then(|i| i.bindings.get(*id).copied().flatten())
                {
                    Some(bound) => cur = bound,
                    None => return cur,
                },
                _ => return cur,
            }
        }
    }

    /// After finalize_fn defaults all Infer vars, fully substitute through
    /// the type tree so no `Infer(_)` leaks into the result tables.
    fn resolve_fully(&mut self, ty: TyId) -> TyId {
        let resolved = self.resolve(ty);
        match self.tys.kind(resolved).clone() {
            TyKind::Infer(_) => self.tys.error, // shouldn't happen post-finalize
            TyKind::Fn(params, ret) => {
                let params: Vec<_> = params.iter().map(|&p| self.resolve_fully(p)).collect();
                let ret = self.resolve_fully(ret);
                self.tys.intern(TyKind::Fn(params, ret))
            }
            TyKind::Ptr(inner, m) => {
                let inner = self.resolve_fully(inner);
                self.tys.intern(TyKind::Ptr(inner, m))
            }
            _ => resolved,
        }
    }

    fn bind_infer(&mut self, id: InferId, ty: TyId) {
        let inf = self.inferer.as_mut().expect("bind_infer outside fn body");
        inf.bindings[id] = Some(ty);
    }

    /// Unifier. Convention: `unify(found, expected, span)` — diagnostic
    /// reports `expected: <expected>, found: <found>`.
    fn unify(&mut self, found: TyId, expected: TyId, span: Span) {
        let found = self.resolve(found);
        let expected = self.resolve(expected);
        if found == expected {
            return;
        }
        let kf = self.tys.kind(found).clone();
        let ke = self.tys.kind(expected).clone();
        match (kf, ke) {
            (TyKind::Error, _) | (_, TyKind::Error) => {}
            (TyKind::Never, _) | (_, TyKind::Never) => {}
            (TyKind::Infer(id), other) => self.bind_infer_checked(id, expected, &other, span),
            (other, TyKind::Infer(id)) => self.bind_infer_checked(id, found, &other, span),
            (TyKind::Prim(p), TyKind::Prim(q)) if p == q => {}
            (TyKind::Unit, TyKind::Unit) => {}
            (TyKind::Fn(params_f, ret_f), TyKind::Fn(params_e, ret_e)) => {
                if params_f.len() != params_e.len() {
                    self.errors.push(TypeError::TypeMismatch {
                        expected,
                        found,
                        span,
                    });
                    return;
                }
                for (pf, pe) in params_f.iter().zip(&params_e) {
                    self.unify(*pf, *pe, span.clone());
                }
                self.unify(ret_f, ret_e, span);
            }
            // Loose on mutability — unify is shape-only (per spec/07_POINTER.md).
            // The mutability subtype rule (`*mut → *const` OK at the outer
            // layer, exact match below) is enforced by `coerce` at use sites.
            (TyKind::Ptr(fi, _), TyKind::Ptr(ei, _)) => self.unify(fi, ei, span),
            _ => {
                self.errors.push(TypeError::TypeMismatch {
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
    fn coerce(&mut self, actual: TyId, expected: TyId, span: Span) {
        self.unify(actual, expected, span.clone());
        self.check_ptr_outer_compat(actual, expected, span);
    }

    /// Outer-layer coercion check. If both sides are pointers, verify
    /// `actual_mut ≤ expected_mut`, then recurse into the pointee with
    /// **strict** mutability equality (`check_ptr_inner_eq`).
    fn check_ptr_outer_compat(&mut self, actual: TyId, expected: TyId, span: Span) {
        let actual = self.resolve(actual);
        let expected = self.resolve(expected);
        let (a_pt, a_mut, e_pt, e_mut) =
            match (self.tys.kind(actual).clone(), self.tys.kind(expected).clone()) {
                (TyKind::Ptr(ap, am), TyKind::Ptr(ep, em)) => (ap, am, ep, em),
                _ => return,
            };
        if !mut_le(a_mut, e_mut) {
            self.errors.push(TypeError::PointerMutabilityMismatch {
                expected,
                actual,
                span: span.clone(),
            });
            return;
        }
        self.check_ptr_inner_eq(a_pt, e_pt, span);
    }

    /// Inner positions must match mutability exactly. Used by coerce after
    /// the outer layer has been checked. Shape mismatches under here are
    /// already caught by `unify`, so this method only emits errors for
    /// mutability divergence.
    fn check_ptr_inner_eq(&mut self, a: TyId, b: TyId, span: Span) {
        let a = self.resolve(a);
        let b = self.resolve(b);
        if let (TyKind::Ptr(a_pt, a_mut), TyKind::Ptr(b_pt, b_mut)) =
            (self.tys.kind(a).clone(), self.tys.kind(b).clone())
        {
            if a_mut != b_mut {
                self.errors.push(TypeError::PointerMutabilityMismatch {
                    expected: b,
                    actual: a,
                    span: span.clone(),
                });
                return;
            }
            self.check_ptr_inner_eq(a_pt, b_pt, span);
        }
    }

    /// Bind an Infer var to a concrete type, but reject if doing so would
    /// violate the var's `int_default` constraint (i.e., int-flagged var
    /// being unified with a non-integer concrete type).
    fn bind_infer_checked(
        &mut self,
        id: InferId,
        target: TyId,
        target_kind: &TyKind,
        span: Span,
    ) {
        let int_flagged = self
            .inferer
            .as_ref()
            .map(|i| i.int_default[id])
            .unwrap_or(false);
        if int_flagged {
            let allowed = match target_kind {
                TyKind::Prim(p) => p.is_integer(),
                TyKind::Infer(_) | TyKind::Error | TyKind::Never => true,
                _ => false,
            };
            if !allowed {
                let infer_ty = self.tys.intern(TyKind::Infer(id));
                self.errors.push(TypeError::TypeMismatch {
                    expected: target,
                    found: infer_ty,
                    span,
                });
                // Bind to error to prevent the same int var triggering more
                // errors downstream.
                self.bind_infer(id, self.tys.error);
                return;
            }
        }
        self.bind_infer(id, target);
    }

    // ---------- walk ----------

    fn infer_block(&mut self, hir: &HirModule, bid: HBlockId) -> TyId {
        let block = &hir.blocks[bid];
        let item_ids: Vec<HExprId> = block.items.clone();
        let tail_id = block.tail;
        for eid in item_ids {
            let _ = self.infer_expr(hir, eid);
        }
        match tail_id {
            Some(eid) => self.infer_expr(hir, eid),
            None => self.tys.unit,
        }
    }

    fn infer_expr(&mut self, hir: &HirModule, eid: HExprId) -> TyId {
        let expr: &HirExpr = &hir.exprs[eid];
        let span = expr.span.clone();
        let ty = match expr.kind.clone() {
            HirExprKind::IntLit(_) => self.fresh_infer(true),
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
            HirExprKind::Unary { op, expr: inner } => self.infer_unary(hir, op, inner, &span),
            HirExprKind::Binary { op, lhs, rhs } => self.infer_binary(hir, op, lhs, rhs, &span),
            HirExprKind::Assign { op, target, rhs } => {
                self.infer_assign(hir, op, target, rhs, &span)
            }
            HirExprKind::Call { callee, args } => self.infer_call(hir, callee, args, &span),
            HirExprKind::Index { base, index } => {
                let _ = self.infer_expr(hir, base);
                let _ = self.infer_expr(hir, index);
                self.errors.push(TypeError::UnsupportedFeature {
                    feature: "indexing",
                    span: span.clone(),
                });
                self.tys.error
            }
            HirExprKind::Field { base, name: _ } => {
                let _ = self.infer_expr(hir, base);
                self.errors.push(TypeError::UnsupportedFeature {
                    feature: "field access",
                    span: span.clone(),
                });
                self.tys.error
            }
            HirExprKind::Cast { expr: inner, ty } => {
                let _ = self.infer_expr(hir, inner);
                self.resolve_named_ty(&ty)
            }
            HirExprKind::If { cond, then_block, else_arm } => {
                self.infer_if(hir, cond, then_block, else_arm, &span)
            }
            HirExprKind::Block(bid) => self.infer_block(hir, bid),
            HirExprKind::Return(val) => {
                if let Some(v) = val {
                    let v_ty = self.infer_expr(hir, v);
                    let cur_ret = self.cur_ret;
                    let v_span = hir.exprs[v].span.clone();
                    self.coerce(v_ty, cur_ret, v_span);
                } else {
                    let cur_ret = self.cur_ret;
                    self.coerce(self.tys.unit, cur_ret, span.clone());
                }
                self.tys.never
            }
            HirExprKind::Let { local, init } => self.infer_let(hir, local, init, &span),
            HirExprKind::Poison => self.tys.error,
        };
        self.expr_tys[eid] = ty;
        ty
    }

    fn infer_unary(
        &mut self,
        hir: &HirModule,
        op: UnOp,
        inner: HExprId,
        _span: &Span,
    ) -> TyId {
        let t = self.infer_expr(hir, inner);
        match op {
            UnOp::Neg | UnOp::BitNot => t, // numeric / integer (typeck v0 trusts; codegen checks)
            UnOp::Not => {
                let span = hir.exprs[inner].span.clone();
                let bool_ty = self.tys.bool;
                self.unify(t, bool_ty, span);
                bool_ty
            }
        }
    }

    fn infer_binary(
        &mut self,
        hir: &HirModule,
        op: BinOp,
        lhs: HExprId,
        rhs: HExprId,
        span: &Span,
    ) -> TyId {
        let lt = self.infer_expr(hir, lhs);
        let rt = self.infer_expr(hir, rhs);
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
                self.unify(lt, rt, span.clone());
                lt
            }
            // Comparisons: same type both sides; result = bool.
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                self.unify(lt, rt, span.clone());
                bool_ty
            }
            // Logical: both sides bool; result = bool.
            BinOp::And | BinOp::Or => {
                self.unify(lt, bool_ty, hir.exprs[lhs].span.clone());
                self.unify(rt, bool_ty, hir.exprs[rhs].span.clone());
                bool_ty
            }
            // Shifts: lhs's type is the result; rhs is any integer (loosely).
            BinOp::Shl | BinOp::Shr => lt,
        }
    }

    fn infer_assign(
        &mut self,
        hir: &HirModule,
        _op: AssignOp,
        target: HExprId,
        rhs: HExprId,
        span: &Span,
    ) -> TyId {
        let t = self.infer_expr(hir, target);
        let r = self.infer_expr(hir, rhs);
        // RHS coerces *to* the LHS slot — direction matters for pointer
        // mutability (`*mut → *const` OK, reverse is not).
        self.coerce(r, t, span.clone());
        self.tys.unit
    }

    fn infer_call(
        &mut self,
        hir: &HirModule,
        callee: HExprId,
        args: Vec<HExprId>,
        span: &Span,
    ) -> TyId {
        let callee_ty = self.infer_expr(hir, callee);
        let callee_resolved = self.resolve(callee_ty);
        let arg_tys: Vec<TyId> = args.iter().map(|&a| self.infer_expr(hir, a)).collect();
        match self.tys.kind(callee_resolved).clone() {
            TyKind::Fn(param_tys, ret_ty) => {
                if param_tys.len() != args.len() {
                    self.errors.push(TypeError::WrongArgCount {
                        expected: param_tys.len(),
                        found: args.len(),
                        span: span.clone(),
                    });
                    return ret_ty;
                }
                for ((&aid, &pty), &aty) in args.iter().zip(&param_tys).zip(&arg_tys) {
                    let arg_span = hir.exprs[aid].span.clone();
                    self.coerce(aty, pty, arg_span);
                }
                ret_ty
            }
            TyKind::Error => self.tys.error,
            _ => {
                self.errors.push(TypeError::NotCallable {
                    found: callee_resolved,
                    span: span.clone(),
                });
                self.tys.error
            }
        }
    }

    fn infer_if(
        &mut self,
        hir: &HirModule,
        cond: HExprId,
        then_block: HBlockId,
        else_arm: Option<HElseArm>,
        _span: &Span,
    ) -> TyId {
        let cond_ty = self.infer_expr(hir, cond);
        let cond_span = hir.exprs[cond].span.clone();
        let bool_ty = self.tys.bool;
        self.unify(cond_ty, bool_ty, cond_span);
        let then_ty = self.infer_block(hir, then_block);
        match else_arm {
            None => {
                let span = hir.blocks[then_block].span.clone();
                let unit = self.tys.unit;
                self.unify(then_ty, unit, span);
                unit
            }
            Some(HElseArm::Block(bid)) => {
                let else_ty = self.infer_block(hir, bid);
                let span = hir.blocks[bid].span.clone();
                self.unify(then_ty, else_ty, span);
                then_ty
            }
            Some(HElseArm::If(eid)) => {
                let else_ty = self.infer_expr(hir, eid);
                let span = hir.exprs[eid].span.clone();
                self.unify(then_ty, else_ty, span);
                then_ty
            }
        }
    }

    fn infer_let(
        &mut self,
        hir: &HirModule,
        local: LocalId,
        init: Option<HExprId>,
        _span: &Span,
    ) -> TyId {
        let local_data: &HirLocal = &hir.locals[local];
        let local_ty = match &local_data.ty {
            Some(t) => self.resolve_named_ty(t),
            None => self.fresh_infer(false),
        };
        self.local_tys[local] = local_ty;
        if let Some(init_id) = init {
            let init_ty = self.infer_expr(hir, init_id);
            let init_span = hir.exprs[init_id].span.clone();
            self.coerce(init_ty, local_ty, init_span);
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
