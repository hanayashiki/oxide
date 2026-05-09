//! Control-flow lowering: `if` / `loop` / `break` / `continue`.

use inkwell::basic_block::BasicBlock;
use inkwell::values::PointerValue;

use crate::codegen::ty::is_void_ret;
use crate::hir::{HBlockId, HElseArm, HExprId};
use crate::typeck::TyId;

use super::{Codegen, FnCodegenContext, LoopTargets, Operand};

impl<'a, 'ctx> Codegen<'a, 'ctx> {
    pub(super) fn emit_if(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        eid: HExprId,
        cond: HExprId,
        then_block: HBlockId,
        else_arm: Option<HElseArm>,
    ) -> Option<Operand<'ctx>> {
        /// Close out one arm of an `if`: if the arm didn't diverge, store its
        /// value into the result slot (when both exist) and branch to the
        /// merge block. No-op if the arm terminated the BB on its own
        /// (`return`/`break` in the arm body).
        fn seal_arm<'a, 'ctx>(
            codegen: &mut Codegen<'a, 'ctx>,
            result_slot: Option<PointerValue<'ctx>>,
            arm_val: Option<Operand<'ctx>>,
            if_ty: TyId,
            merge_bb: BasicBlock<'ctx>,
        ) {
            if codegen.is_terminated() {
                return;
            }
            if let (Some(slot), Some(op)) = (result_slot, arm_val) {
                op.store_into(codegen, slot, if_ty);
            }
            codegen
                .builder
                .build_unconditional_branch(merge_bb)
                .unwrap();
        }

        let cond_ty = self.ty_of(fx, cond);
        let cond_op = self.emit_expr(fx, cond)?;
        let cond_v = cond_op.load_value(self, cond_ty, "load").into_int_value();
        let parent = fx.fn_value;
        let then_bb = self.ctx.append_basic_block(parent, "if.then");
        let else_bb = self.ctx.append_basic_block(parent, "if.else");
        let merge_bb = self.ctx.append_basic_block(parent, "if.end");

        self.builder
            .build_conditional_branch(cond_v, then_bb, else_bb)
            .unwrap();

        // Materialize a result slot iff the if expression has a real
        // value type. For unit / never ifs we skip — keeps IR clean even
        // though the {} alloca would be harmless.
        let if_ty = self.ty_of(fx, eid);
        let result_slot = if !is_void_ret(self.typeck_results.tys(), if_ty) {
            let llty = self.lower_ty(if_ty);
            Some(self.alloca_in_entry(fx, llty, "if.slot"))
        } else {
            None
        };

        // then arm
        self.builder.position_at_end(then_bb);
        let then_val = self.emit_block(fx, then_block);
        seal_arm(self, result_slot, then_val, if_ty, merge_bb);

        // else arm
        self.builder.position_at_end(else_bb);
        match else_arm {
            Some(HElseArm::Block(bid)) => {
                let else_val = self.emit_block(fx, bid);
                seal_arm(self, result_slot, else_val, if_ty, merge_bb);
            }
            Some(HElseArm::If(else_eid)) => {
                let else_val = self.emit_expr(fx, else_eid);
                seal_arm(self, result_slot, else_val, if_ty, merge_bb);
            }
            None => {
                self.builder.build_unconditional_branch(merge_bb).unwrap();
            }
        }

        self.builder.position_at_end(merge_bb);

        // If both arms diverged, the merge block has no predecessors —
        // make it explicitly unreachable so the verifier is happy.
        if merge_bb.get_first_use().is_none() {
            self.builder.build_unreachable().unwrap();
            return None;
        }

        match result_slot {
            Some(slot) => {
                let llty = self.lower_ty(if_ty);
                Some(Operand::Value(
                    self.builder.build_load(llty, slot, "if.val").unwrap(),
                ))
            }
            None => Some(Operand::Unit),
        }
    }

    /// Emit a unified loop (`while` / `loop` / C-style `for`). All three
    /// surface forms collapse to the same C-style skeleton with each of
    /// `init` / `cond` / `update` independently optional. See
    /// spec/13_LOOPS.md "One unified IR skeleton".
    ///
    /// CFG shape:
    /// ```text
    /// init?  -> cond? -> body -> update? -> (back-edge to cond/body)
    ///           |  ^                   ^
    ///           |  +-- false:          +-- continue jumps here
    ///           +----- true:           (= update_bb if Some, else cond_bb if Some, else body_bb)
    ///                                  break jumps to end_bb
    /// ```
    pub(super) fn emit_loop(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        eid: HExprId,
        init: Option<HExprId>,
        cond: Option<HExprId>,
        update: Option<HExprId>,
        body: HBlockId,
    ) -> Option<Operand<'ctx>> {
        let parent = fx.fn_value;

        // Always-present blocks. init / cond / update are appended only
        // when their respective slot is Some.
        let body_bb = self.ctx.append_basic_block(parent, "loop.body");
        let end_bb = self.ctx.append_basic_block(parent, "loop.end");
        let init_bb = init
            .is_some()
            .then(|| self.ctx.append_basic_block(parent, "loop.init"));
        let cond_bb = cond
            .is_some()
            .then(|| self.ctx.append_basic_block(parent, "loop.cond"));
        let update_bb = update
            .is_some()
            .then(|| self.ctx.append_basic_block(parent, "loop.update"));

        // continue_target_bb (also the back-edge target from body):
        // first-Some of [update, cond, body]. break always lands in
        // end_bb.
        let continue_target_bb = update_bb.or(cond_bb).unwrap_or(body_bb);

        // Result slot: allocate iff the loop's typeck'd type is a value
        // type (non-`()`, non-`!`). Concretely fires only for
        // `cond.is_none() && has_break` with at least one valued break.
        let loop_ty = self.ty_of(fx, eid);
        let result_slot = if !is_void_ret(self.typeck_results.tys(), loop_ty) {
            let llty = self.lower_ty(loop_ty);
            Some(self.alloca_in_entry(fx, llty, "loop.slot"))
        } else {
            None
        };

        // Caller block jumps into the first existing of init/cond/body.
        let entry_jump = init_bb.or(cond_bb).unwrap_or(body_bb);
        self.builder.build_unconditional_branch(entry_jump).unwrap();

        // init: <init>; br cond_or_body
        if let (Some(ibb), Some(init_eid)) = (init_bb, init) {
            self.builder.position_at_end(ibb);
            let _ = self.emit_expr(fx, init_eid);
            if !self.is_terminated() {
                self.builder
                    .build_unconditional_branch(cond_bb.unwrap_or(body_bb))
                    .unwrap();
            }
        }

        // cond: %c = <cond>; br i1 %c, body, end
        if let (Some(cbb), Some(cond_eid)) = (cond_bb, cond) {
            self.builder.position_at_end(cbb);
            let cond_ty = self.ty_of(fx, cond_eid);
            if let Some(cond_op) = self.emit_expr(fx, cond_eid) {
                let cond_v = cond_op.load_value(self, cond_ty, "load").into_int_value();
                if !self.is_terminated() {
                    self.builder
                        .build_conditional_branch(cond_v, body_bb, end_bb)
                        .unwrap();
                }
            }
            // Cond diverged (`while return { … }`): cond_bb is now
            // terminated, the back-edge from update/body still targets
            // it, but no new path reaches body or end. The verifier
            // accepts an unreachable cond_bb past its terminator.
        }

        // body: <body>; br continue_target_bb
        fx.loop_targets.push(LoopTargets {
            end_bb,
            continue_target_bb,
            result_slot,
        });
        self.builder.position_at_end(body_bb);
        let _body_val = self.emit_block(fx, body);
        if !self.is_terminated() {
            self.builder
                .build_unconditional_branch(continue_target_bb)
                .unwrap();
        }
        fx.loop_targets.pop();

        // update: <update>; br cond_or_body
        if let (Some(ubb), Some(update_eid)) = (update_bb, update) {
            self.builder.position_at_end(ubb);
            let _ = self.emit_expr(fx, update_eid);
            if !self.is_terminated() {
                self.builder
                    .build_unconditional_branch(cond_bb.unwrap_or(body_bb))
                    .unwrap();
            }
        }

        // end: load result slot if any. If end has no preds — divergent
        // loop, no break ever reaches here — terminate with `unreachable`
        // so the verifier accepts the fn (mirrors emit_if's both-arms-
        // diverged handling).
        self.builder.position_at_end(end_bb);
        if end_bb.get_first_use().is_none() {
            self.builder.build_unreachable().unwrap();
            return None;
        }
        match result_slot {
            Some(slot) => {
                let llty = self.lower_ty(loop_ty);
                Some(Operand::Value(
                    self.builder.build_load(llty, slot, "loop.val").unwrap(),
                ))
            }
            None => Some(Operand::Unit),
        }
    }

    /// Emit `break expr?`. Stores `expr`'s value into the innermost
    /// loop's result slot (if any) before branching to its `end_bb`.
    /// Mirrors `emit_return`'s "compute operand, then exit" shape — the
    /// difference is that return calls `build_return` while break stores
    /// to a slot and branches.
    pub(super) fn emit_break(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        expr: Option<HExprId>,
    ) {
        let target = *fx
            .loop_targets
            .last()
            .expect("HIR ensured break is inside a loop");
        if let Some(eid) = expr {
            let ty = self.ty_of(fx, eid);
            let op = self.emit_expr(fx, eid);
            if self.is_terminated() {
                return;
            }
            if let (Some(slot), Some(op)) = (target.result_slot, op) {
                op.store_into(self, slot, ty);
            }
            self.builder
                .build_unconditional_branch(target.end_bb)
                .unwrap();
        } else if !self.is_terminated() {
            self.builder
                .build_unconditional_branch(target.end_bb)
                .unwrap();
        }
    }

    /// Emit `continue` — branch to the innermost loop's
    /// `continue_target_bb`. No operand in v0.
    pub(super) fn emit_continue(&mut self, fx: &mut FnCodegenContext<'ctx>) {
        let target = *fx
            .loop_targets
            .last()
            .expect("HIR ensured continue is inside a loop");
        if !self.is_terminated() {
            self.builder
                .build_unconditional_branch(target.continue_target_bb)
                .unwrap();
        }
    }
}
