//! Per-fn body emission: `lower_fn` (entry-block setup, param spilling,
//! body emission, implicit return) plus `emit_block`, `emit_return`, and
//! `emit_let`.

use std::collections::HashMap;

use crate::codegen::ty::is_void_ret;
use crate::hir::{HBlockId, HExprId, LocalId};
use crate::typeck::subst_from;

use super::{Codegen, FnCodegenContext, LowerTarget, Operand};

impl<'a, 'ctx> Codegen<'a, 'ctx> {
    pub(super) fn lower_fn(&mut self, target: LowerTarget) {
        // Extract (fid, ret, subst, fnv) from either the HIR-driven
        // non-generic path or the mono-driven generic path. The rest of
        // the body is target-agnostic.
        let (fid, ret, subst, fnv) = match target {
            LowerTarget::NonGeneric(fid) => {
                let sig = self.typeck_results.fn_sig(fid);
                let fnv = self.fn_decls[fid]
                    .expect("non-generic fn must have a fn_decls entry from Pass A");
                (fid, sig.ret, HashMap::new(), fnv)
            }
            LowerTarget::Generic(inst_id) => {
                // self.typeck_results.fn_sig and self.mono.instances are
                // disjoint sub-objects of self, so the two `&` reads
                // coexist.
                let inst_fid = self.mono.instances[inst_id].fid;
                let inst_ret = self.mono.instances[inst_id].ret_ty;
                let subst = subst_from(
                    &self.typeck_results.fn_sig(inst_fid).generic_params,
                    &self.mono.instances[inst_id].type_args,
                );
                (
                    inst_fid,
                    inst_ret,
                    subst,
                    self.inst_decls[inst_id].expect(
                        "lower_fn called on intrinsic instance — \
                         Pass 2 should have skipped non-Call operations",
                    ),
                )
            }
        };

        // Two blocks at start: `allocas:` (the entry block) holds only
        // alloca instructions and falls through to `body:` via an
        // unconditional branch. All real emission happens in `body`.
        let allocas_bb = self.ctx.append_basic_block(fnv, "allocas");
        let body_bb = self.ctx.append_basic_block(fnv, "body");
        self.builder.position_at_end(allocas_bb);
        self.builder.build_unconditional_branch(body_bb).unwrap();
        self.builder.position_at_end(body_bb);

        let mut fx = FnCodegenContext {
            ret_ty: ret,
            subst,
            fn_value: fnv,
            allocas_bb,
            locals: HashMap::new(),
            loop_targets: Vec::new(),
        };

        // Alloca slots for params and store the incoming arg values.
        // Array-typed params skip the alloca+store: per Path A in
        // spec/09_ARRAY.md, `lower_fn_type` lowered the param to LLVM
        // `ptr` and the caller (`emit_call`) memcpy'd into a fresh slot
        // before passing. The incoming `ptr` IS the local's storage.
        let hir_fn = &self.hir.fns[fid];
        for (i, &lid) in hir_fn.params.iter().enumerate() {
            let pty = self.local_ty(&fx, lid);
            let arg = fnv.get_nth_param(i as u32).expect("param exists");
            if self.is_sized_array(pty) {
                fx.locals.insert(lid, arg.into_pointer_value());
                continue;
            }
            let llty = self.lower_ty(pty);
            let slot = self.alloca_in_entry(
                &fx,
                llty,
                &format!("{}.{}.slot", self.hir.locals[lid].name, lid.raw()),
            );
            self.builder.build_store(slot, arg).unwrap();
            fx.locals.insert(lid, slot);
        }

        // `lower_fn` is only called for body-having fns, so unwrap is sound.
        let body_id = hir_fn
            .body
            .expect("lower_fn called on foreign fn — codegen should have skipped");
        let body_val = self.emit_block(&mut fx, body_id);

        if !self.is_terminated() {
            // `fx.ret_ty` is the substituted return type (set at body
            // entry). For non-generic fns it equals `sig.ret`; for
            // generic instances it's `inst.ret` from mono.
            let ret_ty = fx.ret_ty;
            if is_void_ret(self.typeck_results.tys(), ret_ty) {
                self.builder.build_return(None).unwrap();
            } else {
                // Array-typed return — Path A: body produced a place ptr;
                // load_value loads the aggregate before return-by-value
                // so LLVM's calling convention does the sret/register-return
                // rewrite. Non-array returns: load_value passes through.
                let op = body_val.expect("non-void fn body produced no value");
                let v = op.load_value(self, ret_ty, "ret.load");
                self.builder.build_return(Some(&v)).unwrap();
            }
        }
    }

    pub(super) fn emit_block(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        bid: HBlockId,
    ) -> Option<Operand<'ctx>> {
        // Clone the items vec so we don't borrow self.hir while emitting.
        let block = self.hir.blocks[bid].clone();
        let last_idx = block.items.len().checked_sub(1);
        let mut tail: Option<Operand<'ctx>> = None;
        for (i, item) in block.items.iter().enumerate() {
            if self.is_terminated() {
                return None;
            }
            let v = self.emit_expr(fx, item.expr);
            if Some(i) == last_idx && !item.has_semi {
                tail = v;
            }
        }
        if self.is_terminated() {
            return None;
        }
        // No-tail block (or tail with semi) types as `()`: return Unit.
        // Otherwise propagate the tail's operand.
        tail.or(Some(Operand::Unit))
    }

    pub(super) fn emit_return(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        val: Option<HExprId>,
    ) {
        // `fx.ret_ty` is the substituted return type (set at body
        // entry). For non-generic fns it equals `sig.ret`; for generic
        // instances it's `inst.ret` from mono.
        let ret_ty = fx.ret_ty;
        if is_void_ret(self.typeck_results.tys(), ret_ty) {
            // Either `return;` or `return e` where e itself is divergent.
            if let Some(v_eid) = val {
                let _ = self.emit_expr(fx, v_eid);
                if self.is_terminated() {
                    return;
                }
            }
            self.builder.build_return(None).unwrap();
            return;
        }

        match val.and_then(|eid| self.emit_expr(fx, eid).map(|op| (eid, op))) {
            Some((eid, op)) => {
                // Array return: Path A — load the place into an SSA aggregate
                // before returning by value. load_value handles this uniformly
                // (Place → load, Value → passthrough).
                let ty = self.ty_of(fx, eid);
                let v = op.load_value(self, ty, "ret.load");
                self.builder.build_return(Some(&v)).unwrap();
            }
            None => {
                // Divergent operand already terminated the bb, or there's
                // no operand on a non-void fn (typeck should have caught
                // the latter).
                if !self.is_terminated() {
                    self.builder.build_unreachable().unwrap();
                }
            }
        }
    }

    pub(super) fn emit_let(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        lid: LocalId,
        init: Option<HExprId>,
    ) {
        let ty = self.local_ty(fx, lid);
        let local = &self.hir.locals[lid];

        // `Never`-typed locals (`let a = loop {};`, `let a = return;`)
        // cannot have storage — `lower_ty(Never)` panics by design (no
        // value form ever exists). The init diverges, the BB terminates,
        // and no downstream read of `a` can execute. Skip the alloca and
        // just evaluate the init for its side-effecting BB termination.
        if matches!(
            self.typeck_results.tys().kind(ty),
            crate::typeck::TyKind::Never
        ) {
            if let Some(init_eid) = init {
                let _ = self.emit_expr(fx, init_eid);
            }
            return;
        }

        // `()`-typed locals lower to `{}` (zero-sized empty struct).
        // The alloca is dead and gets DCE'd in any opt level.
        let llty = self.lower_ty(ty);
        let slot = self.alloca_in_entry(fx, llty, &format!("{}.{}.slot", local.name, lid.raw()));
        fx.locals.insert(lid, slot);
        if let Some(init_eid) = init {
            // None ⇒ divergent init (`let a = return;`); slot stays
            // uninitialized but the basic block is already terminated by
            // the diverge — no read can follow.
            if let Some(op) = self.emit_expr(fx, init_eid) {
                op.store_into(self, slot, ty);
            }
        }
    }

}
