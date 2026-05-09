//! Place-form lowering: `lvalue` (assignment-target / `&place` ptrs),
//! `emit_field` (rvalue field access), `emit_struct_lit`, plus the
//! pointer-peeling helpers (`peel_ptrs`, `peel_ptrs_ty`) and small GEP
//! utilities (`field_index`, `field_gep`).

use inkwell::values::{BasicValue, PointerValue};

use crate::hir::{FieldIdx, HExprId, HirExprKind, VariantIdx};
use crate::parser::ast::UnOp;
use crate::typeck::{AdtId, TyId, TyKind, subst_from};

use super::{Codegen, FnCodegenContext, Operand};

impl<'a, 'ctx> Codegen<'a, 'ctx> {
    pub(super) fn lvalue(
        &mut self,
        fx: &mut FnCodegenContext<'ctx>,
        eid: HExprId,
    ) -> PointerValue<'ctx> {
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
            } => {
                let op = self
                    .emit_deref_ptr(fx, expr)
                    .expect("lvalue-position Deref operand cannot diverge");
                match op {
                    Operand::Place(p) => p,
                    _ => panic!("emit_deref_ptr must return Operand::Place"),
                }
            }
            other => panic!("v0 codegen: non-lvalue assignment target {:?}", other),
        }
    }

    /// Position of `name` in `adts[aid]`'s sole variant. Typeck has
    /// already validated the field exists; a miss here is an ICE.
    pub(super) fn field_index(&mut self, aid: AdtId, name: &str) -> u32 {
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
    pub(super) fn peel_ptrs(
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
    pub(super) fn peel_ptrs_ty(&mut self, mut cur_ty: TyId) -> TyId {
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
    pub(super) fn field_gep(
        &mut self,
        base_ptr: PointerValue<'ctx>,
        base_ty: TyId,
        field_idx: u32,
    ) -> PointerValue<'ctx> {
        let base_ll = self.lower_ty(base_ty);
        self.builder
            .build_struct_gep(base_ll, base_ptr, field_idx, "fld.gep")
            .unwrap()
    }

    pub(super) fn emit_field(
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
        let field_ll = self.lower_ty(field_ty);

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
    pub(super) fn emit_struct_lit(
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
        let llty = self.lower_adt_type(aid, &args);
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
}
