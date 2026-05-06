//! Call-site lowering. Splits the previously monolithic `emit_call`
//! into a two-step pipeline:
//!
//!   1. `CallLike::resolve` classifies the callee — either an already-
//!      computed value (intrinsic recipe in `Inlined`) or a real call
//!      with concrete dispatch info (`Call`).
//!   2. `CallLike::emit` (delegating to `Call::emit` for the call case)
//!      materializes args, issues `build_call` / `build_indirect_call`,
//!      and wraps the return.
//!
//! All IR-builder work for calls lives here — `lower.rs::emit_call` is
//! a two-line coordinator. See spec/19_FN_PTR.md §6 + the follow-up
//! plan at .claude/plans/review-viability-of-spec-19-fn-ptr-md-witty-karp.md.

use inkwell::types::FunctionType;
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, CallSiteValue, FunctionValue, PointerValue,
};

use crate::codegen::lower::{Codegen, FnCodegenContext, Operand, intrinsics::IntrinsicEmitter};
use crate::codegen::ty::{is_void_ret, lower_fn_type};
use crate::hir::{HExprId, HirExprKind};
use crate::mono::{Instance, InstanceOperation};
use crate::typeck::{PrimTy, TyId, TyKind};

/// Resolved callee — either a known fn (direct dispatch) or a fn-ptr
/// value (indirect dispatch). `Copy` because both inner inkwell types
/// are Copy.
#[derive(Copy, Clone)]
pub(super) enum Callee<'ctx> {
    /// Direct call to a known FunctionValue (non-generic, extern, or
    /// post-mono-resolved generic instance).
    Fn(FunctionValue<'ctx>),
    /// Indirect call through a fn-ptr value. `fn_ty` carries the
    /// FunctionType `build_indirect_call` requires.
    Ptr {
        ptr: PointerValue<'ctx>,
        fn_ty: FunctionType<'ctx>,
    },
}

/// A real call, ready to be issued. Carries the params/return/variadic
/// info the dispatcher needs so neither this module's caller nor
/// `Call::emit` has to revisit typeck/mono.
pub(super) struct Call<'ctx> {
    pub callee: Callee<'ctx>,
    pub param_tys: Vec<TyId>,
    pub ret_ty: TyId,
    pub c_variadic: bool,
}

/// What `CallLike::resolve` produces. Either the call already happened
/// (intrinsic short-circuit produced a value directly) or there's a
/// real call to issue.
pub(super) enum CallLike<'ctx> {
    /// Intrinsic recipe ran during `resolve` — no further IR work.
    Inlined(Operand<'ctx>),
    /// Regular call data ready for `Call::emit`.
    Call(Call<'ctx>),
}

impl<'ctx> CallLike<'ctx> {
    /// Step 1 — classify the callee. Three cases:
    ///   - `HirExprKind::Fn(fid)` with a `fn_ref_type_args` entry →
    ///     generic direct call. Mono lookup may early-resolve to an
    ///     inlined intrinsic (`SizeOf` / `Transmute`).
    ///   - `HirExprKind::Fn(fid)` without an entry → non-generic /
    ///     extern direct call via `fn_decls`.
    ///   - Anything else → indirect: snapshot `TyKind::Fn`, build the
    ///     `FunctionType`, evaluate the callee, strict-match its
    ///     `Operand::Value(BasicValueEnum::PointerValue(_))`.
    pub(super) fn resolve<'a>(
        codegen: &mut Codegen<'a, 'ctx>,
        fx: &mut FnCodegenContext<'ctx>,
        callee_eid: HExprId,
        args: &[HExprId],
    ) -> Self {
        // Clone the kind to release the &codegen.hir borrow before we
        // recurse into &mut codegen further down.
        let callee_kind = codegen.hir.exprs[callee_eid].kind.clone();
        match callee_kind {
            HirExprKind::Fn(fid) => {
                // Direct path. Discriminator: fn_ref_type_args has an
                // entry iff the callee is generic (Decision F1).
                let typeck_args_opt: Option<Vec<TyId>> = codegen
                    .typeck_results
                    .fn_ref_type_args
                    .get(&callee_eid)
                    .cloned();
                // Snapshot the per-instance bits we need so the
                // `&Instance` borrow ends before we hand `&mut codegen`
                // to the intrinsic emitter or index `inst_decls`.
                let maybe_inst = codegen
                    .resolve_instance(fx, fid, typeck_args_opt)
                    .and_then(|(id, inst)| Some((id, inst.clone())));

                if let Some((
                    inst_id,
                    Instance {
                        fid,
                        operation,
                        param_tys,
                        ret_ty,
                        ..
                    },
                )) = maybe_inst
                {
                    match operation {
                        InstanceOperation::SizeOf { .. } | InstanceOperation::Transmute => {
                            let inlined = IntrinsicEmitter(operation.clone())
                                .emit(codegen, fx, args, &param_tys, ret_ty);
                            return CallLike::Inlined(inlined);
                        }
                        InstanceOperation::Call => {
                            let fnv = codegen.inst_decls[inst_id].expect(
                                "Call-operation generic instance must have an LLVM declaration",
                            );
                            CallLike::Call(Call {
                                callee: Callee::Fn(fnv),
                                param_tys,
                                ret_ty,
                                c_variadic: codegen.hir.fns[fid].is_variadic,
                            })
                        }
                    }
                } else {
                    let sig = codegen.typeck_results.fn_sig(fid);
                    let fnv = codegen.fn_decls[fid].expect(
                        "non-generic / extern callee should have a \
                         fn_decls entry",
                    );
                    CallLike::Call(Call {
                        callee: Callee::Fn(fnv),
                        param_tys: sig.params.clone(),
                        ret_ty: sig.ret,
                        c_variadic: sig.c_variadic,
                    })
                }
            }
            _ => {
                // Indirect path. Callee is anything other than
                // HirExprKind::Fn — a Local of fn type, a Field load,
                // a Deref, etc. Typeck guarantees its type is Fn (else
                // NotCallable would have fired upstream).
                let callee_ty = codegen.ty_of(fx, callee_eid);
                let (params_vec, ret_ty, c_variadic) =
                    match codegen.typeck_results.tys().kind(callee_ty) {
                        TyKind::Fn {
                            params,
                            ret,
                            c_variadic,
                            ..
                        } => (params.clone(), *ret, *c_variadic),
                        _ => panic!(
                            "indirect callee did not type as Fn — typeck \
                         should have rejected via NotCallable"
                        ),
                    };

                // FunctionType for build_indirect_call. is_extern_c is
                // a typeck-only concept; LLVM CC is the default for
                // both forms in v0.
                let fn_ty = lower_fn_type(
                    codegen.ctx,
                    codegen.typeck_results,
                    &mut codegen.adt_ll,
                    &params_vec,
                    ret_ty,
                    c_variadic,
                );

                // Evaluate the callee. Invariant: every emit_expr arm
                // that produces a Fn-typed value returns
                // Operand::Value(PointerValue) directly — Local-of-fn
                // loads, Fn(fid)-as-value returns the global ptr, and
                // Deref / Field pre-load. Strict-match guards that
                // invariant; any future regression panics loud.
                let callee_op = codegen
                    .emit_expr(fx, callee_eid)
                    .expect("indirect callee produced no value");
                let ptr = match callee_op {
                    Operand::Value(BasicValueEnum::PointerValue(p)) => p,
                    _ => panic!(
                        "Fn-typed value did not lower to a pointer — \
                         emit_expr arm produced {:?} for a Fn-typed \
                         callee",
                        callee_op
                    ),
                };

                CallLike::Call(Call {
                    callee: Callee::Ptr { ptr, fn_ty },
                    param_tys: params_vec,
                    ret_ty,
                    c_variadic,
                })
            }
        }
    }

    /// Step 2 — pass through inlined; emit real `Call`s.
    pub(super) fn emit<'a>(
        self,
        codegen: &mut Codegen<'a, 'ctx>,
        fx: &mut FnCodegenContext<'ctx>,
        args: &[HExprId],
    ) -> Option<Operand<'ctx>> {
        match self {
            CallLike::Inlined(op) => Some(op),
            CallLike::Call(c) => c.emit(codegen, fx, args),
        }
    }
}

impl<'ctx> Call<'ctx> {
    /// Materialize args + dispatch on `Callee` shape + wrap the
    /// returned `CallSiteValue`.
    pub(super) fn emit<'a>(
        self,
        codegen: &mut Codegen<'a, 'ctx>,
        fx: &mut FnCodegenContext<'ctx>,
        args: &[HExprId],
    ) -> Option<Operand<'ctx>> {
        let arg_vals = self.materialize_args(codegen, fx, args)?;
        let csv = match self.callee {
            Callee::Fn(fnv) => codegen.builder.build_call(fnv, &arg_vals, "call").unwrap(),
            Callee::Ptr { ptr, fn_ty } => codegen
                .builder
                .build_indirect_call(fn_ty, ptr, &arg_vals, "call")
                .unwrap(),
        };
        self.wrap_return(codegen, fx, csv)
    }

    /// Two-phase argument materialization shared by direct and indirect
    /// calls. Per spec/09_ARRAY.md: byval ABI for sized-array params —
    /// caller owns a fresh slot, copies from the source operand,
    /// passes the slot ptr. Per spec/15_VARIADIC.md: variadic args go
    /// through `promote_for_variadic` (sext/zext narrows to `i32`).
    pub(super) fn materialize_args<'a>(
        &self,
        codegen: &mut Codegen<'a, 'ctx>,
        fx: &mut FnCodegenContext<'ctx>,
        args: &[HExprId],
    ) -> Option<Vec<BasicMetadataValueEnum<'ctx>>> {
        let n_fixed = self.param_tys.len();
        let mut arg_vals: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
        for (i, &a) in args.iter().enumerate() {
            let arg_ty = codegen.ty_of(fx, a);
            let op = codegen.emit_expr(fx, a)?;
            if i < n_fixed {
                // Decide byval-spill on the *param* type (callee's ABI),
                // not the arg type — generic-instance param types may
                // differ from the raw FnSig (post-substitution).
                let pty = self.param_tys[i];
                if codegen.is_sized_array(pty) {
                    let fresh = codegen.spill_to_place_fresh(fx, op, arg_ty, "call.arg.slot");
                    arg_vals.push(fresh.into());
                } else {
                    arg_vals.push(op.load_value(codegen, arg_ty, "load").into());
                }
            } else {
                debug_assert!(
                    self.c_variadic,
                    "extra args past n_fixed only on c_variadic call"
                );
                arg_vals.push(promote_for_variadic(codegen, op, arg_ty).into());
            }
        }
        Some(arg_vals)
    }

    /// Wrap the inkwell `CallSiteValue` into an emit_expr-shaped
    /// `Operand`:
    ///   - `()` / `!` → `Operand::Unit`
    ///   - sized-array → spill the returned aggregate to a Place slot
    ///   - everything else → `Operand::Value`
    pub(super) fn wrap_return<'a>(
        &self,
        codegen: &mut Codegen<'a, 'ctx>,
        fx: &mut FnCodegenContext<'ctx>,
        call: CallSiteValue<'ctx>,
    ) -> Option<Operand<'ctx>> {
        if is_void_ret(codegen.typeck_results.tys(), self.ret_ty) {
            return Some(Operand::Unit);
        }
        if codegen.is_sized_array(self.ret_ty) {
            let v = call
                .try_as_basic_value()
                .left()
                .expect("array-returning call produced no value");
            let slot =
                codegen.spill_to_place_fresh(fx, Operand::Value(v), self.ret_ty, "call.ret.slot");
            return Some(Operand::Place(slot));
        }
        Some(Operand::Value(
            call.try_as_basic_value()
                .left()
                .expect("non-void call produced no value"),
        ))
    }
}

/// C default argument promotion for variadic args. C11 §6.5.2.2 ¶7
/// requires the *caller* to perform the "default argument promotions"
/// (defined in ¶6: integer promotions, `float`→`double`) on every
/// trailing arg past the `...`. The receiver relies on this — C11
/// §7.16.1.1 ¶2 makes `va_arg(args, T)` UB if `T` doesn't match the
/// actual-arg type *as promoted*. So a `u8` must arrive as `i32`
/// before the call, not after.
///
/// LLVM's `isVarArg=true` only covers per-platform ABI lowering
/// (register classification, save area, `%al`, `va_start`); it does
/// **not** insert this promotion. clang emits the same sext/zext.
///
/// Table: signed-narrow (`i8`/`i16`) sign-extend to `i32`;
/// unsigned-narrow + `bool` zero-extend to `i32`; `i32`/`u32`/`i64`/
/// `u64`/`isize`/`usize`/`Ptr(_, _)` pass through. Anything else is
/// unreachable — typeck E0272 already rejected it at the call site.
fn promote_for_variadic<'a, 'ctx>(
    codegen: &mut Codegen<'a, 'ctx>,
    op: Operand<'ctx>,
    ty: TyId,
) -> BasicValueEnum<'ctx> {
    let v = op.load_value(codegen, ty, "load");
    match codegen.typeck_results.tys().kind(ty) {
        TyKind::Prim(p) => match p {
            PrimTy::I8 | PrimTy::I16 => codegen
                .builder
                .build_int_s_extend(v.into_int_value(), codegen.ctx.i32_type(), "sext")
                .unwrap()
                .into(),
            PrimTy::U8 | PrimTy::U16 | PrimTy::Bool => codegen
                .builder
                .build_int_z_extend(v.into_int_value(), codegen.ctx.i32_type(), "zext")
                .unwrap()
                .into(),
            PrimTy::I32
            | PrimTy::U32
            | PrimTy::I64
            | PrimTy::U64
            | PrimTy::Isize
            | PrimTy::Usize => v,
        },
        TyKind::Ptr(..) => v,
        _ => unreachable!(
            "promote_for_variadic: typeck E0272 should have rejected non-promotable variadic arg type {:?}",
            codegen.typeck_results.tys().kind(ty)
        ),
    }
}
