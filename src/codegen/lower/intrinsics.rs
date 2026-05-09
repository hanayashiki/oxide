//! Compiler intrinsic codegen recipes — `ox_size_of` and
//! `ox_transmute`. Both are call-only: they synthesize values rather
//! than calling a function. Reached only through `CallLike::resolve`'s
//! direct/generic path, after `mono::instantiate` stamps a non-`Call`
//! `InstanceOperation`. See spec/17_LAYOUT.md §Codegen.

use inkwell::values::{BasicValue, BasicValueEnum};

use crate::codegen::lower::{Codegen, FnCodegenContext, Operand};
use crate::codegen::ty::{is_ptr_width_int, lower_prim, prim_bit_width};
use crate::hir::HExprId;
use crate::mono::InstanceOperation;
use crate::typeck::{TyId, TyKind, layout};

/// Per-instance dispatch wrapper. Cheap to construct (single-field
/// move); the `emit` body is the only logic.
pub(super) struct IntrinsicEmitter(pub InstanceOperation);

impl IntrinsicEmitter {
    /// Produce an `Operand` for the intrinsic's result. `src_ty` /
    /// `dst_ty` are only consumed in the `Transmute` branch — the
    /// caller (`CallLike::resolve`) already has them in hand from the
    /// instance, so passing them here keeps this module side-effect-
    /// free w.r.t. mono lookups.
    pub(super) fn emit<'a, 'ctx>(
        &self,
        codegen: &mut Codegen<'a, 'ctx>,
        fx: &mut FnCodegenContext<'ctx>,
        args: &[HExprId],
        src_ty: &[TyId],
        dst_ty: TyId,
    ) -> Operand<'ctx> {
        match &self.0 {
            InstanceOperation::SizeOf { size } => {
                Operand::Value(codegen.ctx.i64_type().const_int(*size, false).into())
            }
            InstanceOperation::Transmute => {
                debug_assert_eq!(args.len(), 1, "ox_transmute takes one argument");
                let src_ty = *src_ty.first().expect("transmute instance has Src in params[0]");
                let arg_op = codegen
                    .emit_expr(fx, args[0])
                    .expect("transmute arg must produce a value");
                emit_transmute(codegen, fx, arg_op, src_ty, dst_ty)
                    .expect("transmute lowering produced no value")
            }
            InstanceOperation::Call => {
                unreachable!("Call dispatched in CallLike::resolve before reaching IntrinsicEmitter")
            }
        }
    }
}

/// `ox_transmute<Src, Dst>(x)` — bit-copy reinterpret. Size equality
/// is enforced by the per-instance E0276 check at mono time, so
/// codegen trusts it and doesn't recheck. Dispatches structurally on
/// `(Src kind, Dst kind)` per spec/17_LAYOUT.md §Codegen:
///
/// | (Src, Dst)                  | LLVM op                      |
/// |-----------------------------|------------------------------|
/// | (Prim, Prim) same width     | bitcast (no-op for same int) |
/// | (Ptr, Ptr)                  | no-op (LLVM ptr is opaque)   |
/// | (Ptr, Prim) ptr-width int   | ptrtoint                     |
/// | (Prim, Ptr) ptr-width int   | inttoptr                     |
/// | all other size-equal pairs  | alloca + store + load        |
///
/// The fallback uses an alloca sized for `Src` (which equals `Dst`
/// in size by E0276), with alignment = max(align(Src), align(Dst))
/// to keep both the store and the load aligned. Spec is silent on
/// which side's alignment to use; max is the safe default. The
/// alloca lands in the fn's dedicated `allocas:` entry block via
/// `alloca_in_entry` — placing it inline at the call site would
/// hide the slot from `mem2reg` and `SROA` and bloat `-O0` IR.
fn emit_transmute<'a, 'ctx>(
    codegen: &mut Codegen<'a, 'ctx>,
    fx: &FnCodegenContext<'ctx>,
    arg_op: Operand<'ctx>,
    src_ty: TyId,
    dst_ty: TyId,
) -> Option<Operand<'ctx>> {
    // Identity (Src == Dst): no-op. The load_value below would be
    // a no-op anyway, but skipping it preserves the input shape
    // (Place stays Place, etc.).
    if src_ty == dst_ty {
        return Some(arg_op);
    }

    let src_kind = codegen.typeck_results.tys().kind(src_ty).clone();
    let dst_kind = codegen.typeck_results.tys().kind(dst_ty).clone();

    // (Ptr, Ptr) — LLVM `ptr` is opaque; mutability/pointee
    // information lives in the Oxide type system only. Just thread
    // the operand through without touching the LLVM value.
    if matches!(&src_kind, TyKind::Ptr(..)) && matches!(&dst_kind, TyKind::Ptr(..)) {
        return Some(arg_op);
    }

    // For everything below we need the SSA-form input value.
    let src_val: BasicValueEnum<'ctx> = arg_op.load_value(codegen, src_ty, "transmute.in");

    // (Prim, Prim) same width — emit `bitcast`.
    if let (TyKind::Prim(sp), TyKind::Prim(dp)) = (&src_kind, &dst_kind) {
        if prim_bit_width(*sp) == prim_bit_width(*dp) {
            let dst_ll = lower_prim(codegen.ctx, *dp);
            let result = codegen
                .builder
                .build_bit_cast(src_val.into_int_value(), dst_ll, "transmute.bc")
                .unwrap();
            return Some(Operand::Value(result));
        }
        // Same-Prim with different widths shouldn't reach codegen —
        // mono's E0276 already rejected it. Fall through to the
        // alloca fallback for defense-in-depth.
    }

    // (Ptr, Prim) ptr-width int — emit `ptrtoint`. Only ptr-width
    // primitives are valid here (8 bytes on supported targets);
    // E0276 enforces this at mono time.
    if let (TyKind::Ptr(..), TyKind::Prim(dp)) = (&src_kind, &dst_kind) {
        if is_ptr_width_int(*dp) {
            let dst_ll = lower_prim(codegen.ctx, *dp);
            let result = codegen
                .builder
                .build_ptr_to_int(src_val.into_pointer_value(), dst_ll, "transmute.p2i")
                .unwrap();
            return Some(Operand::Value(result.into()));
        }
    }
    if let (TyKind::Prim(sp), TyKind::Ptr(..)) = (&src_kind, &dst_kind) {
        if is_ptr_width_int(*sp) {
            let dst_ll = codegen.ctx.ptr_type(inkwell::AddressSpace::default());
            let result = codegen
                .builder
                .build_int_to_ptr(src_val.into_int_value(), dst_ll, "transmute.i2p")
                .unwrap();
            return Some(Operand::Value(result.into()));
        }
    }

    // Fallback: alloca + store + load.
    let src_ll = codegen.lower_ty(src_ty);
    let dst_ll = codegen.lower_ty(dst_ty);
    let max_align = layout::align_of(codegen.typeck_results, src_ty)
        .unwrap_or(1)
        .max(layout::align_of(codegen.typeck_results, dst_ty).unwrap_or(1))
        as u32;

    let slot = codegen.alloca_in_entry(fx, src_ll, "transmute.slot");
    if let Some(inst) = slot.as_instruction() {
        inst.set_alignment(max_align).ok();
    }
    let store = codegen.builder.build_store(slot, src_val).unwrap();
    store.set_alignment(max_align).ok();
    let load = codegen
        .builder
        .build_load(dst_ll, slot, "transmute.out")
        .unwrap();
    if let Some(inst) = load.as_instruction_value() {
        inst.set_alignment(max_align).ok();
    }
    Some(Operand::Value(load))
}
