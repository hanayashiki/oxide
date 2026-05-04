//! `TyId` → LLVM type lowering. Signedness lives on the *operations*,
//! not the LLVM type — `i32` and `u32` both lower to LLVM `i32`.
//!
//! Generic ADTs are materialized **lazily** at codegen, keyed on
//! `(AdtId, Vec<TyId>)`. Each distinct instantiation gets its own LLVM
//! struct type — `LinkedList<i32>` and `LinkedList<u8>` produce two
//! different layouts. Hash-cons at the TyId level dedupes the cache:
//! structurally-identical instances reach the same key. See
//! spec/16_GENERIC.md §Codegen (extension).

use std::collections::HashMap;

use inkwell::context::Context;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, StructType};

use crate::hir::VariantIdx;
use crate::typeck::{AdtId, PrimTy, TyArena, TyId, TyKind, TypeckResults, subst_from};

/// Per-`(AdtId, Vec<TyId>)` cache of LLVM struct types. The args list
/// is empty for non-generic ADTs and saturates the ADT's
/// `generic_params` for generic instantiations. See
/// spec/16_GENERIC.md §Codegen (extension).
pub type AdtLlTypes<'ctx> = HashMap<(AdtId, Vec<TyId>), StructType<'ctx>>;

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
pub fn lower_adt_type<'ctx>(
    ctx: &'ctx Context,
    typeck: &mut TypeckResults,
    adt_ll: &mut AdtLlTypes<'ctx>,
    aid: AdtId,
    args: &[TyId],
) -> StructType<'ctx> {
    let key = (aid, args.to_vec());
    if let Some(&st) = adt_ll.get(&key) {
        return st;
    }
    let display_name = render_adt_instance_name(typeck, aid, args);
    let opaque = ctx.opaque_struct_type(&display_name);
    adt_ll.insert(key, opaque);

    // Build subst + snapshot field decl types in a tight `&typeck.adts`
    // borrow scope. The borrow ends at block-exit so the loop below is
    // free to call `&mut typeck` for substitution and recursion.
    // For non-generic ADTs the subst is empty and `substitute_ty` is
    // identity (hash-cons returns the same TyId).
    let (subst, field_decl_tys) = {
        let adt = &typeck.adts[aid];
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
            let concrete = typeck.tys.substitute_ty(f_ty, &subst);
            lower_ty(ctx, typeck, adt_ll, concrete)
        })
        .collect();
    opaque.set_body(&fields_ll, false);
    opaque
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

/// Lower a `TyId` to a `BasicTypeEnum`. `Unit` lowers to LLVM `{}`
/// (zero-sized empty struct, sole inhabitant `{} undef`). Panics on
/// `Never` (no value form ever — `!`-typed expressions terminate the
/// BB before any consumer reaches lower_ty), `Fn` (use `lower_fn_type`),
/// or post-typeck poison (`Infer`/`Error`).
///
/// Takes `&mut TypeckResults` and `&mut AdtLlTypes` because lazy
/// materialization of `Adt(aid, args)` interns substituted types into
/// the arena and inserts into the LLVM struct cache. For all non-Adt
/// arms this is a no-op on the arena (no substitution); for Adt arms
/// the work is hash-cons-cheap (most types are already interned).
pub fn lower_ty<'ctx>(
    ctx: &'ctx Context,
    typeck: &mut TypeckResults,
    adt_ll: &mut AdtLlTypes<'ctx>,
    ty: TyId,
) -> BasicTypeEnum<'ctx> {
    let kind = typeck.tys.kind(ty).clone();
    match kind {
        TyKind::Prim(p) => lower_prim(ctx, p).into(),
        TyKind::Ptr(..) => ctx.ptr_type(inkwell::AddressSpace::default()).into(),
        TyKind::Adt(aid, args) => {
            lower_adt_type(ctx, typeck, adt_ll, aid, &args).as_basic_type_enum()
        }
        TyKind::Unit => ctx.struct_type(&[], false).into(),
        TyKind::Never => panic!(
            "lower_ty called on Never — !-typed expressions terminate \
             the BB before any consumer asks for a slot"
        ),
        TyKind::Fn(_, _, _) => panic!("lower_ty called on Fn — use lower_fn_type"),
        TyKind::Array(elem, Some(n)) => {
            let elem_ll = lower_ty(ctx, typeck, adt_ll, elem);
            elem_ll.array_type(n as u32).into()
        }
        TyKind::Array(_, None) => {
            unreachable!("Array(_, None) is not a value type; typeck E0269 should have rejected")
        }
        TyKind::Infer(_) | TyKind::Error => {
            panic!("post-typeck type is unresolved: {}", typeck.tys.render(ty))
        }
        // Phase D (mono) substitutes Param leaves into concrete types
        // before codegen runs. If a Param survives into codegen, it's a
        // mono bug (or the driver ran codegen on an errored mono).
        TyKind::Param(_) => {
            panic!(
                "lower_ty called on Param — mono should have substituted: {}",
                typeck.tys.render(ty)
            )
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
pub fn lower_fn_type<'ctx>(
    ctx: &'ctx Context,
    typeck: &mut TypeckResults,
    adt_ll: &mut AdtLlTypes<'ctx>,
    params: &[TyId],
    ret: TyId,
    c_variadic: bool,
) -> FunctionType<'ctx> {
    let lowered_params: Vec<BasicMetadataTypeEnum<'ctx>> = params
        .iter()
        .map(|&p| {
            if let TyKind::Array(_, Some(_)) = typeck.tys.kind(p) {
                ctx.ptr_type(inkwell::AddressSpace::default()).into()
            } else {
                lower_ty(ctx, typeck, adt_ll, p).into()
            }
        })
        .collect();
    if is_void_ret(&typeck.tys, ret) {
        ctx.void_type().fn_type(&lowered_params, c_variadic)
    } else {
        lower_ty(ctx, typeck, adt_ll, ret).fn_type(&lowered_params, c_variadic)
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
