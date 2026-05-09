//! HIR + TypeckResults → LLVM `Module`. Two-pass: declare every fn,
//! then define each body. Each fn body uses alloca + load/store for
//! locals (mem2reg-friendly canonical form).
//!
//! The actual emission is split across sibling submodules:
//!   - `declare`  — Phase 1 (Pass A non-generic + Pass B generic).
//!   - `fn_body`  — `lower_fn`, `emit_block`, `emit_return`, `emit_let`.
//!   - `expr`     — `emit_expr` dispatcher + literal/const emitters.
//!   - `place`    — `lvalue`, `emit_field`, `emit_struct_lit`, ptr-peel.
//!   - `array`    — array literal, indexing, bounds-check / trap, repeat.
//!   - `op`       — unary, binary, short-circuit, compound assign, cast.
//!   - `control`  — if / loop / break / continue.
//!   - `call`     — call-site lowering (direct + indirect).
//!   - `intrinsics` — built-in intrinsic recipes (size_of / transmute).
//!   - `operand`  — the place-vs-value `Operand` enum.
//!
//! `lower.rs` itself only holds the `Codegen` struct, the per-fn
//! `FnCodegenContext`, the public `codegen()` entry, and the universal
//! helpers (`is_terminated`, `alloca_in_entry`, type-resolution,
//! instance lookup, `emit_memcpy`, `spill_to_place_fresh`).

mod array;
mod call;
mod control;
mod declare;
mod expr;
mod fn_body;
mod intrinsics;
mod op;
mod operand;
mod place;

use std::cell::Cell;
use std::collections::HashMap;

use index_vec::IndexVec;
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{FunctionValue, PointerValue};

use crate::hir::{FnId, HExprId, HirProgram, LocalId};
use crate::mono::{InstId, Instance, InstanceOperation, MonoResults};
use crate::typeck::{ParamId, TyId, TyKind, TypeckResults};

use super::ty::AdtLlTypes;
use operand::*;

/// Lower an entire `HirProgram` to an LLVM `Module`. Consumes mono's
/// `MonoResults` to drive generic-instance declarations + body
/// emission. Verifies before returning; verifier failures panic.
pub fn codegen<'ctx>(
    ctx: &'ctx Context,
    hir: &HirProgram,
    typeck_results: &mut TypeckResults,
    mono: &MonoResults,
    module_name: &str,
) -> Module<'ctx> {
    let module = ctx.create_module(module_name);
    let builder = ctx.create_builder();

    let mut cg = Codegen {
        ctx,
        module,
        builder,
        hir,
        typeck_results,
        mono,
        // Phase 1 fills these in via `cg.declare_all()`.
        // `fn_decls` is sized up front so non-generic FnIds index in
        // place; `inst_decls` is push-built so InstId → idx stays 1:1.
        // `adt_ll` is the lazy `(AdtId, args)` LLVM-struct cache that
        // every `lower_ty(Adt(_, _))` interns into on first encounter
        // (see spec/16_GENERIC.md §Codegen).
        fn_decls: (0..hir.fns.len()).map(|_| None).collect(),
        inst_decls: IndexVec::with_capacity(mono.instances.len()),
        adt_ll: AdtLlTypes::new(),
        str_counter: Cell::new(0),
        str_lit_cache: HashMap::new(),
        llvm_trap: Cell::new(None),
    };
    cg.declare_all();

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

pub(super) struct Codegen<'a, 'ctx> {
    pub(super) ctx: &'ctx Context,
    pub(super) module: Module<'ctx>,
    pub(super) builder: Builder<'ctx>,
    pub(super) hir: &'a HirProgram,
    pub(super) typeck_results: &'a mut TypeckResults,
    /// Mono's instance graph. Codegen reads `mono.instances[inst_id]` for
    /// per-instance signatures (substituted params/ret) and consults
    /// `mono.instance_map[(fid, resolved_args)]` at every generic call
    /// site to dispatch to the correct instance.
    pub(super) mono: &'a MonoResults,
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
    pub(super) fn_decls: IndexVec<FnId, Option<FunctionValue<'ctx>>>,
    /// Per-instance LLVM declarations. Phase 1 Pass B populates from
    /// `mono.instances`; Phase 2 reads to find the FunctionValue being
    /// defined. `emit_call`'s generic-call path resolves
    /// `mono.instance_map[(fid, resolved_args)] → InstId` and then
    /// `inst_decls[inst_id]` to the FunctionValue. Intrinsic instances
    /// (`operation != Call`) push `None` here so the InstId → idx
    /// correspondence is preserved; their callsites short-circuit before
    /// hitting `inst_decls` in `emit_call`. See
    /// spec/17_LAYOUT.md §Intrinsic recognition.
    pub(super) inst_decls: IndexVec<InstId, Option<FunctionValue<'ctx>>>,
    /// LLVM struct type per `AdtId`, populated up front by
    /// `prepare_adt_types`. All later `lower_ty` / `lower_fn_type` calls
    /// thread this in.
    pub(super) adt_ll: AdtLlTypes<'ctx>,
    /// Suffix counter for emitted string-literal globals (`@.str.0`,
    /// `@.str.1`, …). Inkwell uses interior mutability everywhere so the
    /// rest of `Codegen` lives behind `&self`; we do the same here.
    pub(super) str_counter: Cell<u32>,
    /// Content-addressed dedup for `emit_str_lit`. Maps the raw
    /// (pre-NUL) source string to its `@.str.N` global pointer. Two
    /// `"hi"` literals — including those reached via `const HELLO =
    /// "hi";` use sites — share one global. See spec/18_CONST.md
    /// "Side fix".
    pub(super) str_lit_cache: HashMap<String, PointerValue<'ctx>>,
    /// Cached `declare void @llvm.trap()` so each module emits the
    /// declaration at most once. Populated lazily by
    /// `get_or_declare_trap` on the first bounds-check site.
    pub(super) llvm_trap: Cell<Option<FunctionValue<'ctx>>>,
}

/// Drives `lower_fn`'s body emission. Two cases:
///   - `NonGeneric(fid)`: emit a non-generic non-extern fn directly
///     from HIR (no Instance, empty subst).
///   - `Generic(inst_id)`: emit one body per generic instance from
///     mono (subst built from `inst.type_args`).
#[derive(Clone, Copy, Debug)]
pub(super) enum LowerTarget {
    NonGeneric(FnId),
    Generic(InstId),
}

/// Per-fn transient state. Lives on the stack for the duration of one
/// instance's body — created in `lower_fn` and threaded as a `&mut`
/// parameter through the emit methods. Plain data; no methods of its
/// own.
pub(super) struct FnCodegenContext<'ctx> {
    /// Substituted return type of this body. For non-generic fns this
    /// is `sig.ret` (no Param leaves); for generic instances it's
    /// `inst.ret` (already substituted by mono). Read at the implicit-
    /// return path in `emit_return`.
    pub(super) ret_ty: TyId,
    /// Per-body type-parameter substitution. Built once at body-entry
    /// from `sig.generic_params` zipped with `inst.type_args`. Empty
    /// for non-generic fns (in which case `substitute_ty` is identity
    /// through interning, so the same code path works uniformly).
    pub(super) subst: HashMap<ParamId, TyId>,
    pub(super) fn_value: FunctionValue<'ctx>,
    /// Dedicated alloca block for this fn. `allocas:` is the entry block
    /// (first appended) and is terminated by `br label %body`. All allocas
    /// land before that terminator; mem2reg sees them as entry-block
    /// allocas and promotes them. The extra `br` is removed by
    /// `simplifycfg` in optimized builds.
    pub(super) allocas_bb: BasicBlock<'ctx>,
    pub(super) locals: HashMap<LocalId, PointerValue<'ctx>>,
    /// One frame per `Loop` whose body is currently being emitted.
    /// Pushed before emitting the body, popped after. `Break` / `Continue`
    /// read `last()` for their target. HIR-lower already filed
    /// E0263/E0264 if break/continue is outside a loop, so an empty
    /// stack here is an ICE-worthy invariant violation.
    /// See spec/13_LOOPS.md "FnCodegenContext gains a single LoopTargets
    /// shape".
    pub(super) loop_targets: Vec<LoopTargets<'ctx>>,
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
pub(super) struct LoopTargets<'ctx> {
    pub(super) end_bb: BasicBlock<'ctx>,
    pub(super) continue_target_bb: BasicBlock<'ctx>,
    pub(super) result_slot: Option<PointerValue<'ctx>>,
}

impl<'a, 'ctx> Codegen<'a, 'ctx> {
    // ---------- universal helpers ----------

    /// Whether the builder's current basic block already has a terminator.
    /// Used to short-circuit emission after `return`/`br`.
    pub(super) fn is_terminated(&self) -> bool {
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
    pub(super) fn alloca_in_entry(
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
    pub(super) fn ty_of(&mut self, fx: &FnCodegenContext<'ctx>, eid: HExprId) -> TyId {
        let ty = self.typeck_results.type_of_expr(eid);
        self.resolve_ty(fx, ty)
    }

    /// Body-internal local type. Same substitution shape as `ty_of`.
    pub(super) fn local_ty(&mut self, fx: &FnCodegenContext<'ctx>, lid: LocalId) -> TyId {
        let ty = self.typeck_results.type_of_local(lid);
        self.resolve_ty(fx, ty)
    }

    /// Resolve the ty with generic substitution applied
    pub(super) fn resolve_ty(&mut self, fx: &FnCodegenContext<'ctx>, ty: TyId) -> TyId {
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
    pub(super) fn resolve_instance(
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
    pub(super) fn is_sized_array(&mut self, ty: TyId) -> bool {
        matches!(
            self.typeck_results.tys().kind(ty),
            TyKind::Array(_, Some(_))
        )
    }

    /// `llvm.memcpy` of `sizeof(ty)` bytes from `src` to `dst`. Type-driven
    /// size — works for arrays, structs, primitives, anything sized. Align
    /// is 1 — soundness-safe and lets LLVM choose the actual alignment via
    /// the operand types.
    pub(super) fn emit_memcpy(
        &mut self,
        dst: PointerValue<'ctx>,
        src: PointerValue<'ctx>,
        ty: TyId,
    ) {
        let ll = self.lower_ty(ty);
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
    pub(super) fn spill_to_place_fresh(
        &mut self,
        fx: &FnCodegenContext<'ctx>,
        op: Operand<'ctx>,
        ty: TyId,
        name: &str,
    ) -> PointerValue<'ctx> {
        let llty = self.lower_ty(ty);
        let slot = self.alloca_in_entry(fx, llty, name);
        op.store_into(self, slot, ty);
        slot
    }
}
