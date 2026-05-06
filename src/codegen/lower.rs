//! HIR + TypeckResults → LLVM `Module`. Two-pass: declare every fn,
//! then define each body. Each fn body uses alloca + load/store for
//! locals (mem2reg-friendly canonical form).

mod call;
mod intrinsics;
mod operand;

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
    BasicValue, BasicValueEnum, FunctionValue, IntValue, PointerValue, UnnamedAddress,
};

use crate::hir::{
    ConstId, FieldIdx, FnId, HBlockId, HElseArm, HExprId, HirArrayLit, HirConst, HirConstValue,
    HirExprKind, HirProgram, LocalId, VariantIdx,
};
use crate::mono::{InstId, Instance, InstanceOperation, MonoResults};
use crate::parser::ast::{AssignOp, BinOp, UnOp};
use crate::typeck::{AdtId, ParamId, TyId, TyKind, TypeckResults, subst_from};

use super::ty::{
    AdtLlTypes, as_prim, is_signed_prim, is_void_ret, lower_adt_type, lower_fn_type, lower_prim,
    lower_ty,
};
use operand::*;

/// Lower an entire `HirProgram` to an LLVM `Module`. Consumes mono's
/// `MonoResults` to drive generic-instance declarations + body
/// emission. Verifies before returning; verifier failures panic.
///
/// Phase 1 (declare) runs two lookup tables side by side:
///   - `fn_decls: IndexVec<FnId, Option<FunctionValue>>` — the
///     FnId-keyed table consulted by `emit_call`'s non-generic /
///     extern dispatch path. Populated for extern fns and for
///     non-generic non-extern fns directly from `hir.fns` (Pass A).
///   - `inst_decls: IndexVec<InstId, Option<FunctionValue>>` — the
///     InstId-keyed table consulted by `emit_call`'s generic dispatch
///     path. Populated for every generic `Instance` mono produced
///     (Pass B); intrinsic instances push `None` to keep the InstId →
///     idx correspondence intact.
///
/// Pass A walks `hir.fns` and adds one declaration per non-generic fn
/// (extern keeps its verbatim source name; non-generic non-extern uses
/// the source name as the LLVM symbol — `mangle_inst(_, _, &[])`
/// short-circuits to the source name, so no mangling is necessary
/// here).
///
/// Pass B walks `mono.instances` (now generic-only) and adds one
/// declaration per instance (mangled name).
///
/// After Phase 1, every reachable LLVM symbol exists. Phase 2 emits
/// each instance's body — including self-recursive ones — without
/// ordering concerns.
pub fn codegen<'ctx>(
    ctx: &'ctx Context,
    hir: &HirProgram,
    typeck_results: &mut TypeckResults,
    mono: &MonoResults,
    module_name: &str,
) -> Module<'ctx> {
    let module = ctx.create_module(module_name);
    let builder = ctx.create_builder();

    // ADT struct types are materialized **lazily** keyed on
    // `(AdtId, Vec<TyId>)` — see spec/16_GENERIC.md §Codegen
    // (extension). The cache starts empty; each `lower_ty(_, Adt(aid,
    // args))` call interns the LLVM type on first encounter.
    let mut adt_ll: AdtLlTypes<'ctx> = AdtLlTypes::new();

    // Phase 1 Pass A — every non-generic fn (extern + non-extern with
    // body). Declared with their verbatim source names: extern fns
    // resolve against external object files; non-generic non-extern
    // fns share the source name as the LLVM symbol because
    // `mangle_inst(_, _, &[])` collapses to the source name. `fn_decls`
    // uses `Option` because generic fns produce no FnId-keyed entry —
    // they're keyed by instance via `inst_decls` instead (Pass B).
    //
    // Snapshot signatures into an owned `Vec` first: the emission loop
    // calls `lower_fn_type(ctx, typeck_results, ...)` which needs
    // `&mut typeck_results`, conflicting with the `&FnSig` read borrow
    // that `typeck_results.fn_sig(fid)` would hand out inline.
    let mut fn_decls: IndexVec<FnId, Option<FunctionValue<'ctx>>> =
        (0..hir.fns.len()).map(|_| None).collect();
    let non_generic_fns: Vec<(FnId, Vec<TyId>, TyId, bool)> = hir
        .fns
        .iter_enumerated()
        .filter(|(fid, h)| {
            // Non-extern fns must have a body to be declared here; bodyless
            // non-extern fns are HIR-rejected upstream.
            if !h.is_extern && h.body.is_none() {
                return false;
            }
            // Drop generic non-extern (handled by Pass B). Generic externs
            // are typeck-rejected (E0212) so they wouldn't reach codegen,
            // but the filter is symmetric for clarity.
            typeck_results.fn_sig(*fid).generic_params.is_empty()
        })
        .map(|(fid, _)| {
            let sig = typeck_results.fn_sig(fid);
            (fid, sig.params.clone(), sig.ret, sig.c_variadic)
        })
        .collect();
    for (fid, params, ret, c_variadic) in non_generic_fns {
        let hir_fn = &hir.fns[fid];
        let fn_ty = lower_fn_type(ctx, typeck_results, &mut adt_ll, &params, ret, c_variadic);
        let fnv = module.add_function(&hir_fn.name, fn_ty, None);
        // Attach LLVM param names for non-extern fns (debug-friendly).
        if !hir_fn.is_extern {
            for (i, pv) in fnv.get_param_iter().enumerate() {
                let lid = hir_fn.params[i];
                pv.set_name(&hir.locals[lid].name);
            }
        }
        fn_decls[fid] = Some(fnv);
    }

    // Phase 1 Pass B — generic instances. Each `Instance` from mono
    // produces one declaration with its mangled name and substituted
    // signature. The FunctionValue lands in `inst_decls[inst_id]` for
    // the generic dispatch path at `emit_call`. Non-generic fns are
    // **not** in mono.instances under the redesigned model.
    //
    // **Intrinsic instances are NOT declared.** When `inst.operation !=
    // Call`, codegen synthesizes the IR inline at the call site
    // (`emit_call` dispatches on `inst.operation`). To preserve the
    // `InstId → idx` correspondence in `inst_decls`, we still push an
    // entry — `None` instead of `Some(fnv)` — so downstream lookups
    // index correctly. See spec/17_LAYOUT.md §Intrinsic recognition
    // (Symbol emission).
    let mut inst_decls: IndexVec<InstId, Option<FunctionValue<'ctx>>> =
        IndexVec::with_capacity(mono.instances.len());
    for (_inst_id, inst) in mono.instances.iter_enumerated() {
        if inst.operation != InstanceOperation::Call {
            inst_decls.push(None);
            continue;
        }
        let c_variadic = hir.fns[inst.fid].is_variadic;
        let fn_ty = lower_fn_type(
            ctx,
            typeck_results,
            &mut adt_ll,
            &inst.param_tys,
            inst.ret_ty,
            c_variadic,
        );
        let fnv = module.add_function(&inst.mangled, fn_ty, None);
        // Attach LLVM param names (debug-friendly).
        for (i, pv) in fnv.get_param_iter().enumerate() {
            let lid = hir.fns[inst.fid].params[i];
            pv.set_name(&hir.locals[lid].name);
        }
        inst_decls.push(Some(fnv));
    }

    let mut cg = Codegen {
        ctx,
        module,
        builder,
        hir,
        typeck_results,
        mono,
        fn_decls,
        inst_decls,
        adt_ll,
        str_counter: Cell::new(0),
        str_lit_cache: HashMap::new(),
        llvm_trap: Cell::new(None),
    };

    // Pass 2 — define. Two iteration sources under the redesigned model:
    //   1. Non-generic non-extern fns from `hir.fns` (no Instance to
    //      look up; the body is emitted with empty subst).
    //   2. Generic instances from `mono.instances` (each with its own
    //      `inst.type_args`-derived subst).
    // Extern fns are never defined here (no body).
    let non_generic_targets: Vec<FnId> = cg
        .hir
        .fns
        .iter_enumerated()
        .filter(|(fid, h)| {
            !h.is_extern
                && h.body.is_some()
                && cg.typeck_results.fn_sig(*fid).generic_params.is_empty()
        })
        .map(|(fid, _)| fid)
        .collect();
    for fid in non_generic_targets {
        cg.lower_fn(LowerTarget::NonGeneric(fid));
    }
    for (inst_id, inst) in cg.mono.instances.iter_enumerated() {
        // Intrinsic instances have no body — codegen synthesizes the
        // IR at the call site via `emit_call`'s operation dispatch.
        if inst.operation != InstanceOperation::Call {
            continue;
        }
        cg.lower_fn(LowerTarget::Generic(inst_id));
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
    hir: &'a HirProgram,
    typeck_results: &'a mut TypeckResults,
    /// Mono's instance graph. Codegen reads `mono.instances[inst_id]` for
    /// per-instance signatures (substituted params/ret) and consults
    /// `mono.instance_map[(fid, resolved_args)]` at every generic call
    /// site to dispatch to the correct instance.
    mono: &'a MonoResults,
    /// FnId-keyed LLVM declarations. `Some` for fns that have a single
    /// well-defined FunctionValue under their FnId — namely:
    /// (1) extern fns (Pass A creates the declaration with the verbatim
    /// source name); and (2) non-generic non-extern fns (Pass B creates
    /// the FunctionValue under the mangled name in `inst_decls`, then
    /// also stores it here under the FnId). `None` for generic fns,
    /// which produce one FunctionValue per instantiation — those live
    /// only in `inst_decls`, keyed by InstId. `emit_call`'s non-generic
    /// dispatch path reads here whenever the call has no
    /// `typeck.call_type_args` entry; in that case the unwrap is sound.
    fn_decls: IndexVec<FnId, Option<FunctionValue<'ctx>>>,
    /// Per-instance LLVM declarations. Phase 1 Pass B populates from
    /// `mono.instances`; Phase 2 reads to find the FunctionValue being
    /// defined. `emit_call`'s generic-call path resolves
    /// `mono.instance_map[(fid, resolved_args)] → InstId` and then
    /// `inst_decls[inst_id]` to the FunctionValue. Intrinsic instances
    /// (`operation != Call`) push `None` here so the InstId → idx
    /// correspondence is preserved; their callsites short-circuit before
    /// hitting `inst_decls` in `emit_call`. See
    /// spec/17_LAYOUT.md §Intrinsic recognition.
    inst_decls: IndexVec<InstId, Option<FunctionValue<'ctx>>>,
    /// LLVM struct type per `AdtId`, populated up front by
    /// `prepare_adt_types`. All later `lower_ty` / `lower_fn_type` calls
    /// thread this in.
    adt_ll: AdtLlTypes<'ctx>,
    /// Suffix counter for emitted string-literal globals (`@.str.0`,
    /// `@.str.1`, …). Inkwell uses interior mutability everywhere so the
    /// rest of `Codegen` lives behind `&self`; we do the same here.
    str_counter: Cell<u32>,
    /// Content-addressed dedup for `emit_str_lit`. Maps the raw
    /// (pre-NUL) source string to its `@.str.N` global pointer. Two
    /// `"hi"` literals — including those reached via `const HELLO =
    /// "hi";` use sites — share one global. See spec/18_CONST.md
    /// "Side fix".
    str_lit_cache: HashMap<String, PointerValue<'ctx>>,
    /// Cached `declare void @llvm.trap()` so each module emits the
    /// declaration at most once. Populated lazily by
    /// `get_or_declare_trap` on the first bounds-check site.
    llvm_trap: Cell<Option<FunctionValue<'ctx>>>,
}

/// Per-fn transient state. Lives on the stack for the duration of one
/// instance's body — created in `lower_fn` and threaded as a `&mut`
/// parameter through the emit methods. Plain data; no methods of its
/// own.
/// Drives `lower_fn`'s body emission. Two cases:
///   - `NonGeneric(fid)`: emit a non-generic non-extern fn directly
///     from HIR (no Instance, empty subst).
///   - `Generic(inst_id)`: emit one body per generic instance from
///     mono (subst built from `inst.type_args`).
#[derive(Clone, Copy, Debug)]
enum LowerTarget {
    NonGeneric(FnId),
    Generic(InstId),
}

struct FnCodegenContext<'ctx> {
    /// Substituted return type of this body. For non-generic fns this
    /// is `sig.ret` (no Param leaves); for generic instances it's
    /// `inst.ret` (already substituted by mono). Read at the implicit-
    /// return path in `emit_return`.
    ret_ty: TyId,
    /// Per-body type-parameter substitution. Built once at body-entry
    /// from `sig.generic_params` zipped with `inst.type_args`. Empty
    /// for non-generic fns (in which case `substitute_ty` is identity
    /// through interning, so the same code path works uniformly).
    subst: HashMap<ParamId, TyId>,
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
    ///
    /// **Alignment**: callers don't need to call `set_alignment` on the
    /// returned slot when the slot is *only* used through the same LLVM
    /// type that was passed in here — inkwell's default `align` matches
    /// across the alloca/store/load triple in that case. The exception
    /// is `emit_transmute`, which deliberately punts the type at the
    /// load site and so must align all three instructions explicitly.
    fn alloca_in_entry(
        &mut self,
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

    /// Body-internal expr type. Substitutes `typeck.expr_tys[eid]`
    /// through the per-body `fx.subst` so generic-fn bodies see ground
    /// types at codegen. Empty subst → identity through interning, so
    /// non-generic instances take the same code path. `&mut self`
    /// because `TyArena::intern` requires mutable access.
    fn ty_of(&mut self, fx: &FnCodegenContext<'ctx>, eid: HExprId) -> TyId {
        let ty = self.typeck_results.type_of_expr(eid);
        self.resolve_ty(fx, ty)
    }

    /// Body-internal local type. Same substitution shape as `ty_of`.
    fn local_ty(&mut self, fx: &FnCodegenContext<'ctx>, lid: LocalId) -> TyId {
        let ty = self.typeck_results.type_of_local(lid);
        self.resolve_ty(fx, ty)
    }

    /// Resolve the ty with generic substitution applied
    fn resolve_ty(&mut self, fx: &FnCodegenContext<'ctx>, ty: TyId) -> TyId {
        self.typeck_results.substitute_ty(ty, &fx.subst)
    }

    /// Generic-fn-ref → mono `(InstId, &Instance)`. Takes the
    /// `Option<Vec<TyId>>` straight from the typeck-recorded
    /// `fn_ref_type_args` lookup so call sites can write
    /// `if let Some((inst_id, inst)) = codegen.resolve_instance(...)
    /// { /* generic path */ } else { /* non-generic */ }` — the
    /// `None` case threads through naturally as the non-generic
    /// branch.
    ///
    /// Resolves each typeck-recorded type-arg through the caller's
    /// `fx.subst`, asserts no `Infer` leaked through finalize, then
    /// looks up `mono.instance_map`. Returns the `InstId` alongside
    /// the `&Instance` because `inst_decls` (FunctionValue table) is
    /// `InstId`-keyed.
    fn resolve_instance(
        &mut self,
        fx: &FnCodegenContext<'ctx>,
        fid: FnId,
        typeck_args_opt: Option<Vec<TyId>>,
    ) -> Option<(InstId, &Instance)> {
        let typeck_args = typeck_args_opt?;
        let resolved_args: Vec<TyId> = typeck_args
            .iter()
            .map(|&t| self.resolve_ty(fx, t))
            .collect();

        // Defense: post-finalize + post-subst, no Infer.
        for &arg in &resolved_args {
            debug_assert!(
                !matches!(self.typeck_results.tys().kind(arg), TyKind::Infer(_)),
                "unresolved Infer leaked into mono lookup",
            );
        }

        let inst_id = *self
            .mono
            .instance_map
            .get(&(fid, resolved_args))
            .expect("mono should have instantiated every reachable generic fn-ref");
        Some((inst_id, &self.mono.instances[inst_id]))
    }

    /// `true` iff the resolved typeck kind is `Array(_, Some(_))`.
    /// Used at every "is this a place-form array?" boundary
    /// (let-init, fn-arg, fn-return, Local/Field/Index dispatch).
    fn is_sized_array(&mut self, ty: TyId) -> bool {
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

    /// `llvm.memcpy` of `sizeof(ty)` bytes from `src` to `dst`. Type-driven
    /// size — works for arrays, structs, primitives, anything sized. Align
    /// is 1 — soundness-safe and lets LLVM choose the actual alignment via
    /// the operand types.
    fn emit_memcpy(&mut self, dst: PointerValue<'ctx>, src: PointerValue<'ctx>, ty: TyId) {
        let ll = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, ty);
        let size = ll.size_of().expect("type has size_of");
        self.builder
            .build_memcpy(dst, 1, src, 1, size)
            .expect("build_memcpy");
    }

    /// Materialize an `Operand` into a fresh Place. Always allocates a
    /// new slot — the caller gets a distinct copy, even for an
    /// already-Place input. Used by:
    ///
    ///   - the byval-call ABI for array args (callee may write through
    ///     the ptr, so the caller owns its own copy);
    ///   - the array-return bridge (LLVM returned the aggregate as Value;
    ///     we materialize a Place to keep the place-form invariant);
    ///   - `emit_field`'s value-form bridge (extract array from a Value
    ///     struct and re-spill so the result has place form).
    fn spill_to_place_fresh(
        &mut self,
        fx: &FnCodegenContext<'ctx>,
        op: Operand<'ctx>,
        ty: TyId,
        name: &str,
    ) -> PointerValue<'ctx> {
        let llty = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, ty);
        let slot = self.alloca_in_entry(fx, llty, name);
        op.store_into(self, slot, ty);
        slot
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

    // ---------- per-fn entry ----------

    fn lower_fn(&mut self, target: LowerTarget) {
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
            let llty = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, pty);
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

    // ---------- blocks ----------

    fn emit_block(
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

    // ---------- expressions ----------

    /// Lower an expression. Returns `Some(Operand)` for a value-producing
    /// expression; returns `None` IFF the BB is terminated as a result of
    /// this call (the expression diverged via `return`/`break`/`continue`,
    /// or its sub-expression did). The `None` channel is reserved for
    /// divergence — `()`-typed expressions return `Some(Operand::Unit)`.
    ///
    /// **Divergence contract.** Every consumer that calls `emit_expr` MUST
    /// either propagate `None` (typically via `?`) or document why typeck
    /// guarantees the operand cannot be `!`-typed at this site. See
    /// spec/BACKLOG/B005_VOID_TYPES_CODEGEN_MODEL.md (Q3) for the
    /// motivation.
    fn emit_expr(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        eid: HExprId,
    ) -> Option<Operand<'ctx>> {
        if self.is_terminated() {
            return None;
        }
        let kind = self.hir.exprs[eid].kind.clone();
        match kind {
            HirExprKind::IntLit(n) => Some(self.emit_int_lit(fx, eid, n)),
            HirExprKind::BoolLit(b) => Some(Operand::Value(
                self.ctx.bool_type().const_int(b as u64, false).into(),
            )),
            HirExprKind::CharLit(c) => Some(Operand::Value(
                self.ctx.i8_type().const_int(c as u64, false).into(),
            )),
            HirExprKind::Null => Some(Operand::Value(
                self.ctx
                    .ptr_type(inkwell::AddressSpace::default())
                    .const_null()
                    .into(),
            )),
            HirExprKind::Local(lid) => {
                // Array-typed locals stay in place form (slot ptr, not
                // loaded aggregate). `()`-typed locals materialize as
                // Unit. Everything else loads to Value. See
                // spec/09_ARRAY.md "arrays-as-places everywhere".
                let ty = self.local_ty(fx, lid);
                let kind = self.typeck_results.tys().kind(ty);
                Some(match kind {
                    TyKind::Array(_, Some(_)) => Operand::Place(fx.locals[&lid]),
                    TyKind::Unit => Operand::Unit,
                    _ => {
                        let slot = fx.locals[&lid];
                        let llty = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, ty);
                        Operand::Value(self.builder.build_load(llty, slot, "load").unwrap())
                    }
                })
            }
            HirExprKind::Unary { op, expr } => self.emit_unary(fx, op, expr),
            HirExprKind::Binary { op, lhs, rhs } => self.emit_binary(fx, eid, op, lhs, rhs),
            HirExprKind::Assign { op, target, rhs } => {
                self.emit_assign(fx, op, target, rhs);
                // The assign expression types as `()`. If rhs diverged,
                // emit_assign early-returned and the BB is terminated;
                // emit_expr's next call will see is_terminated and return
                // None. Here we report Unit on the non-divergent path.
                if self.is_terminated() {
                    None
                } else {
                    Some(Operand::Unit)
                }
            }
            // Codegen consults `typeck.call_type_args[eid]` (sparse —
            // only for generic call sites) plus mono.instance_map to
            // resolve the callee instance for generic calls. Non-generic
            // and extern calls fall through to fn_decls[fid]. See
            // spec/16_GENERIC.md §Codegen.
            HirExprKind::Call {
                callee,
                args,
                type_args: _,
            } => self.emit_call(fx, callee, &args),
            HirExprKind::Cast { expr, ty: _ } => self.emit_cast(fx, eid, expr),
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
                if self.is_terminated() {
                    None
                } else {
                    Some(Operand::Unit)
                }
            }
            HirExprKind::Fn(fid) => {
                // spec/19_FN_PTR.md §6.iii.2: Fn-as-value lowers to a
                // pointer to the LLVM function. Two paths:
                //   - Non-generic / extern → `fn_decls[fid]` from Pass A/B.
                //   - Generic → resolve the recorded `fn_ref_type_args`
                //     through the caller's subst, look up the instance
                //     in `mono.instance_map`, point at its FunctionValue
                //     in `inst_decls`. Mirrors the call-side mono
                //     dispatch in `lower/call.rs`.
                // Intrinsic-as-value is rejected at typeck (E0281
                // IntrinsicAsValue) so we never reach the inst_decls
                // lookup for an intrinsic instance.
                let typeck_args_opt: Option<Vec<TyId>> =
                    self.typeck_results.fn_ref_type_args.get(&eid).cloned();
                let fnv = if let Some((inst_id, _inst)) =
                    self.resolve_instance(fx, fid, typeck_args_opt)
                {
                    self.inst_decls[inst_id].expect(
                        "Call-operation generic instances are declared in Pass B; \
                         intrinsic generics rejected at typeck (E0281 IntrinsicAsValue)",
                    )
                } else {
                    self.fn_decls[fid]
                        .expect("non-generic fn must be declared in fn_decls (Pass A/B)")
                };
                Some(Operand::Value(
                    fnv.as_global_value().as_pointer_value().into(),
                ))
            }
            HirExprKind::StrLit(s) => Some(self.emit_str_lit(&s)),
            HirExprKind::Index { base, index } => self.emit_index_rvalue(fx, base, index),
            HirExprKind::ArrayLit(lit) => {
                let ty = self.ty_of(fx, eid);
                self.emit_array_lit(fx, lit, ty)
            }
            HirExprKind::Field { base, name } => self.emit_field(fx, base, &name),
            HirExprKind::StructLit {
                adt,
                type_args: _,
                fields,
            } => self.emit_struct_lit(fx, eid, adt, &fields),
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
                Some(Operand::Value(ptr.into()))
            }
            HirExprKind::Unresolved(_) | HirExprKind::Poison => {
                panic!("v0 codegen: poisoned expr reached codegen")
            }
            // `has_break` and `source` are read by typeck for the value
            // type and by HIR pretty-print respectively; codegen reads
            // `self.ty_of(fx, eid)` directly to decide whether to allocate
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
            HirExprKind::Const(cid) => Some(self.emit_const(cid)),
        }
    }

    /// Emit a private constant global holding `s` followed by a `\0`
    /// terminator and return a pointer to its first byte. The value's
    /// type is opaque `ptr` (LLVM 15+); no GEP needed since the global
    /// itself is already a pointer.
    ///
    /// Cached by content (`str_lit_cache`): two `"hi"` literals — from
    /// regular code or from `const HELLO = "hi";` use sites — share a
    /// single `@.str.N` global. Without caching, two source-level
    /// `"hi"`s would produce two distinct pointers, breaking pointer-
    /// equality reasoning. See spec/18_CONST.md "Side fix".
    fn emit_str_lit(&mut self, s: &str) -> Operand<'ctx> {
        if let Some(ptr) = self.str_lit_cache.get(s) {
            return Operand::Value((*ptr).into());
        }

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

        let ptr = global.as_pointer_value();
        self.str_lit_cache.insert(s.to_string(), ptr);
        Operand::Value(ptr.into())
    }

    /// Materialize a `const` item's value at a use site. Dispatches on
    /// the `HirConstValue` variant and reuses the existing literal
    /// emitters: `Int` reads its width from `typeck.const_tys[cid]`
    /// (analogous to how `emit_int_lit` reads from `expr_tys[eid]`);
    /// `Bool`/`Char` are inlined as `const_int`; `Str` goes through
    /// the cached `emit_str_lit`. No per-`ConstId` cache needed —
    /// LLVM dedups identical `const_int` materializations, and Str
    /// dedup is already handled by `emit_str_lit`'s content cache.
    /// See spec/18_CONST.md.
    fn emit_const(&mut self, cid: ConstId) -> Operand<'ctx> {
        let hc = &self.hir.consts[cid];
        match hc.value.clone() {
            HirConstValue::Int(n) => {
                let ty = self.typeck_results.const_tys[cid];
                match self.typeck_results.tys().kind(ty) {
                    TyKind::Prim(p) => {
                        Operand::Value(lower_prim(self.ctx, *p).const_int(n, false).into())
                    }
                    other => panic!("const Int had non-prim annotation {:?}", other),
                }
            }
            HirConstValue::Bool(b) => {
                Operand::Value(self.ctx.bool_type().const_int(b as u64, false).into())
            }
            HirConstValue::Char(c) => {
                Operand::Value(self.ctx.i8_type().const_int(c as u64, false).into())
            }
            HirConstValue::Str(s) => self.emit_str_lit(&s),
        }
    }

    fn emit_int_lit(&mut self, fx: &FnCodegenContext<'ctx>, eid: HExprId, n: u64) -> Operand<'ctx> {
        let ty = self.ty_of(fx, eid);
        match self.typeck_results.tys().kind(ty) {
            TyKind::Prim(p) => Operand::Value(lower_prim(self.ctx, *p).const_int(n, false).into()),
            other => panic!("int lit had non-prim type {:?}", other),
        }
    }

    fn lvalue(&mut self, fx: &mut FnCodegenContext<'ctx>, eid: HExprId) -> PointerValue<'ctx> {
        match self.hir.exprs[eid].kind.clone() {
            HirExprKind::Local(lid) => fx.locals[&lid],
            HirExprKind::Index { base, index } => {
                // Bounds check fires here too — writing past the end is
                // as wrong as reading past it. Same auto-deref machinery
                // as the rvalue path. Lvalue positions can't diverge by
                // typeck (lvalue-positions are place-expressions, not
                // value-producers like `return`/`break`), so unwrap.
                self.emit_index_place(fx, base, index)
                    .expect("lvalue-position Index produced no place")
                    .0
            }
            HirExprKind::Field { base, name } => {
                // Auto-deref through any number of outer Ptr layers so
                // `q.x` for `q: *mut P` (or `*mut *mut P`, …) reaches the
                // underlying Adt. Mirrors `emit_index_place`'s peel-loop;
                // typeck's `auto_deref_ptr` already accepted the syntax,
                // codegen just lowers it.
                let lv = self.lvalue(fx, base);
                let bt = self.ty_of(fx, base);
                let (base_ptr, base_ty) = self.peel_ptrs(lv, bt);
                let aid = match self.typeck_results.tys().kind(base_ty) {
                    // `_args` are intentionally dropped here: `field_gep`
                    // calls `lower_ty(base_ty)` which re-derives the LLVM
                    // struct type via `lower_adt_type(aid, args)` from
                    // `base_ty` itself, so the args plumb through without
                    // the lvalue path having to substitute manually.
                    // Asymmetric with the rvalue Field arm below, which
                    // does substitute (it needs the field's *typeck*
                    // type, not just its LLVM offset).
                    TyKind::Adt(aid, _args) => *aid,
                    other => panic!("Field base lvalue: non-Adt type after peel {:?}", other),
                };
                let fidx = self.field_index(aid, &name);
                self.field_gep(base_ptr, base_ty, fidx)
            }
            HirExprKind::Unary {
                op: UnOp::Deref,
                expr,
            } => self
                .emit_deref_ptr(fx, expr)
                .expect("lvalue-position Deref operand cannot diverge"),
            other => panic!("v0 codegen: non-lvalue assignment target {:?}", other),
        }
    }

    /// Position of `name` in `adts[aid]`'s sole variant. Typeck has
    /// already validated the field exists; a miss here is an ICE.
    fn field_index(&mut self, aid: AdtId, name: &str) -> u32 {
        let adt = self.typeck_results.adt_def(aid);
        adt.variants[VariantIdx::from_raw(0)]
            .fields
            .iter()
            .position(|f| f.name == name)
            .expect("typeck guaranteed field exists") as u32
    }

    /// Walk through outer `Ptr` layers on `(cur_ptr, cur_ty)`, loading
    /// the next pointer at each step until `cur_ty` is no longer a
    /// `Ptr`. Mirrors `emit_index_place`'s peel-loop. Used by
    /// `lvalue(Field)` and `emit_field`'s Place path so `q.x` for
    /// `q: *mut P` reaches the Adt without the user writing `(*q).x`.
    fn peel_ptrs(
        &mut self,
        mut cur_ptr: PointerValue<'ctx>,
        mut cur_ty: TyId,
    ) -> (PointerValue<'ctx>, TyId) {
        let tcx = self.typeck_results.tys();
        let ptr_ll = self.ctx.ptr_type(inkwell::AddressSpace::default());
        while let TyKind::Ptr(inner, _) = tcx.kind(cur_ty) {
            let next = *inner;
            cur_ptr = self
                .builder
                .build_load(ptr_ll, cur_ptr, "deref")
                .unwrap()
                .into_pointer_value();
            cur_ty = next;
        }
        (cur_ptr, cur_ty)
    }

    /// Type-only counterpart to `peel_ptrs` — peels outer `Ptr` layers
    /// off `ty` without emitting IR. Used at the top of `emit_field` to
    /// find the `Adt` for `aid` lookup before deciding which lowering
    /// path to take.
    fn peel_ptrs_ty(&mut self, mut cur_ty: TyId) -> TyId {
        let tcx = self.typeck_results.tys();
        while let TyKind::Ptr(inner, _) = tcx.kind(cur_ty) {
            cur_ty = *inner;
        }
        cur_ty
    }

    /// `getelementptr` of `base_ptr` to the `field_idx`'th field of an
    /// ADT-typed place. Shared by `lvalue`'s Field arm (assignment
    /// targets, `&place.field`) and `emit_field`'s Place path
    /// (single-field rvalue load).
    fn field_gep(
        &mut self,
        base_ptr: PointerValue<'ctx>,
        base_ty: TyId,
        field_idx: u32,
    ) -> PointerValue<'ctx> {
        let base_ll = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, base_ty);
        self.builder
            .build_struct_gep(base_ll, base_ptr, field_idx, "fld.gep")
            .unwrap()
    }

    fn emit_field(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        base: HExprId,
        name: &str,
    ) -> Option<Operand<'ctx>> {
        let base_expr = &self.hir.exprs[base];
        // Peel outer Ptr layers off the base type so `q.x` for `q: *mut P`
        // can locate the Adt. The Value path below never sees a Ptr-typed
        // aggregate (Ptr-typed exprs are place-form via Local/Field/Deref),
        // so peeling unconditionally is safe — `peel_ptrs_ty` is a no-op
        // on non-Ptr types.
        let bt = self.ty_of(fx, base);
        let base_ty = self.peel_ptrs_ty(bt);
        let (aid, base_args): (AdtId, Vec<TyId>) = match self.typeck_results.tys().kind(base_ty) {
            TyKind::Adt(aid, args) => (*aid, args.clone()),
            other => panic!("Field rvalue: non-Adt base type after peel {:?}", other),
        };
        let field_idx = self.field_index(aid, name);
        // Look up the field's *declared* type (which may contain
        // `Param(_)` for a generic ADT) and substitute via the
        // `(adt.generic_params, base_args)` map. For non-generic ADTs
        // `base_args` is empty and `substitute_ty` is identity.
        // See spec/16_GENERIC.md §Codegen (extension).
        let (field_decl_ty, subst) = {
            let adt = self.typeck_results.adt_def(aid);
            let decl_ty =
                adt.variants[VariantIdx::from_raw(0)].fields[FieldIdx::from_raw(field_idx)].ty;
            let subst = subst_from(&adt.generic_params, &base_args);
            (decl_ty, subst)
        };
        // Two-step substitution: first map the ADT's `Param(_)`
        // through `(adt.generic_params, base_args)`, then feed the
        // result through the caller's `fx.subst` (which resolves any
        // `Param` left over from the enclosing fn's generic context).
        let field_ty = self.typeck_results.substitute_ty(field_decl_ty, &subst);
        let field_ty = self.resolve_ty(fx, field_ty);
        let field_ll = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, field_ty);

        if base_expr.is_place {
            // Place path — single-field load via `getelementptr`, no whole-struct copy.
            // Peel base_ptr in lockstep with base_ty (loading at each Ptr layer).
            let lv = self.lvalue(fx, base);
            let bt = self.ty_of(fx, base);
            let (base_ptr, _) = self.peel_ptrs(lv, bt);
            let gep = self.field_gep(base_ptr, base_ty, field_idx);
            // Array-typed fields stay in place form: hand back the GEP'd
            // pointer instead of loading the aggregate. Mirrors the
            // arrays-as-places invariant for Locals.
            if self.is_sized_array(field_ty) {
                Some(Operand::Place(gep))
            } else {
                Some(Operand::Value(
                    self.builder.build_load(field_ll, gep, "fld.load").unwrap(),
                ))
            }
        } else {
            // Value path — base is an rvalue aggregate; pull the field
            // out via extractvalue, no memory traffic.
            let agg_op = self.emit_expr(fx, base)?;
            let agg = agg_op.load_value(self, base_ty, "load").into_struct_value();
            if self.is_sized_array(field_ty) {
                // Bridge: extract the array value, then spill into a fresh
                // slot so the result has place form. Rare path — only fires
                // when the struct itself is in SSA value form (e.g., direct
                // Field on a Call return), which v0 codegen doesn't construct
                // for ADTs containing arrays. Future work: revisit if it trips.
                let arr_val = self
                    .builder
                    .build_extract_value(agg, field_idx, "fld.arr")
                    .unwrap();
                let slot = self.spill_to_place_fresh(
                    fx,
                    Operand::Value(arr_val),
                    field_ty,
                    "fld.arr.slot",
                );
                Some(Operand::Place(slot))
            } else {
                let val = self
                    .builder
                    .build_extract_value(agg, field_idx, "fld")
                    .unwrap();
                Some(Operand::Value(val))
            }
        }
    }

    /// Build a struct value as an SSA aggregate via `insertvalue`. The
    /// HIR-side field list isn't necessarily in declaration order; we
    /// walk the declared fields and find each provided value by name.
    /// Typeck has already validated the field set, so missing/extra/
    /// duplicate are unreachable at this point.
    fn emit_struct_lit(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        lit_eid: HExprId,
        adt: crate::hir::HAdtId,
        fields: &[crate::hir::HirStructLitField],
    ) -> Option<Operand<'ctx>> {
        let aid = AdtId::from_raw(adt.raw());
        // Read the lit's resolved Adt type to extract the type-args.
        // Post-finalize+mono these are concrete (no `Param`/`Infer`).
        // For non-generic ADTs `args` is empty.
        let lit_ty = self.ty_of(fx, lit_eid);
        let args: Vec<TyId> = match self.typeck_results.tys().kind(lit_ty) {
            TyKind::Adt(_, args) => args.clone(),
            other => panic!("emit_struct_lit: lit type is not Adt: {:?}", other),
        };
        let llty = lower_adt_type(self.ctx, self.typeck_results, &mut self.adt_ll, aid, &args);
        let mut agg = llty.get_undef();

        // Snapshot the declared field names by value so the loop body
        // can take `&mut self` for ty_of/emit_expr/load_value without
        // fighting an outstanding `&adt_def` borrow.
        let declared_names: Vec<String> = self.typeck_results.adt_def(aid).variants
            [VariantIdx::from_raw(0)]
        .fields
        .iter()
        .map(|f| f.name.clone())
        .collect();
        for (i, declared_name) in declared_names.iter().enumerate() {
            let provided = fields
                .iter()
                .find(|p| &p.name == declared_name)
                .expect("typeck guaranteed all fields are provided");
            let provided_ty = self.ty_of(fx, provided.value);
            let provided_op = self.emit_expr(fx, provided.value)?;
            let value = provided_op.load_value(self, provided_ty, "load");
            let new_agg = self
                .builder
                .build_insert_value(agg, value, i as u32, "lit.fld")
                .unwrap();
            agg = new_agg.into_struct_value();
        }
        Some(Operand::Value(agg.as_basic_value_enum()))
    }

    // ---------- array literals & indexing ----------

    /// Lower an array literal to a fresh alloca-backed place. Returns
    /// `Operand::Place(slot)`; downstream consumers (let-init, fn-arg,
    /// Index, …) see this as the literal's place form. Per
    /// spec/09_ARRAY.md "ArrayLit shape" (Q1 in the codegen plan):
    /// alloca + GEP+store, no SSA aggregate.
    fn emit_array_lit(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        lit: HirArrayLit,
        array_ty: TyId,
    ) -> Option<Operand<'ctx>> {
        let arr_ll = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, array_ty);
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
    fn emit_index_rvalue(
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
        let elem_ll = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, elem_ty);
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
    fn emit_index_place(
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
                let arr_ll = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, cur_ty);
                let elt_ptr = unsafe {
                    self.builder
                        .build_in_bounds_gep(arr_ll, cur_ptr, &[zero, idx_v], "idx.gep")
                        .unwrap()
                };
                Some((elt_ptr, elem))
            }
            TyKind::Array(elem, None) => {
                let elem_ll = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, elem);
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

    // ---------- unary / binary ----------

    fn emit_unary(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        op: UnOp,
        expr: HExprId,
    ) -> Option<Operand<'ctx>> {
        if let UnOp::Deref = op {
            let ptr = self.emit_deref_ptr(fx, expr)?;
            return Some(Operand::Place(ptr));
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

    /// Load the operand of a `Deref` and return the resulting raw pointer.
    /// Shared by `emit_unary`'s Deref rvalue arm (wraps in `Operand::Place`)
    /// and `lvalue`'s Deref arm (returns the ptr directly).
    fn emit_deref_ptr(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        expr: HExprId,
    ) -> Option<PointerValue<'ctx>> {
        let inner_ty = self.ty_of(fx, expr);
        let inner_op = self.emit_expr(fx, expr)?;
        Some(
            inner_op
                .load_value(self, inner_ty, "deref")
                .into_pointer_value(),
        )
    }

    fn emit_binary(
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

    fn emit_assign(
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
        let llty = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, target_ty);
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

    // ---------- calls ----------

    /// Two-step call lowering. Step 1 — `CallLike::resolve` classifies
    /// the callee (intrinsic recipe → `Inlined(Operand)`, real call →
    /// `Call` with concrete dispatch info). Step 2 — `CallLike::emit`
    /// passes through inlined operands, materializes args + issues
    /// `build_call` / `build_indirect_call` + wraps the return for
    /// real calls. All IR-builder work for calls lives in
    /// `lower/call.rs`. See spec/19_FN_PTR.md §6 + the follow-up plan.
    fn emit_call(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        callee_eid: HExprId,
        args: &[HExprId],
    ) -> Option<Operand<'ctx>> {
        let call_like = call::CallLike::resolve(self, fx, callee_eid, args);
        call_like.emit(self, fx, args)
    }

    // ---------- casts ----------

    /// `expr as Ty` codegen. Per spec/12_AS.md §"Codegen": dispatch
    /// on `(src_kind, dst_kind)` per the allowed-set table. Typeck's
    /// `infer_cast` (E0274 `InvalidCast`) has already rejected
    /// off-table pairs, so the catch-all arm is an invariant assertion.
    fn emit_cast(
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

    // ---------- if / else ----------

    fn emit_if(
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
            let llty = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, if_ty);
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
                let llty = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, if_ty);
                Some(Operand::Value(
                    self.builder.build_load(llty, slot, "if.val").unwrap(),
                ))
            }
            None => Some(Operand::Unit),
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
            let llty = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, loop_ty);
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
                let llty = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, loop_ty);
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
    fn emit_break(&mut self, fx: &mut FnCodegenContext<'ctx>, expr: Option<HExprId>) {
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
    fn emit_continue(&mut self, fx: &mut FnCodegenContext<'ctx>) {
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

    fn emit_return(&mut self, fx: &mut FnCodegenContext<'ctx>, val: Option<HExprId>) {
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

    // ---------- let ----------

    fn emit_let(&mut self, fx: &mut FnCodegenContext<'ctx>, lid: LocalId, init: Option<HExprId>) {
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
        let llty = lower_ty(self.ctx, self.typeck_results, &mut self.adt_ll, ty);
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
