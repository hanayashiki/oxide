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
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, IntValue, PointerValue,
    UnnamedAddress,
};

use crate::hir::{
    FieldIdx, FnId, HBlockId, HElseArm, HExprId, HirArrayLit, HirConst, HirExprKind, HirModule,
    LocalId, VariantIdx,
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
    let mut fn_decls: IndexVec<FnId, FunctionValue<'ctx>> = IndexVec::with_capacity(hir.fns.len());
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
        llvm_trap: Cell::new(None),
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
    /// Cached `declare void @llvm.trap()` so each module emits the
    /// declaration at most once. Populated lazily by
    /// `get_or_declare_trap` on the first bounds-check site.
    llvm_trap: Cell<Option<FunctionValue<'ctx>>>,
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
    /// One frame per `Loop` whose body is currently being emitted.
    /// Pushed before emitting the body, popped after. `Break` / `Continue`
    /// read `last()` for their target. HIR-lower already filed
    /// E0263/E0264 if break/continue is outside a loop, so an empty
    /// stack here is an ICE-worthy invariant violation.
    /// See spec/13_LOOPS.md "FnCodegenContext gains a single LoopTargets
    /// shape".
    loop_targets: Vec<LoopTargets<'ctx>>,
}

/// Per-loop targets read by `emit_break` / `emit_continue`. Pushed onto
/// `FnCodegenContext::loop_targets` while lowering the loop body.
///
/// `end_bb` is where `break` jumps (always the loop's `loop.end` block —
/// no labels, so there's nowhere else to go). `continue_target_bb` is
/// the "top of the next iteration": `update_bb` if Some, else `cond_bb`
/// if Some, else `body_bb`. `result_slot` is `Some` only when the
/// loop's typeck'd type is a value type — see spec/13_LOOPS.md
/// "Result-slot rule".
#[derive(Copy, Clone)]
struct LoopTargets<'ctx> {
    end_bb: BasicBlock<'ctx>,
    continue_target_bb: BasicBlock<'ctx>,
    result_slot: Option<PointerValue<'ctx>>,
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

    /// `true` iff the resolved typeck kind is `Array(_, Some(_))`.
    /// Used at every "is this a place-form array?" boundary
    /// (let-init, fn-arg, fn-return, Local/Field/Index dispatch).
    fn is_sized_array(&self, ty: TyId) -> bool {
        matches!(
            self.typeck_results.tys().kind(ty),
            TyKind::Array(_, Some(_))
        )
    }

    // ---------- array helpers ----------

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
    fn emit_bounds_check(&self, fx: &FnCodegenContext<'ctx>, idx: IntValue<'ctx>, n: u64) {
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

    /// `llvm.memcpy` of an `[N x T]` aggregate from `src` to `dst`.
    /// Uses the LLVM type's static `size_of()` (an i64 const). Align is
    /// 1 — soundness-safe and lets LLVM choose the actual alignment via
    /// the operand types.
    fn emit_memcpy_array(&self, dst: PointerValue<'ctx>, src: PointerValue<'ctx>, array_ty: TyId) {
        let ll = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, array_ty);
        let size = ll.size_of().expect("array type has size_of");
        self.builder
            .build_memcpy(dst, 1, src, 1, size)
            .expect("build_memcpy");
    }

    /// Store `v` (a fully-evaluated rhs/init value) into `dst`,
    /// dispatching on `target_ty`:
    ///
    ///   - `Array(_, Some(_))` — `v` is a place pointer to the source
    ///     slot; memcpy `sizeof([N x T])` bytes from src to dst. Arrays
    ///     are place-form everywhere in our codegen (`emit_expr` of an
    ///     array-typed expression returns the slot pointer, not a
    ///     loaded `[N x T]` aggregate).
    ///   - `Array(_, None)` — defensive ICE. Typeck E0269 rejects
    ///     unsized arrays at every value position (`Param` / `Return` /
    ///     `Field` / `LetBinding`), so they cannot reach codegen as a
    ///     store target. See `src/typeck/error.rs::SizedPos`.
    ///   - everything else — `v` is an SSA value (primitive, bool,
    ///     pointer, struct). LLVM `store %T %v, ptr` is legal for any
    ///     `T`; inkwell reads the type off the value, the pointer is
    ///     opaque.
    ///
    /// Shared by `emit_let` (init-into-slot) and `emit_assign` (rhs-
    /// into-target-slot).
    fn emit_store_into_slot(
        &self,
        dst: PointerValue<'ctx>,
        target_ty: TyId,
        v: BasicValueEnum<'ctx>,
    ) {
        match self.typeck_results.tys().kind(target_ty) {
            TyKind::Array(_, Some(_)) => {
                self.emit_memcpy_array(dst, v.into_pointer_value(), target_ty);
            }
            TyKind::Array(_, None) => unreachable!(
                "ICE: unsized array as store target — typeck E0269 should have rejected"
            ),
            _ => {
                self.builder.build_store(dst, v).unwrap();
            }
        }
    }

    /// Runtime-loop fill of `slot: [N x T]` with `init_v` repeated `n`
    /// times. Per Q2 decision: no memset fast-path for `[0; N]` —
    /// always emit the loop and let LLVM coalesce. Three-bb shape
    /// modeled after `emit_short_circuit`.
    fn emit_repeat_loop(
        &self,
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
            loop_targets: Vec::new(),
        };

        // Alloca slots for params and store the incoming arg values.
        // Array-typed params skip the alloca+store: per Path A in
        // spec/09_ARRAY.md, `lower_fn_type` lowered the param to LLVM
        // `ptr` and the caller (`emit_call`) memcpy'd into a fresh slot
        // before passing. The incoming `ptr` IS the local's storage.
        let hir_fn = &self.hir.fns[fid];
        for (i, &lid) in hir_fn.params.iter().enumerate() {
            let pty = self.local_ty(lid);
            let arg = fnv.get_nth_param(i as u32).expect("param exists");
            if self.is_sized_array(pty) {
                fx.locals.insert(lid, arg.into_pointer_value());
                continue;
            }
            let llty = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, pty);
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
            let sig = self.typeck_results.fn_sig(fid);
            if is_void_ret(self.typeck_results.tys(), sig.ret) {
                self.builder.build_return(None).unwrap();
            } else if self.is_sized_array(sig.ret) {
                // Array-typed return — Path A: body produced a place ptr;
                // load the aggregate and return it by value. LLVM's
                // calling convention does the sret/register-return rewrite.
                let v = body_val.expect("non-void fn body produced no value");
                let arr_ll = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, sig.ret);
                let loaded = self
                    .builder
                    .build_load(arr_ll, v.into_pointer_value(), "ret.load")
                    .unwrap();
                self.builder.build_return(Some(&loaded)).unwrap();
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
            HirExprKind::Local(lid) => {
                // Array-typed locals stay in place form: return the slot ptr,
                // not the loaded aggregate. Consumers (let-init, fn-arg,
                // Index, Field) bridge as needed. See spec/09_ARRAY.md
                // "arrays-as-places everywhere".
                if self.is_sized_array(self.local_ty(lid)) {
                    Some(fx.locals[&lid].into())
                } else {
                    Some(self.emit_local_load(fx, lid))
                }
            }
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
            HirExprKind::Index { base, index } => self.emit_index_rvalue(fx, base, index),
            HirExprKind::ArrayLit(lit) => Some(self.emit_array_lit(fx, lit, self.ty_of(eid))),
            HirExprKind::Field { base, name } => self.emit_field(fx, base, &name),
            HirExprKind::StructLit { adt, fields } => Some(self.emit_struct_lit(fx, adt, &fields)),
            // `&place` / `&mut place` — the slot pointer that `lvalue`
            // already produces for assignment targets *is* the value we
            // want here. LLVM `ptr` is mutability-agnostic; the
            // mutability tag was a typeck concept only. See
            // spec/10_ADDRESS_OF.md "Codegen".
            HirExprKind::AddrOf {
                mutability: _,
                expr,
            } => {
                let ptr = self.lvalue(fx, expr);
                Some(ptr.into())
            }
            HirExprKind::Unresolved(_) | HirExprKind::Poison => {
                panic!("v0 codegen: poisoned expr reached codegen")
            }
            // `has_break` and `source` are read by typeck for the value
            // type and by HIR pretty-print respectively; codegen reads
            // `self.ty_of(eid)` directly to decide whether to allocate
            // a result slot, so it ignores them here.
            HirExprKind::Loop {
                init,
                cond,
                update,
                body,
                has_break: _,
                source: _,
            } => self.emit_loop(fx, eid, init, cond, update, body),
            HirExprKind::Break { expr } => {
                self.emit_break(fx, expr);
                None
            }
            HirExprKind::Continue => {
                self.emit_continue(fx);
                None
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

    fn emit_local_load(&self, fx: &FnCodegenContext<'ctx>, lid: LocalId) -> BasicValueEnum<'ctx> {
        let slot = fx.locals[&lid];
        let ty = self.local_ty(lid);
        let llty = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, ty);
        self.builder.build_load(llty, slot, "load").unwrap()
    }

    fn lvalue(&self, fx: &mut FnCodegenContext<'ctx>, eid: HExprId) -> PointerValue<'ctx> {
        match self.hir.exprs[eid].kind.clone() {
            HirExprKind::Local(lid) => fx.locals[&lid],
            HirExprKind::Index { base, index } => {
                // Bounds check fires here too — writing past the end is
                // as wrong as reading past it. Same auto-deref machinery
                // as the rvalue path.
                self.emit_index_place(fx, base, index).0
            }
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
            // Array-typed fields stay in place form: hand back the GEP'd
            // pointer instead of loading the aggregate. Mirrors the
            // arrays-as-places invariant for Locals.
            if self.is_sized_array(field_ty) {
                Some(gep.into())
            } else {
                Some(self.builder.build_load(field_ll, gep, "fld.load").unwrap())
            }
        } else {
            // Value path — base is an rvalue aggregate; pull the field
            // out via extractvalue, no memory traffic.
            let agg = self.emit_expr(fx, base)?;
            if self.is_sized_array(field_ty) {
                // Bridge: extract the array value, then alloca a temp slot
                // and store it so the result has place form. Rare path —
                // only fires when the struct itself is in SSA value form
                // (e.g., direct Field on a Call return), which v0 codegen
                // doesn't construct for ADTs containing arrays. Future
                // work: revisit if this trips.
                let arr_val = self
                    .builder
                    .build_extract_value(agg.into_struct_value(), field_idx, "fld.arr")
                    .unwrap();
                let slot = self.alloca_in_entry(fx, field_ll, "fld.arr.slot");
                self.builder.build_store(slot, arr_val).unwrap();
                Some(slot.into())
            } else {
                let val = self
                    .builder
                    .build_extract_value(agg.into_struct_value(), field_idx, "fld")
                    .unwrap();
                Some(val)
            }
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

    // ---------- array literals & indexing ----------

    /// Lower an array literal to a fresh alloca-backed place.
    /// Returns the slot pointer as a `BasicValueEnum::PointerValue`;
    /// downstream consumers (let-init, fn-arg, Index, …) see this as
    /// the literal's place form. Per spec/09_ARRAY.md "ArrayLit shape"
    /// (Q1 in the codegen plan): alloca + GEP+store, no SSA aggregate.
    fn emit_array_lit(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        lit: HirArrayLit,
        array_ty: TyId,
    ) -> BasicValueEnum<'ctx> {
        let arr_ll = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, array_ty);
        let slot = self.alloca_in_entry(fx, arr_ll, "lit.slot");
        let i64_ty = self.ctx.i64_type();
        let zero = i64_ty.const_zero();
        match lit {
            HirArrayLit::Elems(es) => {
                for (i, eid) in es.into_iter().enumerate() {
                    let v = self
                        .emit_expr(fx, eid)
                        .expect("array literal element produced no value");
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
                let init_v = self
                    .emit_expr(fx, init)
                    .expect("array repeat init produced no value");
                self.emit_repeat_loop(fx, slot, arr_ll, init_v, n);
            }
            HirArrayLit::Repeat {
                len: HirConst::Error,
                ..
            } => unreachable!(
                "HirConst::Error in repeat-literal length unreachable in v0 (parser rejects non-IntLit)"
            ),
        }
        slot.into()
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
    fn emit_index_rvalue(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        base_eid: HExprId,
        idx_eid: HExprId,
    ) -> Option<BasicValueEnum<'ctx>> {
        let (elt_ptr, elem_ty) = self.emit_index_place(fx, base_eid, idx_eid);
        let elem_ll = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, elem_ty);
        Some(
            self.builder
                .build_load(elem_ll, elt_ptr, "idx.load")
                .unwrap(),
        )
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
    fn emit_index_place(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        base_eid: HExprId,
        idx_eid: HExprId,
    ) -> (PointerValue<'ctx>, TyId) {
        let base_ty = self.ty_of(base_eid);
        let i64_ty = self.ctx.i64_type();
        let zero = i64_ty.const_zero();
        let tcx = self.typeck_results.tys();
        let ptr_ll = self.ctx.ptr_type(inkwell::AddressSpace::default());

        let base_v = self
            .emit_expr(fx, base_eid)
            .expect("index base produced no value")
            .into_pointer_value();

        // Set up the loop. At entry, `cur_ptr` addresses either the
        // array storage (when base is an array place) or the next
        // pointer in a chain (when base is a pointer).
        let (mut cur_ptr, mut cur_ty) = match tcx.kind(base_ty) {
            TyKind::Array(_, _) => (base_v, base_ty),
            TyKind::Ptr(inner, _) => (base_v, *inner),
            other => panic!(
                "v0 codegen: index base has non-indexable type; typeck should have rejected ({:?})",
                other
            ),
        };
        while let TyKind::Ptr(inner, _) = tcx.kind(cur_ty) {
            let next = *inner;
            cur_ptr = self
                .builder
                .build_load(ptr_ll, cur_ptr, "deref")
                .unwrap()
                .into_pointer_value();
            cur_ty = next;
        }

        let idx_v = self
            .emit_expr(fx, idx_eid)
            .expect("index produced no value")
            .into_int_value();

        match tcx.kind(cur_ty) {
            TyKind::Array(elem, Some(n)) => {
                let elem_ty = *elem;
                let n_v = *n;
                self.emit_bounds_check(fx, idx_v, n_v);
                let arr_ll = lower_ty(self.ctx, tcx, &self.adt_ll, cur_ty);
                let elt_ptr = unsafe {
                    self.builder
                        .build_in_bounds_gep(arr_ll, cur_ptr, &[zero, idx_v], "idx.gep")
                        .unwrap()
                };
                (elt_ptr, elem_ty)
            }
            TyKind::Array(elem, None) => {
                let elem_ty = *elem;
                let elem_ll = lower_ty(self.ctx, tcx, &self.adt_ll, elem_ty);
                let elt_ptr = unsafe {
                    self.builder
                        .build_in_bounds_gep(elem_ll, cur_ptr, &[idx_v], "idx.gep")
                        .unwrap()
                };
                (elt_ptr, elem_ty)
            }
            other => panic!(
                "v0 codegen: non-array reached after auto-deref; typeck should have rejected ({:?})",
                other
            ),
        }
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
        if let BinOp::And | BinOp::Or = op {
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
        let v = self
            .emit_expr(fx, rhs)
            .expect("assign rhs produced no value");

        let new_val = if let AssignOp::Eq = op {
            // Plain `=`: rhs passes through to the shared store
            // dispatch below.
            v
        } else {
            // Compound ops (+=, -=, *=, /=, %=, &=, |=, ^=, <<=, >>=) are
            // int-only by language design — the `Eq` arm above handles
            // plain `=`, and compound ops only apply to integer targets.
            let r = v.into_int_value();
            let llty = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, target_ty);
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
                AssignOp::Eq => unreachable!("covered by the outer if-let `Eq` arm"),
            };

            // Convert `IntValue`s into `BasicValueEnum` in one shot.
            build_result.unwrap().into()
        };

        // Shared dispatch with let-init. Handles primitives, structs
        // (LLVM aggregate store), arrays (memcpy from src slot).
        self.emit_store_into_slot(slot, target_ty, new_val);
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

        // Args. For each `Array(_, Some(_))`-typed arg: emit_expr returns
        // the source's place ptr; we alloca a fresh "byval" slot, memcpy
        // from src, and pass the slot ptr to build_call (the fn's LLVM
        // signature has `ptr` for this param — see lower_fn_type). Other
        // args flow as values.
        let mut arg_vals: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
        for &a in args {
            let arg_ty = self.ty_of(a);
            if self.is_sized_array(arg_ty) {
                let src_ptr = self
                    .emit_expr(fx, a)
                    .expect("array arg produced no value")
                    .into_pointer_value();
                let arr_ll = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, arg_ty);
                let arg_slot = self.alloca_in_entry(fx, arr_ll, "call.arg.slot");
                self.emit_memcpy_array(arg_slot, src_ptr, arg_ty);
                arg_vals.push(arg_slot.into());
            } else {
                let v = self.emit_expr(fx, a).expect("call arg produced no value");
                arg_vals.push(v.into());
            }
        }

        let call = self.builder.build_call(fnv, &arg_vals, "call").unwrap();

        // Return-value bridging. Path A: array returns lower as `[N x T]`
        // values; we materialize a temp slot and store so the result has
        // place form like every other array-typed expression.
        let ret_ty = self.typeck_results.fn_sig(fid).ret;
        if self.is_sized_array(ret_ty) {
            let v = call
                .try_as_basic_value()
                .left()
                .expect("array-returning call produced no value");
            let arr_ll = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, ret_ty);
            let slot = self.alloca_in_entry(fx, arr_ll, "call.ret.slot");
            self.builder.build_store(slot, v).unwrap();
            return Some(slot.into());
        }

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

    // ---------- loop / break / continue ----------

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
    fn emit_loop(
        &self,
        fx: &mut FnCodegenContext<'ctx>,
        eid: HExprId,
        init: Option<HExprId>,
        cond: Option<HExprId>,
        update: Option<HExprId>,
        body: HBlockId,
    ) -> Option<BasicValueEnum<'ctx>> {
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
        let loop_ty = self.ty_of(eid);
        let result_slot = if !is_void_ret(self.typeck_results.tys(), loop_ty) {
            let llty = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, loop_ty);
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
            let cond_v = self
                .emit_expr(fx, cond_eid)
                .expect("loop cond produced no value")
                .into_int_value();
            if !self.is_terminated() {
                self.builder
                    .build_conditional_branch(cond_v, body_bb, end_bb)
                    .unwrap();
            }
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
        if merge_bb_has_no_preds(end_bb) {
            self.builder.build_unreachable().unwrap();
            return None;
        }
        match result_slot {
            Some(slot) => {
                let llty = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, loop_ty);
                Some(self.builder.build_load(llty, slot, "loop.val").unwrap())
            }
            None => None,
        }
    }

    /// Emit `break expr?`. Stores `expr`'s value into the innermost
    /// loop's result slot (if any) before branching to its `end_bb`.
    /// Mirrors `emit_return`'s "compute operand, then exit" shape — the
    /// difference is that return calls `build_return` while break stores
    /// to a slot and branches.
    fn emit_break(&self, fx: &mut FnCodegenContext<'ctx>, expr: Option<HExprId>) {
        let target = *fx
            .loop_targets
            .last()
            .expect("HIR ensured break is inside a loop");
        if let Some(eid) = expr {
            let v = self.emit_expr(fx, eid);
            if self.is_terminated() {
                return;
            }
            if let (Some(slot), Some(val)) = (target.result_slot, v) {
                self.builder.build_store(slot, val).unwrap();
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
    fn emit_continue(&self, fx: &mut FnCodegenContext<'ctx>) {
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

        let is_array_ret = self.is_sized_array(ret_ty);
        match val.and_then(|eid| self.emit_expr(fx, eid)) {
            Some(v) if is_array_ret => {
                // Array return — Path A: operand is a place ptr; load
                // the aggregate before returning by value.
                let arr_ll = lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, ret_ty);
                let loaded = self
                    .builder
                    .build_load(arr_ll, v.into_pointer_value(), "ret.load")
                    .unwrap();
                self.builder.build_return(Some(&loaded)).unwrap();
            }
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

    fn emit_let(&self, fx: &mut FnCodegenContext<'ctx>, lid: LocalId, init: Option<HExprId>) {
        let ty = self.local_ty(lid);

        let local = &self.hir.locals[lid];

        let llty = if is_void_ret(self.typeck_results.tys(), ty) {
            // Today's codegen has no representation for void-typed locals.
            // Surface shapes that hit this: `let a = b = 1;` (a: ()),
            // `let a = loop {};` (a: !), etc. Tracked in
            // spec/BACKLOG/B005_VOID_TYPES_CODEGEN_MODEL.md.
            panic!("void type for local {}", local.name);
        } else {
            lower_ty(self.ctx, self.typeck_results.tys(), &self.adt_ll, ty)
        };
        let slot = self.alloca_in_entry(fx, llty, &format!("{}.{}.slot", local.name, lid.raw()));
        fx.locals.insert(lid, slot);
        if let Some(init_eid) = init {
            // None ⇒ divergent init (`let a = return;`); slot stays
            // uninitialized but the basic block is already terminated by
            // the diverge — no read can follow.
            if let Some(v) = self.emit_expr(fx, init_eid) {
                self.emit_store_into_slot(slot, ty, v);
            }
        }
    }
}

fn merge_bb_has_no_preds(bb: BasicBlock<'_>) -> bool {
    bb.get_first_use().is_none()
}
