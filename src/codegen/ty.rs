//! `TyId` → LLVM type lowering. Signedness lives on the *operations*,
//! not the LLVM type — `i32` and `u32` both lower to LLVM `i32`.
//!
//! Generic ADTs are materialized **lazily** at codegen, keyed on
//! `(AdtId, Vec<TyId>)`. Each distinct instantiation gets its own LLVM
//! struct type — `LinkedList<i32>` and `LinkedList<u8>` produce two
//! different layouts. Hash-cons at the TyId level dedupes the cache:
//! structurally-identical instances reach the same key. See
//! spec/16_GENERIC.md §Codegen (extension).
//!
//! The three "needs-the-world" lowerers (`lower_ty`, `lower_adt_type`,
//! `lower_fn_type`) are methods on `Codegen` — they all read
//! `typeck_results` and intern into `adt_ll`, so threading those as
//! separate arguments was pure noise. Pure helpers that don't touch the
//! Codegen state stay as free functions (`lower_prim`, `is_void_ret`,
//! `is_signed_prim`, `prim_bit_width`, `is_ptr_width_int`, `as_prim`).

use std::collections::HashMap;

use inkwell::context::Context;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, StructType};

use crate::hir::VariantIdx;
use crate::typeck::{AdtId, PrimTy, TyArena, TyId, TyKind, TypeckResults, subst_from};

use super::lower::Codegen;

/// Per-`(AdtId, Vec<TyId>)` cache of LLVM struct types. The args list
/// is empty for non-generic ADTs and saturates the ADT's
/// `generic_params` for generic instantiations. See
/// spec/16_GENERIC.md §Codegen (extension).
pub type AdtLlTypes<'ctx> = HashMap<(AdtId, Vec<TyId>), StructType<'ctx>>;

impl<'a, 'ctx> Codegen<'a, 'ctx> {
    /// Lower a `TyId` to a `BasicTypeEnum`. `Unit` lowers to LLVM `{}`
    /// (zero-sized empty struct, sole inhabitant `{} undef`). Panics on
    /// `Never` (no value form ever — `!`-typed expressions terminate the
    /// BB before any consumer reaches `lower_ty`), `Fn` (use
    /// `lower_fn_type`), or post-typeck poison (`Infer`/`Error`).
    pub(in crate::codegen) fn lower_ty(&mut self, ty: TyId) -> BasicTypeEnum<'ctx> {
        let kind = self.typeck_results.tys.kind(ty).clone();
        match kind {
            TyKind::Prim(p) => lower_prim(self.ctx, p).into(),
            TyKind::Ptr(..) => self.ctx.ptr_type(inkwell::AddressSpace::default()).into(),
            TyKind::Adt(aid, args) => self.lower_adt_type(aid, &args).as_basic_type_enum(),
            TyKind::Unit => self.ctx.struct_type(&[], false).into(),
            TyKind::Never => panic!(
                "lower_ty called on Never — !-typed expressions terminate \
                 the BB before any consumer asks for a slot"
            ),
            // spec/19_FN_PTR.md §6: `Fn` always lowers to opaque `ptr` at
            // value position. The `FunctionType` shape (used at call sites
            // and at fn declarations) is built separately via
            // `lower_fn_type`. LLVM `ptr` is type-agnostic, so this single
            // arm covers all `[extern "C"]?` / variadic combinations —
            // those are typeck-level invariants only.
            TyKind::Fn { .. } => self
                .ctx
                .ptr_type(inkwell::AddressSpace::default())
                .into(),
            TyKind::Array(elem, Some(n)) => {
                let elem_ll = self.lower_ty(elem);
                elem_ll.array_type(n as u32).into()
            }
            TyKind::Array(_, None) => unreachable!(
                "Array(_, None) is not a value type; typeck E0269 should have rejected"
            ),
            TyKind::Infer(_) | TyKind::Error => {
                panic!(
                    "post-typeck type is unresolved: {}",
                    self.typeck_results.tys.render(ty)
                )
            }
            // Phase D (mono) substitutes Param leaves into concrete types
            // before codegen runs. If a Param survives into codegen, it's a
            // mono bug (or the driver ran codegen on an errored mono).
            TyKind::Param(_) => {
                panic!(
                    "lower_ty called on Param — mono should have substituted: {}",
                    self.typeck_results.tys.render(ty)
                )
            }
        }
    }

    /// Lazy materialization of an LLVM struct for a specific
    /// `(AdtId, args)` pair. Two-phase per-call (mirrors typeck's
    /// phase 0 / 0.5):
    ///   - Insert opaque struct into the cache **before** recursing, so
    ///     self-referential types via pointer (`LinkedList<T>.next: *mut
    ///     LinkedList<T>`) hit the cache on recursion. `Ptr` lowers to
    ///     opaque LLVM `ptr` without re-entering `lower_adt_type`, so the
    ///     recursion converges anyway, but the cache-first insert is
    ///     belt-and-suspenders.
    ///   - Substitute the ADT's declared field types via
    ///     `(generic_params, args)` and lower each substituted type. The
    ///     substituted types are concrete (or fn-Param when this is being
    ///     called from inside a generic-fn body's lowering, in which case
    ///     mono's body subst handles it).
    ///
    /// The display name carries the args via the same Display style as
    /// `TyArena::render` (`Adt(<raw>, [<args>])`) so LLVM IR dumps line up
    /// with diagnostic output.
    pub(in crate::codegen) fn lower_adt_type(
        &mut self,
        aid: AdtId,
        args: &[TyId],
    ) -> StructType<'ctx> {
        let key = (aid, args.to_vec());
        if let Some(&st) = self.adt_ll.get(&key) {
            return st;
        }
        let display_name = render_adt_instance_name(self.typeck_results, aid, args);
        let opaque = self.ctx.opaque_struct_type(&display_name);
        self.adt_ll.insert(key, opaque);

        // Build subst + snapshot field decl types in a tight `&typeck.adts`
        // borrow scope. The borrow ends at block-exit so the loop below is
        // free to call `&mut typeck` for substitution and recursion.
        // For non-generic ADTs the subst is empty and `substitute_ty` is
        // identity (hash-cons returns the same TyId).
        let (subst, field_decl_tys) = {
            let adt = &self.typeck_results.adts[aid];
            (
                subst_from(&adt.generic_params, args),
                adt.variants[VariantIdx::from_raw(0)]
                    .fields
                    .iter()
                    .map(|f| f.ty)
                    .collect::<Vec<TyId>>(),
            )
        };

        let fields_ll: Vec<BasicTypeEnum<'ctx>> = field_decl_tys
            .into_iter()
            .map(|f_ty| {
                let concrete = self.typeck_results.tys.substitute_ty(f_ty, &subst);
                self.lower_ty(concrete)
            })
            .collect();
        opaque.set_body(&fields_ll, false);
        opaque
    }

    /// Build a `FunctionType` from raw param types, return type, and
    /// c_variadic flag. Unit/Never returns become LLVM `void`.
    ///
    /// Takes `(params, ret, c_variadic)` as plain arguments rather than a
    /// `&FnSig` so callers can pass `(&inst.params, inst.ret,
    /// hir.fns[fid].is_variadic)` from a mono `Instance` without
    /// synthesizing a pseudo `FnSig` (which would force a `Vec<TyId>` clone
    /// of the params). Existing callers pass `(&sig.params, sig.ret,
    /// sig.c_variadic)`.
    ///
    /// Array params lower to LLVM `ptr` (manual byval ABI per
    /// spec/09_ARRAY.md): the caller copies into a fresh slot and passes
    /// the pointer; the callee uses the incoming ptr directly as the
    /// param's storage. Array *returns* lower normally to `[N x T]` and
    /// rely on LLVM's calling-convention machinery to pick sret /
    /// register-return per target — Path A in the codegen plan.
    pub(in crate::codegen) fn lower_fn_type(
        &mut self,
        params: &[TyId],
        ret: TyId,
        c_variadic: bool,
    ) -> FunctionType<'ctx> {
        let lowered_params: Vec<BasicMetadataTypeEnum<'ctx>> = params
            .iter()
            .map(|&p| {
                if let TyKind::Array(_, Some(_)) = self.typeck_results.tys.kind(p) {
                    self.ctx
                        .ptr_type(inkwell::AddressSpace::default())
                        .into()
                } else {
                    self.lower_ty(p).into()
                }
            })
            .collect();
        if is_void_ret(&self.typeck_results.tys, ret) {
            self.ctx.void_type().fn_type(&lowered_params, c_variadic)
        } else {
            self.lower_ty(ret).fn_type(&lowered_params, c_variadic)
        }
    }
}

fn render_adt_instance_name(typeck: &TypeckResults, aid: AdtId, args: &[TyId]) -> String {
    let name = &typeck.adts[aid].name;
    if args.is_empty() {
        // Preserve source name for non-generic ADTs — keeps existing
        // LLVM IR snapshots and `nm` output user-recognizable.
        name.clone()
    } else {
        // Generic instances: source name plus the rendered args. The
        // mangler at `mono::mangle` is what LLVM consumes; this string
        // is purely the LLVM struct *type name* (debug-friendly), so
        // collisions across instantiations are fine — LLVM struct
        // type names are not symbol names.
        let rendered: Vec<String> = args.iter().map(|&a| typeck.tys.render(a)).collect();
        format!("{}<{}>", name, rendered.join(", "))
    }
}

/// Lower a primitive to its LLVM int type. Width-only — the LLVM
/// type doesn't know about signedness.
pub fn lower_prim<'ctx>(ctx: &'ctx Context, p: PrimTy) -> inkwell::types::IntType<'ctx> {
    match p {
        PrimTy::I8 | PrimTy::U8 => ctx.i8_type(),
        PrimTy::I16 | PrimTy::U16 => ctx.i16_type(),
        PrimTy::I32 | PrimTy::U32 => ctx.i32_type(),
        // `usize` / `isize` are target-fixed at 64-bit in v0. The day
        // we add 32-bit-target awareness, this single arm flips per the
        // target's pointer width (`DataLayout::get_pointer_size`).
        PrimTy::I64 | PrimTy::U64 | PrimTy::Usize | PrimTy::Isize => ctx.i64_type(),
        PrimTy::Bool => ctx.bool_type(),
    }
}

/// `()` and `!` both surface as `void` returns / no value at the IR level.
pub fn is_void_ret(tcx: &TyArena, ty: TyId) -> bool {
    matches!(tcx.kind(ty), TyKind::Unit | TyKind::Never)
}

/// Whether a primitive participates in *signed* integer ops (sdiv, ashr,
/// icmp slt, etc.).
pub fn is_signed_prim(p: PrimTy) -> bool {
    matches!(
        p,
        PrimTy::I8 | PrimTy::I16 | PrimTy::I32 | PrimTy::I64 | PrimTy::Isize
    )
}

/// LLVM bit-width of a primitive. Mirrors `lower_prim` row by row so the
/// two stay in sync. Used by `emit_transmute` for the (Prim, Prim)
/// same-width arm and by anything else that needs to compare widths
/// without materializing an `IntType<'ctx>`.
pub fn prim_bit_width(p: PrimTy) -> u32 {
    match p {
        PrimTy::I8 | PrimTy::U8 | PrimTy::Bool => 8,
        PrimTy::I16 | PrimTy::U16 => 16,
        PrimTy::I32 | PrimTy::U32 => 32,
        PrimTy::I64 | PrimTy::U64 | PrimTy::Usize | PrimTy::Isize => 64,
    }
}

/// Whether a primitive is a target-pointer-width integer. v0 is fixed
/// at 64-bit pointers, so the predicate matches `i64`, `u64`, `usize`,
/// `isize`. Used by `emit_transmute` to gate the `ptrtoint`/`inttoptr`
/// arms (LLVM rejects bitcast between non-ptr-width int and ptr).
pub fn is_ptr_width_int(p: PrimTy) -> bool {
    matches!(
        p,
        PrimTy::I64 | PrimTy::U64 | PrimTy::Usize | PrimTy::Isize
    )
}

/// Resolve a `TyId` to its `PrimTy`. Used by binary-op codegen to pick
/// signed vs unsigned opcodes from the operand type.
pub fn as_prim(tcx: &TyArena, ty: TyId) -> Option<PrimTy> {
    match tcx.kind(ty) {
        TyKind::Prim(p) => Some(*p),
        _ => None,
    }
}
