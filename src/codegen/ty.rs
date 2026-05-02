//! `TyId` → LLVM type lowering. Signedness lives on the *operations*,
//! not the LLVM type — `i32` and `u32` both lower to LLVM `i32`.
//!
//! ADTs need a per-`AdtId` cache because LLVM struct types are by
//! identity (each `opaque_struct_type` call creates a fresh distinct
//! type). Callers populate the cache once via `prepare_adt_types` and
//! pass it to `lower_ty` / `lower_fn_type` thereafter.

use index_vec::IndexVec;
use inkwell::context::Context;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, StructType};

use crate::hir::VariantIdx;
use crate::typeck::{AdtId, FnSig, PrimTy, TyArena, TyId, TyKind, TypeckResults};

/// Per-`AdtId` cache of the LLVM struct types. Indexed by `AdtId`; entry
/// `aid` is the type used for any `TyKind::Adt(aid)` lowering.
pub type AdtLlTypes<'ctx> = IndexVec<AdtId, StructType<'ctx>>;

/// Two-phase ADT type construction (mirrors typeck's phase 0 / 0.5):
///   - Phase A: `opaque_struct_type` for every adt, so each gets a stable
///     identity before any field type is resolved.
///   - Phase B: `set_body` for every adt, recursively lowering each
///     declared field type (which may itself be an `Adt(_)` referring
///     to one of the now-allocated opaque handles).
///
/// Self-referential types via pointer (`struct Node { next: *const Node }`)
/// resolve cleanly because `Ptr` lowers to opaque `ptr` without recursing
/// into the pointee. Direct self-containment (`struct A { x: A }`) is
/// rejected by typeck (TBD-T2) before we get here.
pub fn prepare_adt_types<'ctx>(
    ctx: &'ctx Context,
    typeck_results: &TypeckResults,
) -> AdtLlTypes<'ctx> {
    let mut adt_ll: AdtLlTypes<'ctx> = IndexVec::with_capacity(typeck_results.adts.len());
    for adt in typeck_results.adts.iter() {
        adt_ll.push(ctx.opaque_struct_type(&adt.name));
    }
    for (aid, adt) in typeck_results.adts.iter_enumerated() {
        let fields: Vec<BasicTypeEnum<'ctx>> = adt.variants[VariantIdx::from_raw(0)]
            .fields
            .iter()
            .map(|f| lower_ty(ctx, typeck_results.tys(), &adt_ll, f.ty))
            .collect();
        adt_ll[aid].set_body(&fields, false);
    }
    adt_ll
}

/// Lower a `TyId` to a `BasicTypeEnum`. `Unit` lowers to LLVM `{}`
/// (zero-sized empty struct, sole inhabitant `{} undef`). Panics on
/// `Never` (no value form ever — `!`-typed expressions terminate the
/// BB before any consumer reaches lower_ty), `Fn` (use `lower_fn_type`),
/// or post-typeck poison (`Infer`/`Error`).
pub fn lower_ty<'ctx>(
    ctx: &'ctx Context,
    tcx: &TyArena,
    adt_ll: &AdtLlTypes<'ctx>,
    ty: TyId,
) -> BasicTypeEnum<'ctx> {
    match tcx.kind(ty) {
        TyKind::Prim(p) => lower_prim(ctx, *p).into(),
        TyKind::Ptr(..) => ctx.ptr_type(inkwell::AddressSpace::default()).into(),
        TyKind::Adt(aid) => adt_ll[*aid].as_basic_type_enum(),
        TyKind::Unit => ctx.struct_type(&[], false).into(),
        TyKind::Never => panic!(
            "lower_ty called on Never — !-typed expressions terminate \
             the BB before any consumer asks for a slot"
        ),
        TyKind::Fn(_, _) => panic!("lower_ty called on Fn — use lower_fn_type"),
        TyKind::Array(elem, Some(n)) => {
            let elem_ll = lower_ty(ctx, tcx, adt_ll, *elem);
            elem_ll.array_type(*n as u32).into()
        }
        TyKind::Array(_, None) => {
            unreachable!("Array(_, None) is not a value type; typeck E0269 should have rejected")
        }
        TyKind::Infer(_) | TyKind::Error => {
            panic!("post-typeck type is unresolved: {}", tcx.render(ty))
        }
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

/// Build a `FunctionType` from a typecheck `FnSig`. Unit/Never returns
/// become LLVM `void`.
///
/// Array params lower to LLVM `ptr` (manual byval ABI per
/// spec/09_ARRAY.md): the caller copies into a fresh slot and passes
/// the pointer; the callee uses the incoming ptr directly as the
/// param's storage. Array *returns* lower normally to `[N x T]` and
/// rely on LLVM's calling-convention machinery to pick sret /
/// register-return per target — Path A in the codegen plan.
pub fn lower_fn_type<'ctx>(
    ctx: &'ctx Context,
    tcx: &TyArena,
    adt_ll: &AdtLlTypes<'ctx>,
    sig: &FnSig,
) -> FunctionType<'ctx> {
    let params: Vec<BasicMetadataTypeEnum<'ctx>> = sig
        .params
        .iter()
        .map(|&p| {
            if let TyKind::Array(_, Some(_)) = tcx.kind(p) {
                ctx.ptr_type(inkwell::AddressSpace::default()).into()
            } else {
                lower_ty(ctx, tcx, adt_ll, p).into()
            }
        })
        .collect();
    if is_void_ret(tcx, sig.ret) {
        ctx.void_type().fn_type(&params, false)
    } else {
        lower_ty(ctx, tcx, adt_ll, sig.ret).fn_type(&params, false)
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

/// Resolve a `TyId` to its `PrimTy`. Used by binary-op codegen to pick
/// signed vs unsigned opcodes from the operand type.
pub fn as_prim(tcx: &TyArena, ty: TyId) -> Option<PrimTy> {
    match tcx.kind(ty) {
        TyKind::Prim(p) => Some(*p),
        _ => None,
    }
}
