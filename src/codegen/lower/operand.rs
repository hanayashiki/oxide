use inkwell::values::{BasicValueEnum, PointerValue};

use crate::typeck::TyId;

/// The form a value-producing expression takes after lowering. This is the
/// central place-vs-value abstraction in codegen — every `emit_expr` site
/// returns one of these (or `None` for divergence).
///
///   - `Value(_)` — live SSA value (Int, Bool, Ptr, Struct).
///   - `Place(_)` — memory-backed value. The pointer is opaque LLVM `ptr`;
///     the type lives alongside via the consumer's `TyId`. Reads via
///     `load(llty, ptr)`; writes/copies via `memcpy(sizeof(ty))`. Today
///     only `Array(_, Some(_))`-typed expressions ever produce `Place`;
///     everything else is `Value` or `Unit`.
///   - `Unit`    — zero-sized canonical value of type `()`. Materializes
///     as `{} undef` when an SSA form is needed; no-op when stored.
///
/// No `From`/`Into` impls — `PointerValue` is ambiguous between Value
/// (a `BasicValueEnum::PointerValue`) and Place (a slot ptr), and the
/// place-vs-value choice is the whole point of this abstraction. Every
/// constructor site spells the variant explicitly.
#[derive(Copy, Clone, Debug)]
pub(in crate::codegen) enum Operand<'ctx> {
    Value(BasicValueEnum<'ctx>),
    Place(PointerValue<'ctx>),
    Unit,
}

impl<'ctx> Operand<'ctx> {
    // ---------- Operand helpers ----------

    /// Materialize an `Operand` into a destination slot. The only place
    /// in codegen that ever stores anything to a slot.
    ///
    ///   - `Value` ⇒ `build_store` (LLVM lowers aggregates).
    ///   - `Place` ⇒ `memcpy(sizeof(ty))` — type-driven size; works for
    ///     any sized type, not just arrays.
    ///   - `Unit`  ⇒ no-op. The `{}`-typed slot needs no init; mem2reg
    ///     removes the dead alloca.
    ///
    /// Shared by `emit_let`, `emit_assign`, `emit_if`, `emit_loop`,
    /// `emit_break`, anywhere a value flows into memory.
    pub(in crate::codegen) fn store_into<'a>(
        self,
        codegen: &mut super::Codegen<'a, 'ctx>,
        dst: PointerValue<'ctx>,
        ty: TyId,
    ) {
        match self {
            Operand::Value(v) => {
                codegen.builder.build_store(dst, v).unwrap();
            }
            Operand::Place(src) => codegen.emit_memcpy(dst, src, ty),
            Operand::Unit => {}
        }
    }

    /// Force an `Operand` to SSA-value form. Loads from memory if the
    /// operand is a Place; passes through Value; materializes `{} undef`
    /// for Unit. `name` is the LLVM SSA name suffix for the generated
    /// `load` (consumed only when the operand is a Place).
    pub(in crate::codegen) fn load_value<'a>(
        self,
        codegen: &mut super::Codegen<'a, 'ctx>,
        ty: TyId,
        name: &str,
    ) -> BasicValueEnum<'ctx> {
        match self {
            Operand::Value(v) => v,
            Operand::Place(p) => {
                let llty = codegen.lower_ty(ty);
                codegen.builder.build_load(llty, p, name).unwrap()
            }
            Operand::Unit => codegen.ctx.struct_type(&[], false).get_undef().into(),
        }
    }
}
