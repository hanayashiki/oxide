//! Expression dispatch (`emit_expr`) plus the literal-shaped emitters
//! (int / bool / char / null / string / const) and the small `emit_call`
//! coordinator that delegates to `lower/call.rs`.

use inkwell::module::Linkage;
use inkwell::values::{PointerValue, UnnamedAddress};

use crate::codegen::ty::lower_prim;
use crate::hir::{ConstId, HExprId, HirConstValue, HirExprKind};
use crate::typeck::TyKind;

use super::{Codegen, FnCodegenContext, Operand, call};

impl<'a, 'ctx> Codegen<'a, 'ctx> {
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
    pub(super) fn emit_expr(
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
                        let llty = self.lower_ty(ty);
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
                let typeck_args_opt: Option<Vec<crate::typeck::TyId>> =
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

        let ptr: PointerValue<'ctx> = global.as_pointer_value();
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

    fn emit_int_lit(
        &mut self,
        fx: &FnCodegenContext<'ctx>,
        eid: HExprId,
        n: u64,
    ) -> Operand<'ctx> {
        let ty = self.ty_of(fx, eid);
        match self.typeck_results.tys().kind(ty) {
            TyKind::Prim(p) => Operand::Value(lower_prim(self.ctx, *p).const_int(n, false).into()),
            other => panic!("int lit had non-prim type {:?}", other),
        }
    }
}

