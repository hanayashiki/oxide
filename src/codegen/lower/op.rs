//! Operator lowering: unary, binary (incl. short-circuit), compound
//! assign, deref-ptr extraction, and the `as` cast.

use inkwell::IntPredicate;
use inkwell::values::{IntValue, PointerValue};

use crate::codegen::ty::{as_prim, is_signed_prim, lower_prim};
use crate::hir::HExprId;
use crate::parser::ast::{AssignOp, BinOp, UnOp};
use crate::typeck::TyKind;

use super::{Codegen, FnCodegenContext, Operand};

impl<'a, 'ctx> Codegen<'a, 'ctx> {
    pub(super) fn emit_unary(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        op: UnOp,
        expr: HExprId,
    ) -> Option<Operand<'ctx>> {
        if let UnOp::Deref = op {
            return self.emit_deref_ptr(fx, expr);
        }
        let inner_ty = self.ty_of(fx, expr);
        let inner_op = self.emit_expr(fx, expr)?;
        let v = inner_op.load_value(self, inner_ty, "load").into_int_value();
        let ty = v.get_type();
        let res: IntValue<'ctx> = match op {
            UnOp::Neg => self.builder.build_int_neg(v, "neg").unwrap(),
            UnOp::Not => self
                .builder
                .build_xor(v, ty.const_int(1, false), "not")
                .unwrap(),
            UnOp::BitNot => self
                .builder
                .build_xor(v, ty.const_all_ones(), "bnot")
                .unwrap(),
            UnOp::Deref => unreachable!("Deref handled above"),
        };
        Some(Operand::Value(res.into()))
    }

    /// Lower the operand of a `Deref` to its place form. The "ptr" in
    /// the name refers to the operand expression's type — typeck has
    /// already validated that the operand is `Ptr(_, _)`-typed (it'd
    /// have rejected `*5`), so the SSA value loaded from the operand
    /// IS the address being dereferenced. Wrapping it as
    /// `Operand::Place(ptr)` gives the place-form `*p`: "the location
    /// at address p". Reached only from `emit_unary`'s `Deref` arm
    /// (which propagates this Operand directly) and `lvalue`'s `Deref`
    /// arm (which extracts the inner ptr).
    ///
    /// `into_pointer_value` is a typeck-guaranteed downcast — the
    /// operand's `lower_ty` produces an LLVM `ptr`, so the load yields
    /// `BasicValueEnum::PointerValue`. Mismatch here would mean typeck
    /// let through a non-`Ptr` Deref, which is an upstream bug.
    pub(super) fn emit_deref_ptr(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        expr: HExprId,
    ) -> Option<Operand<'ctx>> {
        let inner_ty = self.ty_of(fx, expr);
        let inner_op = self.emit_expr(fx, expr)?;
        let ptr = inner_op
            .load_value(self, inner_ty, "deref")
            .into_pointer_value();
        Some(Operand::Place(ptr))
    }

    pub(super) fn emit_binary(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        eid: HExprId,
        op: BinOp,
        lhs: HExprId,
        rhs: HExprId,
    ) -> Option<Operand<'ctx>> {
        // Short-circuit operators have their own control-flow shape.
        if let BinOp::And | BinOp::Or = op {
            return self.emit_short_circuit(fx, op, lhs, rhs);
        }

        let lt = self.ty_of(fx, lhs);
        let rt = self.ty_of(fx, rhs);
        let l_op = self.emit_expr(fx, lhs)?;
        let r_op = self.emit_expr(fx, rhs)?;
        let l = l_op.load_value(self, lt, "load").into_int_value();
        let r = r_op.load_value(self, rt, "load").into_int_value();
        let signed = as_prim(self.typeck_results.tys(), lt)
            .map(is_signed_prim)
            .unwrap_or(false);

        let v: IntValue<'ctx> = match op {
            BinOp::Add => self.builder.build_int_add(l, r, "add").unwrap(),
            BinOp::Sub => self.builder.build_int_sub(l, r, "sub").unwrap(),
            BinOp::Mul => self.builder.build_int_mul(l, r, "mul").unwrap(),
            BinOp::Div if signed => self.builder.build_int_signed_div(l, r, "sdiv").unwrap(),
            BinOp::Div => self.builder.build_int_unsigned_div(l, r, "udiv").unwrap(),
            BinOp::Rem if signed => self.builder.build_int_signed_rem(l, r, "srem").unwrap(),
            BinOp::Rem => self.builder.build_int_unsigned_rem(l, r, "urem").unwrap(),
            BinOp::BitAnd => self.builder.build_and(l, r, "and").unwrap(),
            BinOp::BitOr => self.builder.build_or(l, r, "or").unwrap(),
            BinOp::BitXor => self.builder.build_xor(l, r, "xor").unwrap(),
            BinOp::Shl => {
                let r = self.coerce_shift_amt(r, l.get_type());
                self.builder.build_left_shift(l, r, "shl").unwrap()
            }
            BinOp::Shr => {
                let r = self.coerce_shift_amt(r, l.get_type());
                self.builder.build_right_shift(l, r, signed, "shr").unwrap()
            }
            BinOp::Eq => self
                .builder
                .build_int_compare(IntPredicate::EQ, l, r, "eq")
                .unwrap(),
            BinOp::Ne => self
                .builder
                .build_int_compare(IntPredicate::NE, l, r, "ne")
                .unwrap(),
            BinOp::Lt => self
                .builder
                .build_int_compare(
                    if signed {
                        IntPredicate::SLT
                    } else {
                        IntPredicate::ULT
                    },
                    l,
                    r,
                    "lt",
                )
                .unwrap(),
            BinOp::Le => self
                .builder
                .build_int_compare(
                    if signed {
                        IntPredicate::SLE
                    } else {
                        IntPredicate::ULE
                    },
                    l,
                    r,
                    "le",
                )
                .unwrap(),
            BinOp::Gt => self
                .builder
                .build_int_compare(
                    if signed {
                        IntPredicate::SGT
                    } else {
                        IntPredicate::UGT
                    },
                    l,
                    r,
                    "gt",
                )
                .unwrap(),
            BinOp::Ge => self
                .builder
                .build_int_compare(
                    if signed {
                        IntPredicate::SGE
                    } else {
                        IntPredicate::UGE
                    },
                    l,
                    r,
                    "ge",
                )
                .unwrap(),
            BinOp::And | BinOp::Or => unreachable!("handled by short-circuit path"),
        };
        let _ = eid;
        Some(Operand::Value(v.into()))
    }

    /// LLVM requires shift amounts to match the lhs's int type.
    fn coerce_shift_amt(
        &mut self,
        r: IntValue<'ctx>,
        target: inkwell::types::IntType<'ctx>,
    ) -> IntValue<'ctx> {
        if r.get_type().get_bit_width() == target.get_bit_width() {
            return r;
        }
        if r.get_type().get_bit_width() < target.get_bit_width() {
            self.builder.build_int_z_extend(r, target, "shamt").unwrap()
        } else {
            self.builder.build_int_truncate(r, target, "shamt").unwrap()
        }
    }

    fn emit_short_circuit(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        op: BinOp,
        lhs: HExprId,
        rhs: HExprId,
    ) -> Option<Operand<'ctx>> {
        let lt = self.ty_of(fx, lhs);
        let l_op = self.emit_expr(fx, lhs)?;
        let l = l_op.load_value(self, lt, "load").into_int_value();
        let lhs_end_bb = self.builder.get_insert_block().unwrap();
        let parent = fx.fn_value;
        let rhs_bb = self.ctx.append_basic_block(parent, "logic.rhs");
        let end_bb = self.ctx.append_basic_block(parent, "logic.end");

        match op {
            BinOp::And => {
                self.builder
                    .build_conditional_branch(l, rhs_bb, end_bb)
                    .unwrap();
            }
            BinOp::Or => {
                self.builder
                    .build_conditional_branch(l, end_bb, rhs_bb)
                    .unwrap();
            }
            _ => unreachable!(),
        }

        self.builder.position_at_end(rhs_bb);
        let rt = self.ty_of(fx, rhs);
        let r_op = self.emit_expr(fx, rhs);
        // rhs may diverge (`a && return`); short-circuit still has the
        // lhs-false predecessor edge into end_bb, so the phi is well-formed
        // with one incoming. Skip the rhs incoming if it diverged.
        let rhs_incoming = r_op.map(|op| {
            let r = op.load_value(self, rt, "load").into_int_value();
            let rhs_end_bb = self.builder.get_insert_block().unwrap();
            if !self.is_terminated() {
                self.builder.build_unconditional_branch(end_bb).unwrap();
            }
            (r, rhs_end_bb)
        });

        self.builder.position_at_end(end_bb);
        let phi = self
            .builder
            .build_phi(self.ctx.bool_type(), "logic")
            .unwrap();
        let short_circuit_val = match op {
            BinOp::And => self.ctx.bool_type().const_int(0, false),
            BinOp::Or => self.ctx.bool_type().const_int(1, false),
            _ => unreachable!(),
        };
        match rhs_incoming {
            Some((r, rhs_end_bb)) => {
                phi.add_incoming(&[(&short_circuit_val, lhs_end_bb), (&r, rhs_end_bb)]);
            }
            None => {
                phi.add_incoming(&[(&short_circuit_val, lhs_end_bb)]);
            }
        }
        Some(Operand::Value(phi.as_basic_value()))
    }

    pub(super) fn emit_assign(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        op: AssignOp,
        target: HExprId,
        rhs: HExprId,
    ) {
        let target_ty = self.ty_of(fx, target);
        // Rust evaluates rhs first; if rhs diverges (`b = return;`), the
        // BB is already terminated and lvalue computation is unreachable.
        let Some(rhs_op) = self.emit_expr(fx, rhs) else {
            return;
        };

        if let AssignOp::Eq = op {
            let slot: PointerValue<'_> = self.lvalue(fx, target);
            rhs_op.store_into(self, slot, target_ty);
            return;
        }

        // Compound ops (+=, -=, *=, /=, %=, &=, |=, ^=, <<=, >>=) are
        // int-only by language design.
        let slot = self.lvalue(fx, target);
        let r = rhs_op.load_value(self, target_ty, "load").into_int_value();
        let llty = self.lower_ty(target_ty);
        let cur = self
            .builder
            .build_load(llty, slot, "asgn.cur")
            .unwrap()
            .into_int_value();
        let signed = as_prim(self.typeck_results.tys(), target_ty)
            .map(is_signed_prim)
            .unwrap_or(false);
        let build_result = match op {
            AssignOp::Add => self.builder.build_int_add(cur, r, "asgn.add"),
            AssignOp::Sub => self.builder.build_int_sub(cur, r, "asgn.sub"),
            AssignOp::Mul => self.builder.build_int_mul(cur, r, "asgn.mul"),
            AssignOp::Div if signed => self.builder.build_int_signed_div(cur, r, "asgn.sdiv"),
            AssignOp::Div => self.builder.build_int_unsigned_div(cur, r, "asgn.udiv"),
            AssignOp::Rem if signed => self.builder.build_int_signed_rem(cur, r, "asgn.srem"),
            AssignOp::Rem => self.builder.build_int_unsigned_rem(cur, r, "asgn.urem"),
            AssignOp::BitAnd => self.builder.build_and(cur, r, "asgn.and"),
            AssignOp::BitOr => self.builder.build_or(cur, r, "asgn.or"),
            AssignOp::BitXor => self.builder.build_xor(cur, r, "asgn.xor"),
            AssignOp::Shl => {
                let r = self.coerce_shift_amt(r, cur.get_type());
                self.builder.build_left_shift(cur, r, "asgn.shl")
            }
            AssignOp::Shr => {
                let r = self.coerce_shift_amt(r, cur.get_type());
                self.builder.build_right_shift(cur, r, signed, "asgn.shr")
            }
            AssignOp::Eq => unreachable!("handled by the early return above"),
        };
        self.builder
            .build_store(slot, build_result.unwrap())
            .unwrap();
    }

    /// `expr as Ty` codegen. Per spec/12_AS.md §"Codegen": dispatch
    /// on `(src_kind, dst_kind)` per the allowed-set table. Typeck's
    /// `infer_cast` (E0274 `InvalidCast`) has already rejected
    /// off-table pairs, so the catch-all arm is an invariant assertion.
    pub(super) fn emit_cast(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        eid: HExprId,
        inner: HExprId,
    ) -> Option<Operand<'ctx>> {
        let dst_ty = self.ty_of(fx, eid);
        let src_ty = self.ty_of(fx, inner);
        let inner_op = self.emit_expr(fx, inner)?;

        let tys = self.typeck_results.tys();
        let kind = crate::typeck::cast_kind(tys, src_ty, dst_ty);

        match kind {
            crate::typeck::CastKind::PtrToPtr | crate::typeck::CastKind::Identity
                if matches!(tys.kind(src_ty), TyKind::Ptr(..)) =>
            {
                // LLVM `ptr` is opaque; mutability/pointee shape
                // lives in the Oxide type system only. Thread the
                // operand through unchanged.
                Some(inner_op)
            }
            crate::typeck::CastKind::IntToInt
            | crate::typeck::CastKind::BoolToInt
            | crate::typeck::CastKind::Identity => {
                // Both ends are primitives (or src == dst Prim);
                // existing trunc / sext / zext logic handles all of
                // them uniformly.
                let v = inner_op.load_value(self, src_ty, "load").into_int_value();
                let dst_prim = as_prim(self.typeck_results.tys(), dst_ty).expect(
                    "emit_cast: typeck should have rejected non-prim destination \
                     for IntToInt / BoolToInt",
                );
                let dst_ll = lower_prim(self.ctx, dst_prim);
                let src_w = v.get_type().get_bit_width();
                let dst_w = dst_ll.get_bit_width();
                if src_w == dst_w {
                    return Some(Operand::Value(v.into()));
                }
                if dst_w < src_w {
                    return Some(Operand::Value(
                        self.builder
                            .build_int_truncate(v, dst_ll, "trunc")
                            .unwrap()
                            .into(),
                    ));
                }
                let src_signed = as_prim(self.typeck_results.tys(), src_ty)
                    .map(is_signed_prim)
                    .unwrap_or(false);
                let v = if src_signed {
                    self.builder.build_int_s_extend(v, dst_ll, "sext").unwrap()
                } else {
                    self.builder.build_int_z_extend(v, dst_ll, "zext").unwrap()
                };
                Some(Operand::Value(v.into()))
            }
            crate::typeck::CastKind::PtrToPtr => {
                // Reachable when src == dst was *not* the Identity
                // short-circuit (impossible in practice — a same-TyId
                // PtrToPtr is Identity), kept for completeness.
                Some(inner_op)
            }
            // spec/19_FN_PTR.md §5: Fn-Fn casts are subtype-validated
            // at typeck (`Obligation::Cast` discharge routes them through
            // `discharge_subtype`). Codegen is a no-op — LLVM `ptr` is
            // opaque, and the variance / `is_extern_c` rules are typeck-
            // level invariants.
            crate::typeck::CastKind::FnSubtype => Some(inner_op),
            crate::typeck::CastKind::Reject => unreachable!(
                "emit_cast: typeck E0274 should have rejected this cast \
                 ({} as {})",
                self.typeck_results.tys().render(src_ty),
                self.typeck_results.tys().render(dst_ty),
            ),
        }
    }
}
