//! HIR + TypeckResults → LLVM `Module`. Two-pass: declare every fn,
//! then define each body. Each fn body uses alloca + load/store for
//! locals (mem2reg-friendly canonical form).

use std::cell::Cell;
use std::collections::HashMap;

use index_vec::IndexVec;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, IntValue, PointerValue,
    UnnamedAddress,
};

use crate::hir::{
    FieldIdx, FnId, HBlockId, HElseArm, HExprId, HirExprKind, HirModule, LocalId, VariantIdx,
};
use crate::parser::ast::{AssignOp, BinOp, UnOp};
use crate::typeck::{AdtId, TyId, TyKind, TypeckResults};

use super::ty::{
    AdtLlTypes, as_prim, is_signed_prim, is_void_ret, lower_fn_type, lower_prim, lower_ty,
    prepare_adt_types,
};

/// Lower an entire `HirModule` to an LLVM `Module`. Verifies before
/// returning; verifier failures panic.
pub fn codegen<'ctx>(
    ctx: &'ctx Context,
    hir: &HirModule,
    typeck_results: &TypeckResults,
    module_name: &str,
) -> Module<'ctx> {
    let module = ctx.create_module(module_name);
    let builder = ctx.create_builder();

    // Phase A — pre-allocate one LLVM struct type per ADT (opaque),
    // then phase B set_body recursively. Mirrors typeck's phase 0/0.5.
    let adt_ll = prepare_adt_types(ctx, typeck_results);

    // Phase 1 — declare functions. `Module::add_function` uses interior
    // mutability, so fn_decls is filled before constructing Codegen and
    // Codegen never needs &mut.
    let mut fn_decls: IndexVec<FnId, FunctionValue<'ctx>> =
        IndexVec::with_capacity(hir.fns.len());
    for (_fid, hir_fn) in hir.fns.iter_enumerated() {
        let sig = typeck_results.fn_sig(_fid);
        let fn_ty = lower_fn_type(ctx, typeck_results.tys(), &adt_ll, sig);
        let fnv = module.add_function(&hir_fn.name, fn_ty, None);
        for (i, pv) in fnv.get_param_iter().enumerate() {
            let lid = hir_fn.params[i];
            pv.set_name(&hir.locals[lid].name);
        }
        fn_decls.push(fnv);
    }

    let cg = Codegen {
        ctx,
        module,
        builder,
        hir,
        typeck_results,
        fn_decls,
        adt_ll,
        str_counter: Cell::new(0),
    };

    // Pass 2 — define. Each fn body gets a fresh FnCodegenContext on
    // the stack inside `lower_fn`. Foreign fns (`body == None`) skip this
    // pass entirely; the FunctionValue from pass 1 stays as a `declare`
    // line in the IR with no basic blocks.
    for (fid, hir_fn) in cg.hir.fns.iter_enumerated() {
        if hir_fn.body.is_some() {
            cg.lower_fn(fid);
        }
    }

    if let Err(msg) = cg.module.verify() {
        panic!(
            "LLVM verifier rejected codegen output:\n{}",
            msg.to_string()
        );
    }
    cg.module
}

struct Codegen<'a, 'ctx> {
    ctx: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    hir: &'a HirModule,
    typeck_results: &'a TypeckResults,
    fn_decls: IndexVec<FnId, FunctionValue<'ctx>>,
    /// LLVM struct type per `AdtId`, populated up front by
    /// `prepare_adt_types`. All later `lower_ty` / `lower_fn_type` calls
    /// thread this in.
    adt_ll: AdtLlTypes<'ctx>,
    /// Suffix counter for emitted string-literal globals (`@.str.0`,
    /// `@.str.1`, …). Inkwell uses interior mutability everywhere so the
    /// rest of `Codegen` lives behind `&self`; we do the same here.
    str_counter: Cell<u32>,
}

/// Per-fn transient state. Lives on the stack for the duration of one fn
/// body — created in `lower_fn` and threaded as a `&mut` parameter through
/// the emit methods. Plain data; no methods of its own.
struct FnCodegenContext<'ctx> {
    fn_id: FnId,
    fn_value: FunctionValue<'ctx>,
    /// Dedicated alloca block for this fn. `allocas:` is the entry block
    /// (first appended) and is terminated by `br label %body`. All allocas
    /// land before that terminator; mem2reg sees them as entry-block
    /// allocas and promotes them. The extra `br` is removed by
    /// `simplifycfg` in optimized builds.
    allocas_bb: BasicBlock<'ctx>,
    locals: HashMap<LocalId, PointerValue<'ctx>>,
}

impl<'a, 'ctx> Codegen<'a, 'ctx> {
    // ---------- helpers ----------

    /// Whether the builder's current basic block already has a terminator.
    /// Used to short-circuit emission after `return`/`br`.
    fn is_terminated(&self) -> bool {
        self.builder
            .get_insert_block()
            .and_then(|bb| bb.get_terminator())
            .is_some()
    }

    /// Build an alloca in the current fn's dedicated `allocas` block.
    /// Always inserts right before the block's terminator (the `br` to
    /// `body`), so allocas stay grouped at the top of the entry block in
    /// emission order.
    fn alloca_in_entry(
        &self,
        fx: &FnCodegenContext<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        name: &str,
    ) -> PointerValue<'ctx> {
        let terminator = fx
            .allocas_bb
            .get_terminator()
            .expect("allocas bb missing terminator");
        let saved = self.builder.get_insert_block();
        self.builder.position_before(&terminator);
        let slot = self.builder.build_alloca(ty, name).unwrap();
        if let Some(bb) = saved {
            self.builder.position_at_end(bb);
        }
        slot
    }

    fn ty_of(&self, eid: HExprId) -> TyId {
        self.typeck_results.type_of_expr(eid)
    }

    fn local_ty(&self, lid: LocalId) -> TyId {
        self.typeck_results.type_of_local(lid)
    }

    // ---------- per-fn entry ----------

    fn lower_fn(&self, fid: FnId) {
        let fnv = self.fn_decls[fid];

        // Two blocks at start: `allocas:` (the entry block) holds only
        // alloca instructions and falls through to `body:` via an
        // unconditional branch. All real emission happens in `body`.
        let allocas_bb = self.ctx.append_basic_block(fnv, "allocas");
        let body_bb = self.ctx.append_basic_block(fnv, "body");
        self.builder.position_at_end(allocas_bb);
        self.builder.build_unconditional_branch(body_bb).unwrap();
        self.builder.position_at_end(body_bb);

        let mut fx = FnCodegenContext {
            fn_id: fid,
            fn_value: fnv,
            allocas_bb,
            locals: HashMap::new(),
        };

        // Alloca slots for params and store the incoming arg values.
        let hir_fn = &self.hir.fns[fid];
        for (i, &lid) in hir_fn.params.iter().enumerate() {
            let pty = self.local_ty(lid);
            let llty = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, pty);
            let slot = self.alloca_in_entry(
                &fx,
                llty,
                &format!("{}.{}.slot", self.hir.locals[lid].name, lid.raw()),
            );
            let arg = fnv.get_nth_param(i as u32).expect("param exists");
            self.builder.build_store(slot, arg).unwrap();
            fx.locals.insert(lid, slot);
        }

        // `lower_fn` is only called for body-having fns, so unwrap is sound.
        let body_id = hir_fn
            .body
            .expect("lower_fn called on foreign fn — codegen should have skipped");
        let body_val = self.emit_block(&mut fx, body_id);

        if !self.is_terminated() {
            let sig = self.typeck_results.fn_sig(fid);
            if is_void_ret(self.typeck_results.tys(), sig.ret) {
                self.builder.build_return(None).unwrap();
            } else {
                let v = body_val.expect("non-void fn body produced no value");
                self.builder.build_return(Some(&v)).unwrap();
            }
        }
    }

    // ---------- blocks ----------

    fn emit_block(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        bid: HBlockId,
    ) -> Option<BasicValueEnum<'ctx>> {
        // Clone the items vec so we don't borrow self.hir while emitting.
        let block = self.hir.blocks[bid].clone();
        let last_idx = block.items.len().checked_sub(1);
        let mut last_val: Option<BasicValueEnum<'ctx>> = None;
        for (i, item) in block.items.iter().enumerate() {
            if self.is_terminated() {
                return None;
            }
            let v = self.emit_expr(fx, item.expr);
            if Some(i) == last_idx && !item.has_semi {
                last_val = v;
            }
        }
        last_val
    }

    // ---------- expressions ----------

    fn emit_expr(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        eid: HExprId,
    ) -> Option<BasicValueEnum<'ctx>> {
        if self.is_terminated() {
            return None;
        }
        let kind = self.hir.exprs[eid].kind.clone();
        match kind {
            HirExprKind::IntLit(n) => Some(self.emit_int_lit(eid, n)),
            HirExprKind::BoolLit(b) => Some(self.ctx.bool_type().const_int(b as u64, false).into()),
            HirExprKind::CharLit(c) => Some(self.ctx.i8_type().const_int(c as u64, false).into()),
            HirExprKind::Local(lid) => Some(self.emit_local_load(fx, lid)),
            HirExprKind::Unary { op, expr } => self.emit_unary(fx, op, expr),
            HirExprKind::Binary { op, lhs, rhs } => self.emit_binary(fx, eid, op, lhs, rhs),
            HirExprKind::Assign { op, target, rhs } => {
                self.emit_assign(fx, op, target, rhs);
                None
            }
            HirExprKind::Call { callee, args } => self.emit_call(fx, callee, &args),
            HirExprKind::Cast { expr, ty: _ } => Some(self.emit_cast(fx, eid, expr)),
            HirExprKind::If {
                cond,
                then_block,
                else_arm,
            } => self.emit_if(fx, eid, cond, then_block, else_arm),
            HirExprKind::Block(bid) => self.emit_block(fx, bid),
            HirExprKind::Return(val) => {
                self.emit_return(fx, val);
                None
            }
            HirExprKind::Let { local, init } => {
                self.emit_let(fx, local, init);
                None
            }
            HirExprKind::Fn(_) => {
                panic!("v0 codegen: fn references are only valid as call targets")
            }
            HirExprKind::StrLit(s) => Some(self.emit_str_lit(&s)),
            HirExprKind::Index { .. } => {
                panic!("v0 codegen: index should have been rejected at typeck")
            }
            HirExprKind::Field { base, name } => self.emit_field(fx, base, &name),
            HirExprKind::StructLit { adt, fields } => {
                Some(self.emit_struct_lit(fx, adt, &fields))
            }
            // `&place` / `&mut place` — the slot pointer that `lvalue`
            // already produces for assignment targets *is* the value we
            // want here. LLVM `ptr` is mutability-agnostic; the
            // mutability tag was a typeck concept only. See
            // spec/10_ADDRESS_OF.md "Codegen".
            HirExprKind::AddrOf { mutability: _, expr } => {
                let ptr = self.lvalue(fx, expr);
                Some(ptr.into())
            }
            HirExprKind::Unresolved(_) | HirExprKind::Poison => {
                panic!("v0 codegen: poisoned expr reached codegen")
            }
        }
    }

    /// Emit a private constant global holding `s` followed by a `\0`
    /// terminator and return a pointer to its first byte. The value's
    /// type is opaque `ptr` (LLVM 15+); no GEP needed since the global
    /// itself is already a pointer.
    fn emit_str_lit(&self, s: &str) -> BasicValueEnum<'ctx> {
        let mut bytes: Vec<u8> = s.as_bytes().to_vec();
        bytes.push(0); // C-style NUL terminator (see spec/07_POINTER.md).
        let i8_ty = self.ctx.i8_type();
        let const_arr = i8_ty.const_array(
            &bytes
                .iter()
                .map(|&b| i8_ty.const_int(b as u64, false))
                .collect::<Vec<_>>(),
        );
        let arr_ty = i8_ty.array_type(bytes.len() as u32);

        let idx = self.str_counter.get();
        self.str_counter.set(idx + 1);
        let name = format!(".str.{idx}");

        let global = self.module.add_global(arr_ty, None, &name);
        global.set_linkage(Linkage::Private);
        global.set_constant(true);
        global.set_unnamed_address(UnnamedAddress::Global);
        global.set_initializer(&const_arr);

        global.as_pointer_value().into()
    }

    fn emit_int_lit(&self, eid: HExprId, n: u64) -> BasicValueEnum<'ctx> {
        let ty = self.ty_of(eid);
        match self.typeck_results.tys().kind(ty) {
            TyKind::Prim(p) => lower_prim(self.ctx, *p).const_int(n, false).into(),
            other => panic!("int lit had non-prim type {:?}", other),
        }
    }

    fn emit_local_load(
        &self,
        fx: &FnCodegenContext<'ctx>,
        lid: LocalId,
    ) -> BasicValueEnum<'ctx> {
        let slot = fx.locals[&lid];
        let ty = self.local_ty(lid);
        let llty = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, ty);
        self.builder.build_load(llty, slot, "load").unwrap()
    }

    fn lvalue(&self, fx: &FnCodegenContext<'ctx>, eid: HExprId) -> PointerValue<'ctx> {
        match self.hir.exprs[eid].kind.clone() {
            HirExprKind::Local(lid) => fx.locals[&lid],
            HirExprKind::Field { base, name } => {
                let base_ptr = self.lvalue(fx, base);
                let base_ty = self.ty_of(base);
                let base_ll = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, base_ty);
                let aid = match self.typeck_results.tys().kind(base_ty) {
                    TyKind::Adt(aid) => *aid,
                    other => panic!("Field base lvalue: non-Adt type {:?}", other),
                };
                let field_idx = self.field_index(aid, &name);
                self.builder
                    .build_struct_gep(base_ll, base_ptr, field_idx, "fld.gep")
                    .unwrap()
            }
            other => panic!("v0 codegen: non-lvalue assignment target {:?}", other),
        }
    }

    /// Position of `name` in `adts[aid]`'s sole variant. Typeck has
    /// already validated the field exists; a miss here is an ICE.
    fn field_index(&self, aid: AdtId, name: &str) -> u32 {
        let adt = self.typeck_results.adt_def(aid);
        adt.variants[VariantIdx::from_raw(0)]
            .fields
            .iter()
            .position(|f| f.name == name)
            .expect("typeck guaranteed field exists") as u32
    }

    fn emit_field(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        base: HExprId,
        name: &str,
    ) -> Option<BasicValueEnum<'ctx>> {
        let base_expr = &self.hir.exprs[base];
        let base_ty = self.ty_of(base);
        let aid = match self.typeck_results.tys().kind(base_ty) {
            TyKind::Adt(aid) => *aid,
            other => panic!("Field rvalue: non-Adt base type {:?}", other),
        };
        let field_idx = self.field_index(aid, name);
        let field_ty = {
            let adt = self.typeck_results.adt_def(aid);
            adt.variants[VariantIdx::from_raw(0)].fields[FieldIdx::from_raw(field_idx)].ty
        };
        let field_ll = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, field_ty);

        if base_expr.is_place {
            // Place path — single-field load via `getelementptr`, no whole-struct copy.
            let base_ptr = self.lvalue(fx, base);
            let base_ll = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, base_ty);
            let gep = self
                .builder
                .build_struct_gep(base_ll, base_ptr, field_idx, "fld.gep")
                .unwrap();
            Some(self.builder.build_load(field_ll, gep, "fld.load").unwrap())
        } else {
            // Value path — base is an rvalue aggregate; pull the field
            // out via extractvalue, no memory traffic.
            let agg = self.emit_expr(fx, base)?;
            let val = self
                .builder
                .build_extract_value(agg.into_struct_value(), field_idx, "fld")
                .unwrap();
            Some(val)
        }
    }

    /// Build a struct value as an SSA aggregate via `insertvalue`. The
    /// HIR-side field list isn't necessarily in declaration order; we
    /// walk the declared fields and find each provided value by name.
    /// Typeck has already validated the field set, so missing/extra/
    /// duplicate are unreachable at this point.
    fn emit_struct_lit(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        adt: crate::hir::HAdtId,
        fields: &[crate::hir::HirStructLitField],
    ) -> BasicValueEnum<'ctx> {
        let aid = AdtId::from_raw(adt.raw());
        let llty = self.adt_ll[aid];
        let mut agg = llty.get_undef();

        let adt_def = self.typeck_results.adt_def(aid);
        for (i, declared) in adt_def.variants[VariantIdx::from_raw(0)]
            .fields
            .iter()
            .enumerate()
        {
            let provided = fields
                .iter()
                .find(|p| p.name == declared.name)
                .expect("typeck guaranteed all fields are provided");
            let value = self
                .emit_expr(fx, provided.value)
                .expect("struct literal field produced no value");
            let new_agg = self
                .builder
                .build_insert_value(agg, value, i as u32, "lit.fld")
                .unwrap();
            agg = new_agg.into_struct_value();
        }
        agg.as_basic_value_enum()
    }

    // ---------- unary / binary ----------

    fn emit_unary(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        op: UnOp,
        expr: HExprId,
    ) -> Option<BasicValueEnum<'ctx>> {
        let v = self.emit_expr(fx, expr)?.into_int_value();
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
        };
        Some(res.into())
    }

    fn emit_binary(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        eid: HExprId,
        op: BinOp,
        lhs: HExprId,
        rhs: HExprId,
    ) -> Option<BasicValueEnum<'ctx>> {
        // Short-circuit operators have their own control-flow shape.
        if matches!(op, BinOp::And | BinOp::Or) {
            return Some(self.emit_short_circuit(fx, op, lhs, rhs));
        }

        let lt = self.ty_of(lhs);
        let l = self.emit_expr(fx, lhs)?.into_int_value();
        let r = self.emit_expr(fx, rhs)?.into_int_value();
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
        Some(v.into())
    }

    /// LLVM requires shift amounts to match the lhs's int type.
    fn coerce_shift_amt(
        &self,
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
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        op: BinOp,
        lhs: HExprId,
        rhs: HExprId,
    ) -> BasicValueEnum<'ctx> {
        let l = self
            .emit_expr(fx, lhs)
            .expect("logical lhs produced no value")
            .into_int_value();
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
        let r = self
            .emit_expr(fx, rhs)
            .expect("logical rhs produced no value")
            .into_int_value();
        let rhs_end_bb = self.builder.get_insert_block().unwrap();
        if !self.is_terminated() {
            self.builder.build_unconditional_branch(end_bb).unwrap();
        }

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
        phi.add_incoming(&[(&short_circuit_val, lhs_end_bb), (&r, rhs_end_bb)]);
        phi.as_basic_value()
    }

    fn emit_assign(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        op: AssignOp,
        target: HExprId,
        rhs: HExprId,
    ) {
        let slot = self.lvalue(fx, target);
        let target_ty = self.ty_of(target);
        let llty = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, target_ty);

        let r = self
            .emit_expr(fx, rhs)
            .expect("assign rhs produced no value")
            .into_int_value();

        let new_val = if matches!(op, AssignOp::Eq) {
            r
        } else {
            let cur = self
                .builder
                .build_load(llty, slot, "asgn.cur")
                .unwrap()
                .into_int_value();
            let signed = as_prim(self.typeck_results.tys(), target_ty)
                .map(is_signed_prim)
                .unwrap_or(false);
            match op {
                AssignOp::Add => self.builder.build_int_add(cur, r, "asgn.add").unwrap(),
                AssignOp::Sub => self.builder.build_int_sub(cur, r, "asgn.sub").unwrap(),
                AssignOp::Mul => self.builder.build_int_mul(cur, r, "asgn.mul").unwrap(),
                AssignOp::Div if signed => self
                    .builder
                    .build_int_signed_div(cur, r, "asgn.sdiv")
                    .unwrap(),
                AssignOp::Div => self
                    .builder
                    .build_int_unsigned_div(cur, r, "asgn.udiv")
                    .unwrap(),
                AssignOp::Rem if signed => self
                    .builder
                    .build_int_signed_rem(cur, r, "asgn.srem")
                    .unwrap(),
                AssignOp::Rem => self
                    .builder
                    .build_int_unsigned_rem(cur, r, "asgn.urem")
                    .unwrap(),
                AssignOp::BitAnd => self.builder.build_and(cur, r, "asgn.and").unwrap(),
                AssignOp::BitOr => self.builder.build_or(cur, r, "asgn.or").unwrap(),
                AssignOp::BitXor => self.builder.build_xor(cur, r, "asgn.xor").unwrap(),
                AssignOp::Shl => {
                    let r = self.coerce_shift_amt(r, cur.get_type());
                    self.builder.build_left_shift(cur, r, "asgn.shl").unwrap()
                }
                AssignOp::Shr => {
                    let r = self.coerce_shift_amt(r, cur.get_type());
                    self.builder
                        .build_right_shift(cur, r, signed, "asgn.shr")
                        .unwrap()
                }
                AssignOp::Eq => unreachable!(),
            }
        };

        self.builder.build_store(slot, new_val).unwrap();
    }

    // ---------- calls ----------

    fn emit_call(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        callee_eid: HExprId,
        args: &[HExprId],
    ) -> Option<BasicValueEnum<'ctx>> {
        let HirExprKind::Fn(fid) = self.hir.exprs[callee_eid].kind.clone() else {
            panic!("v0 codegen: callee must be a direct fn reference")
        };
        let fnv = self.fn_decls[fid];

        let mut arg_vals: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
        for &a in args {
            let v = self.emit_expr(fx, a).expect("call arg produced no value");
            arg_vals.push(v.into());
        }

        let call = self.builder.build_call(fnv, &arg_vals, "call").unwrap();
        call.try_as_basic_value().left()
    }

    // ---------- casts ----------

    fn emit_cast(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        eid: HExprId,
        inner: HExprId,
    ) -> BasicValueEnum<'ctx> {
        let dst_ty = self.ty_of(eid);
        let src_ty = self.ty_of(inner);
        let v = self
            .emit_expr(fx, inner)
            .expect("cast operand produced no value")
            .into_int_value();
        let dst_prim = as_prim(self.typeck_results.tys(), dst_ty)
            .expect("v0: cast target must be a primitive");
        let dst_ll = lower_prim(self.ctx, dst_prim);
        let src_w = v.get_type().get_bit_width();
        let dst_w = dst_ll.get_bit_width();
        if src_w == dst_w {
            return v.into();
        }
        if dst_w < src_w {
            return self
                .builder
                .build_int_truncate(v, dst_ll, "trunc")
                .unwrap()
                .into();
        }
        let src_signed = as_prim(self.typeck_results.tys(), src_ty)
            .map(is_signed_prim)
            .unwrap_or(false);
        let v = if src_signed {
            self.builder.build_int_s_extend(v, dst_ll, "sext").unwrap()
        } else {
            self.builder.build_int_z_extend(v, dst_ll, "zext").unwrap()
        };
        v.into()
    }

    // ---------- if / else ----------

    fn emit_if(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        eid: HExprId,
        cond: HExprId,
        then_block: HBlockId,
        else_arm: Option<HElseArm>,
    ) -> Option<BasicValueEnum<'ctx>> {
        let cond_v = self
            .emit_expr(fx, cond)
            .expect("if cond produced no value")
            .into_int_value();
        let parent = fx.fn_value;
        let then_bb = self.ctx.append_basic_block(parent, "if.then");
        let else_bb = self.ctx.append_basic_block(parent, "if.else");
        let merge_bb = self.ctx.append_basic_block(parent, "if.end");

        self.builder
            .build_conditional_branch(cond_v, then_bb, else_bb)
            .unwrap();

        // Materialize a result slot iff the if expression has a real
        // value type. For unit / never / divergent ifs we don't.
        let if_ty = self.ty_of(eid);
        let result_slot = if !is_void_ret(self.typeck_results.tys(), if_ty) {
            let llty = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, if_ty);
            Some(self.alloca_in_entry(fx, llty, "if.slot"))
        } else {
            None
        };

        // then arm
        self.builder.position_at_end(then_bb);
        let then_val = self.emit_block(fx, then_block);
        if !self.is_terminated() {
            if let (Some(slot), Some(v)) = (result_slot, then_val) {
                self.builder.build_store(slot, v).unwrap();
            }
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        // else arm
        self.builder.position_at_end(else_bb);
        match else_arm {
            Some(HElseArm::Block(bid)) => {
                let else_val = self.emit_block(fx, bid);
                if !self.is_terminated() {
                    if let (Some(slot), Some(v)) = (result_slot, else_val) {
                        self.builder.build_store(slot, v).unwrap();
                    }
                    self.builder.build_unconditional_branch(merge_bb).unwrap();
                }
            }
            Some(HElseArm::If(else_eid)) => {
                let else_val = self.emit_expr(fx, else_eid);
                if !self.is_terminated() {
                    if let (Some(slot), Some(v)) = (result_slot, else_val) {
                        self.builder.build_store(slot, v).unwrap();
                    }
                    self.builder.build_unconditional_branch(merge_bb).unwrap();
                }
            }
            None => {
                self.builder.build_unconditional_branch(merge_bb).unwrap();
            }
        }

        self.builder.position_at_end(merge_bb);

        // If both arms diverged, the merge block has no predecessors —
        // make it explicitly unreachable so the verifier is happy.
        if merge_bb_has_no_preds(merge_bb) {
            self.builder.build_unreachable().unwrap();
            return None;
        }

        match result_slot {
            Some(slot) => {
                let llty = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, if_ty);
                Some(self.builder.build_load(llty, slot, "if.val").unwrap())
            }
            None => None,
        }
    }

    // ---------- return ----------

    fn emit_return(&self, fx: &mut FnCodegenContext<'ctx>, val: Option<HExprId>) {
        let sig = self.typeck_results.fn_sig(fx.fn_id).clone();
        let ret_ty = sig.ret;
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

        match val.and_then(|eid| self.emit_expr(fx, eid)) {
            Some(v) => {
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

    // ---------- let ----------

    fn emit_let(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        lid: LocalId,
        init: Option<HExprId>,
    ) {
        let ty = self.local_ty(lid);

        let local = &self.hir.locals[lid];

        let llty = if is_void_ret(self.typeck_results.tys(), ty) {
            panic!("void type for local {}", local.name);
        } else {
            lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, ty)
        };
        let slot = self.alloca_in_entry(fx, llty, &format!("{}.{}.slot", local.name, lid.raw()));
        fx.locals.insert(lid, slot);
        if let Some(init_eid) = init {
            if let Some(v) = self.emit_expr(fx, init_eid) {
                self.builder.build_store(slot, v).unwrap();
            }
        }
    }
}

fn merge_bb_has_no_preds(bb: BasicBlock<'_>) -> bool {
    bb.get_first_use().is_none()
}
