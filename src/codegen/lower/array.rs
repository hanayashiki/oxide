//! Array-shaped lowering: literals (`emit_array_lit`), index access
//! (`emit_index_rvalue`, `emit_index_place`), bounds-check / trap
//! plumbing, and the runtime-fill repeat loop.

use inkwell::IntPredicate;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::hir::{HExprId, HirArrayLit, HirConst};
use crate::typeck::{TyId, TyKind};

use super::{Codegen, FnCodegenContext, Operand};

impl<'a, 'ctx> Codegen<'a, 'ctx> {
    /// Lazily declare `void @llvm.trap()` and return its FunctionValue.
    /// First call inserts the declaration into the module; subsequent
    /// calls hit the cache.
    fn get_or_declare_trap(&self) -> FunctionValue<'ctx> {
        if let Some(fv) = self.llvm_trap.get() {
            return fv;
        }
        let fn_ty = self.ctx.void_type().fn_type(&[], false);
        let fv = self.module.add_function("llvm.trap", fn_ty, None);
        self.llvm_trap.set(Some(fv));
        fv
    }

    /// Bounds-check `idx` against the static length `n`. Builds:
    ///   %cmp = icmp uge i64 %idx, N
    ///   br %cmp, %bounds.trap, %bounds.ok
    ///   bounds.trap: call @llvm.trap(); unreachable
    ///   bounds.ok:  ; builder positioned here on return
    /// Per spec/09_ARRAY.md the guard is always emitted; LLVM folds
    /// const-known-safe cases at any opt level.
    fn emit_bounds_check(&mut self, fx: &FnCodegenContext<'ctx>, idx: IntValue<'ctx>, n: u64) {
        let i64_ty = self.ctx.i64_type();
        let n_v = i64_ty.const_int(n, false);
        let cmp = self
            .builder
            .build_int_compare(IntPredicate::UGE, idx, n_v, "bounds.cmp")
            .unwrap();
        let parent = fx.fn_value;
        let trap_bb = self.ctx.append_basic_block(parent, "bounds.trap");
        let ok_bb = self.ctx.append_basic_block(parent, "bounds.ok");
        self.builder
            .build_conditional_branch(cmp, trap_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(trap_bb);
        let trap = self.get_or_declare_trap();
        self.builder.build_call(trap, &[], "trap").unwrap();
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);
    }

    /// Runtime-loop fill of `slot: [N x T]` with `init_v` repeated `n`
    /// times. Per Q2 decision: no memset fast-path for `[0; N]` —
    /// always emit the loop and let LLVM coalesce. Three-bb shape
    /// modeled after `emit_short_circuit`.
    fn emit_repeat_loop(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        slot: PointerValue<'ctx>,
        arr_ll: BasicTypeEnum<'ctx>,
        init_v: BasicValueEnum<'ctx>,
        n: u64,
    ) {
        let i64_ty = self.ctx.i64_type();
        let zero = i64_ty.const_zero();
        let one = i64_ty.const_int(1, false);
        let n_v = i64_ty.const_int(n, false);
        let parent = fx.fn_value;
        let entry_bb = self.builder.get_insert_block().unwrap();
        let header_bb = self.ctx.append_basic_block(parent, "repeat.header");
        let body_bb = self.ctx.append_basic_block(parent, "repeat.body");
        let end_bb = self.ctx.append_basic_block(parent, "repeat.end");
        self.builder.build_unconditional_branch(header_bb).unwrap();

        self.builder.position_at_end(header_bb);
        let phi = self.builder.build_phi(i64_ty, "i").unwrap();
        let i_v = phi.as_basic_value().into_int_value();
        let cmp = self
            .builder
            .build_int_compare(IntPredicate::ULT, i_v, n_v, "cont")
            .unwrap();
        self.builder
            .build_conditional_branch(cmp, body_bb, end_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let gep = unsafe {
            self.builder
                .build_in_bounds_gep(arr_ll, slot, &[zero, i_v], "rep.gep")
                .unwrap()
        };
        self.builder.build_store(gep, init_v).unwrap();
        let i_next = self.builder.build_int_add(i_v, one, "i.next").unwrap();
        let body_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(header_bb).unwrap();
        phi.add_incoming(&[(&zero, entry_bb), (&i_next, body_end)]);

        self.builder.position_at_end(end_bb);
    }

    /// Lower an array literal to a fresh alloca-backed place. Returns
    /// `Operand::Place(slot)`; downstream consumers (let-init, fn-arg,
    /// Index, …) see this as the literal's place form. Per
    /// spec/09_ARRAY.md "ArrayLit shape" (Q1 in the codegen plan):
    /// alloca + GEP+store, no SSA aggregate.
    pub(super) fn emit_array_lit(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        lit: HirArrayLit,
        array_ty: TyId,
    ) -> Option<Operand<'ctx>> {
        let arr_ll = self.lower_ty(array_ty);
        let slot = self.alloca_in_entry(fx, arr_ll, "lit.slot");
        let i64_ty = self.ctx.i64_type();
        let zero = i64_ty.const_zero();
        match lit {
            HirArrayLit::Elems(es) => {
                for (i, eid) in es.into_iter().enumerate() {
                    let elem_ty = self.ty_of(fx, eid);
                    let elem_op = self.emit_expr(fx, eid)?;
                    let v = elem_op.load_value(self, elem_ty, "load");
                    let idx_v = i64_ty.const_int(i as u64, false);
                    let gep = unsafe {
                        self.builder
                            .build_in_bounds_gep(arr_ll, slot, &[zero, idx_v], "lit.gep")
                            .unwrap()
                    };
                    self.builder.build_store(gep, v).unwrap();
                }
            }
            HirArrayLit::Repeat {
                init,
                len: HirConst::Lit(n),
            } => {
                let init_ty = self.ty_of(fx, init);
                let init_op = self.emit_expr(fx, init)?;
                let init_v = init_op.load_value(self, init_ty, "load");
                self.emit_repeat_loop(fx, slot, arr_ll, init_v, n);
            }
            HirArrayLit::Repeat {
                len: HirConst::Error,
                ..
            } => unreachable!(
                "HirConst::Error in repeat-literal length unreachable in v0 (parser rejects non-IntLit)"
            ),
        }
        Some(Operand::Place(slot))
    }

    /// Index rvalue — `base[idx]` as a value-producing expression.
    /// Dispatches on the base's resolved typeck kind:
    ///
    ///   - `Array(elem, Some(n))`        place-form base; bounds check;
    ///                                   GEP `[N x T], ptr, 0, idx`; load.
    ///   - `Ptr(Array(elem, Some(n)),_)` value-form base; bounds check;
    ///                                   same GEP shape; load.
    ///   - `Ptr(Array(elem, None),_)`    value-form base; flat element-stride
    ///                                   GEP `T, ptr, idx`; **no bounds
    ///                                   check** (the unsized form is the
    ///                                   deliberate opt-out). Load.
    ///
    /// See spec/09_ARRAY.md "Index lowering".
    pub(super) fn emit_index_rvalue(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        base_eid: HExprId,
        idx_eid: HExprId,
    ) -> Option<Operand<'ctx>> {
        let (elt_ptr, elem_ty) = self.emit_index_place(fx, base_eid, idx_eid)?;
        // Array-typed elements stay in place form (slot ptr, not loaded
        // aggregate). Mirrors `Local` of array type and `emit_field`'s
        // Place path. Without this, nested indexing like `g[i][j]` would
        // load the inner `[N x T]` as an SSA aggregate and the outer
        // `emit_index_place` would try to `into_pointer_value()` it. See
        // spec/09_ARRAY.md "arrays-as-places everywhere".
        if self.is_sized_array(elem_ty) {
            return Some(Operand::Place(elt_ptr));
        }
        let elem_ll = self.lower_ty(elem_ty);
        Some(Operand::Value(
            self.builder
                .build_load(elem_ll, elt_ptr, "idx.load")
                .unwrap(),
        ))
    }

    /// Index lvalue — produces the element pointer (no load) for use as
    /// an assignment target or `&arr[i]` operand. Bounds check still
    /// fires for sized bases (writing past the end is just as wrong as
    /// reading past it). Returns `(elem_ptr, elem_ty_id)`.
    ///
    /// **Auto-deref through arbitrary `Ptr` depth.** Typeck's
    /// `auto_deref_ptr` strips *all* outer `Ptr` layers before checking
    /// for `Array` underneath, so `pp: *const *const [T; N]` accepts
    /// `pp[i]`. Codegen mirrors that: peel pointer levels via
    /// successive loads, then GEP the array. Each `Ptr` layer = one
    /// `load ptr`. The first level is implicit (`emit_expr` of a
    /// pointer-typed base already returns the loaded ptr value).
    pub(super) fn emit_index_place(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        base_eid: HExprId,
        idx_eid: HExprId,
    ) -> Option<(PointerValue<'ctx>, TyId)> {
        let base_ty = self.ty_of(fx, base_eid);
        let i64_ty = self.ctx.i64_type();
        let zero = i64_ty.const_zero();
        let ptr_ll = self.ctx.ptr_type(inkwell::AddressSpace::default());

        // Array base: Operand::Place — the slot ptr IS the array storage.
        // Ptr base: Operand::Value(PointerValue) — the loaded ptr value.
        // Both produce the pointer we need to index off of.
        let base_op = self.emit_expr(fx, base_eid)?;
        let base_v = match base_op {
            Operand::Place(p) => p,
            Operand::Value(v) => v.into_pointer_value(),
            Operand::Unit => unreachable!("typeck rejects index on ()"),
        };

        // Set up the loop. At entry, `cur_ptr` addresses either the
        // array storage (when base is an array place) or the next
        // pointer in a chain (when base is a pointer).
        let (mut cur_ptr, mut cur_ty) = match self.typeck_results.tys().kind(base_ty).clone() {
            TyKind::Array(_, _) => (base_v, base_ty),
            TyKind::Ptr(inner, _) => (base_v, inner),
            other => panic!(
                "v0 codegen: index base has non-indexable type; typeck should have rejected ({:?})",
                other
            ),
        };
        while let TyKind::Ptr(inner, _) = self.typeck_results.tys().kind(cur_ty).clone() {
            cur_ptr = self
                .builder
                .build_load(ptr_ll, cur_ptr, "deref")
                .unwrap()
                .into_pointer_value();
            cur_ty = inner;
        }

        let idx_ty = self.ty_of(fx, idx_eid);
        let idx_op = self.emit_expr(fx, idx_eid)?;
        let idx_v = idx_op.load_value(self, idx_ty, "load").into_int_value();

        match self.typeck_results.tys().kind(cur_ty).clone() {
            TyKind::Array(elem, Some(n)) => {
                self.emit_bounds_check(fx, idx_v, n);
                let arr_ll = self.lower_ty(cur_ty);
                let elt_ptr = unsafe {
                    self.builder
                        .build_in_bounds_gep(arr_ll, cur_ptr, &[zero, idx_v], "idx.gep")
                        .unwrap()
                };
                Some((elt_ptr, elem))
            }
            TyKind::Array(elem, None) => {
                let elem_ll = self.lower_ty(elem);
                let elt_ptr = unsafe {
                    self.builder
                        .build_in_bounds_gep(elem_ll, cur_ptr, &[idx_v], "idx.gep")
                        .unwrap()
                };
                Some((elt_ptr, elem))
            }
            other => panic!(
                "v0 codegen: non-array reached after auto-deref; typeck should have rejected ({:?})",
                other
            ),
        }
    }
}
