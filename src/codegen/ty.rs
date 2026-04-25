//! `TyId` → LLVM type lowering. Signedness lives on the *operations*,
//! not the LLVM type — `i32` and `u32` both lower to LLVM `i32`.

use inkwell::context::Context;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType};

use crate::typeck::{FnSig, PrimTy, TyArena, TyId, TyKind};

/// Lower a `TyId` to a `BasicTypeEnum`. Panics on `Unit`/`Never` (not
/// a value), `Fn` (use `lower_fn_type`), or post-typeck poison
/// (`Infer`/`Error`).
pub fn lower_ty<'ctx>(ctx: &'ctx Context, tcx: &TyArena, ty: TyId) -> BasicTypeEnum<'ctx> {
    match tcx.kind(ty) {
        TyKind::Prim(p) => lower_prim(ctx, *p).into(),
        TyKind::Ptr(_) => ctx.ptr_type(inkwell::AddressSpace::default()).into(),
        TyKind::Unit | TyKind::Never => {
            panic!("lower_ty called on non-value type {}", tcx.render(ty))
        }
        TyKind::Fn(_, _) => panic!("lower_ty called on Fn — use lower_fn_type"),
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
        PrimTy::I64 | PrimTy::U64 => ctx.i64_type(),
        PrimTy::Bool => ctx.bool_type(),
    }
}

/// Build a `FunctionType` from a typecheck `FnSig`. Unit/Never returns
/// become LLVM `void`.
pub fn lower_fn_type<'ctx>(
    ctx: &'ctx Context,
    tcx: &TyArena,
    sig: &FnSig,
) -> FunctionType<'ctx> {
    let params: Vec<BasicMetadataTypeEnum<'ctx>> = sig
        .params
        .iter()
        .map(|&p| lower_ty(ctx, tcx, p).into())
        .collect();
    if is_void_ret(tcx, sig.ret) {
        ctx.void_type().fn_type(&params, false)
    } else {
        lower_ty(ctx, tcx, sig.ret).fn_type(&params, false)
    }
}

/// `()` and `!` both surface as `void` returns / no value at the IR level.
pub fn is_void_ret(tcx: &TyArena, ty: TyId) -> bool {
    matches!(tcx.kind(ty), TyKind::Unit | TyKind::Never)
}

/// Whether a primitive participates in *signed* integer ops (sdiv, ashr,
/// icmp slt, etc.).
pub fn is_signed_prim(p: PrimTy) -> bool {
    matches!(p, PrimTy::I8 | PrimTy::I16 | PrimTy::I32 | PrimTy::I64)
}

/// Resolve a `TyId` to its `PrimTy`. Used by binary-op codegen to pick
/// signed vs unsigned opcodes from the operand type.
pub fn as_prim(tcx: &TyArena, ty: TyId) -> Option<PrimTy> {
    match tcx.kind(ty) {
        TyKind::Prim(p) => Some(*p),
        _ => None,
    }
}
