//! Layout helpers — `size_of` and `align_of` queries on concrete `TyId`s.
//!
//! Both helpers return `Option<u64>`; `None` means the type's layout is
//! unknown (unsized array, unresolved infer var, generic `Param`, fn type,
//! poison). Callers must pass concrete (post-substitution) types if they
//! want a `Some` answer for parametric inputs.
//!
//! The ADT walk runs the C layout algorithm (per spec/17_LAYOUT.md):
//! place each field at the next offset rounded up to the field's
//! alignment, take the struct's alignment as the max field alignment,
//! pad the total to the struct's alignment. Generic ADTs substitute
//! their field types through `(generic_params, args)` via
//! `TyArena::substitute_ty` — the same pattern `codegen/ty.rs:lower_adt_type`
//! uses for LLVM struct lowering. Both helpers therefore take
//! `&mut TypeckResults` so the substitution can intern intermediate
//! `TyId`s.
//!
//! Target assumption (v0): natural-alignment, 64-bit pointers. Both
//! `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu` produce
//! identical layouts for our v0 type set, so no `target_triple`
//! parameter is threaded through.

use crate::hir::VariantIdx;
use crate::typeck::{AdtId, PrimTy, TyId, TyKind, TypeckResults, subst_from};

/// Size in bytes for a concrete type. `None` for types whose layout is
/// not statically determined: unsized arrays, generic `Param`, infer
/// vars, fn types, poison.
pub fn size_of(typeck: &mut TypeckResults, t: TyId) -> Option<u64> {
    match typeck.tys.kind(t).clone() {
        TyKind::Prim(p) => Some(prim_size(p)),
        // Pointers are target-pointer-width = 8 on every supported v0 target.
        TyKind::Ptr(_, _) => Some(8),
        TyKind::Unit | TyKind::Never => Some(0),
        TyKind::Array(elem, Some(n)) => {
            let elem_size = size_of(typeck, elem)?;
            elem_size.checked_mul(n)
        }
        TyKind::Adt(aid, args) => adt_size_align(typeck, aid, &args).map(|(s, _)| s),
        TyKind::Array(_, None)
        | TyKind::Param(_)
        | TyKind::Infer(_)
        | TyKind::Fn(..)
        | TyKind::Error => None,
    }
}

/// Alignment in bytes for a concrete type. Same `None` discipline as
/// `size_of`.
pub fn align_of(typeck: &mut TypeckResults, t: TyId) -> Option<u64> {
    match typeck.tys.kind(t).clone() {
        TyKind::Prim(p) => Some(prim_align(p)),
        TyKind::Ptr(_, _) => Some(8),
        // Per spec: empty struct / unit has align 1 (aggregate convention).
        // Never has no value form; we still pin it to align 1 for shape
        // uniformity with Unit.
        TyKind::Unit | TyKind::Never => Some(1),
        TyKind::Array(elem, Some(_)) => align_of(typeck, elem),
        TyKind::Adt(aid, args) => adt_size_align(typeck, aid, &args).map(|(_, a)| a),
        TyKind::Array(_, None)
        | TyKind::Param(_)
        | TyKind::Infer(_)
        | TyKind::Fn(..)
        | TyKind::Error => None,
    }
}

/// Compute `(size, align)` for one ADT instantiation. Single field walk
/// produces both numbers — callers that want only one of them shouldn't
/// pay for the other, but the v0 caller mix calls both back-to-back so
/// fusing is the simpler shape.
fn adt_size_align(
    typeck: &mut TypeckResults,
    aid: AdtId,
    args: &[TyId],
) -> Option<(u64, u64)> {
    // Snapshot decl info under a tight `&typeck.adts` borrow; release
    // before recursing through the field types so the inner
    // `substitute_ty` / `size_of` / `align_of` calls can take `&mut typeck`.
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

    let mut offset: u64 = 0;
    let mut struct_align: u64 = 1;
    for f_ty in field_decl_tys {
        let concrete = typeck.tys.substitute_ty(f_ty, &subst);
        let f_size = size_of(typeck, concrete)?;
        let f_align = align_of(typeck, concrete)?;
        offset = round_up(offset, f_align);
        offset = offset.checked_add(f_size)?;
        struct_align = struct_align.max(f_align);
    }
    let total = round_up(offset, struct_align);
    Some((total, struct_align))
}

fn prim_size(p: PrimTy) -> u64 {
    match p {
        PrimTy::I8 | PrimTy::U8 | PrimTy::Bool => 1,
        PrimTy::I16 | PrimTy::U16 => 2,
        PrimTy::I32 | PrimTy::U32 => 4,
        PrimTy::I64 | PrimTy::U64 | PrimTy::Usize | PrimTy::Isize => 8,
    }
}

/// Natural alignment — every primitive is aligned to its size.
fn prim_align(p: PrimTy) -> u64 {
    prim_size(p)
}

fn round_up(x: u64, align: u64) -> u64 {
    debug_assert!(
        align > 0 && align.is_power_of_two(),
        "round_up: align must be a positive power of two, got {align}"
    );
    (x + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use index_vec::IndexVec;

    use super::*;
    use crate::hir::AdtKind;
    use crate::parser::ast::Mutability;
    use crate::reporter::Span;
    use crate::typeck::{
        AdtDef, FieldDef, ParamId, TyArena, TypeckResults, VariantDef,
    };

    fn empty_typeck() -> TypeckResults {
        TypeckResults {
            tys: TyArena::new(),
            adts: IndexVec::new(),
            fn_sigs: IndexVec::new(),
            local_tys: IndexVec::new(),
            expr_tys: IndexVec::new(),
            const_tys: IndexVec::new(),
            call_type_args: std::collections::HashMap::new(),
        }
    }

    /// Push an ADT with the given fields (in order) and zero generic
    /// params. Returns its `AdtId`.
    fn push_adt(typeck: &mut TypeckResults, name: &str, fields: &[(&str, TyId)]) -> AdtId {
        let variant = VariantDef {
            name: None,
            fields: fields
                .iter()
                .map(|(n, ty)| FieldDef {
                    name: (*n).to_string(),
                    ty: *ty,
                    span: Span::default(),
                })
                .collect(),
        };
        let mut variants = IndexVec::new();
        variants.push(variant);
        typeck.adts.push(AdtDef {
            name: name.to_string(),
            kind: AdtKind::Struct,
            generic_params: vec![],
            variants,
            partial: false,
        })
    }

    /// Push a generic ADT — fields use `Param(ParamId(idx))` to refer to
    /// the i-th type parameter.
    fn push_generic_adt(
        typeck: &mut TypeckResults,
        name: &str,
        n_params: u32,
        fields: &[(&str, TyId)],
    ) -> AdtId {
        let generic_params: Vec<ParamId> =
            (0..n_params).map(ParamId::from_raw).collect();
        let variant = VariantDef {
            name: None,
            fields: fields
                .iter()
                .map(|(n, ty)| FieldDef {
                    name: (*n).to_string(),
                    ty: *ty,
                    span: Span::default(),
                })
                .collect(),
        };
        let mut variants = IndexVec::new();
        variants.push(variant);
        typeck.adts.push(AdtDef {
            name: name.to_string(),
            kind: AdtKind::Struct,
            generic_params,
            variants,
            partial: false,
        })
    }

    #[test]
    fn primitives_have_natural_size_and_align() {
        let mut t = empty_typeck();
        let bool_ty = t.tys.bool;
        let i8 = t.tys.i8;
        let u8 = t.tys.u8;
        let i16 = t.tys.i16;
        let u16 = t.tys.u16;
        let i32 = t.tys.i32;
        let u32 = t.tys.u32;
        let i64 = t.tys.i64;
        let u64 = t.tys.u64;
        let usize_ty = t.tys.usize;
        let isize_ty = t.tys.isize;

        assert_eq!(size_of(&mut t, bool_ty), Some(1));
        assert_eq!(align_of(&mut t, bool_ty), Some(1));
        assert_eq!(size_of(&mut t, i8), Some(1));
        assert_eq!(size_of(&mut t, u8), Some(1));
        assert_eq!(size_of(&mut t, i16), Some(2));
        assert_eq!(size_of(&mut t, u16), Some(2));
        assert_eq!(align_of(&mut t, u16), Some(2));
        assert_eq!(size_of(&mut t, i32), Some(4));
        assert_eq!(size_of(&mut t, u32), Some(4));
        assert_eq!(align_of(&mut t, i32), Some(4));
        assert_eq!(size_of(&mut t, i64), Some(8));
        assert_eq!(size_of(&mut t, u64), Some(8));
        assert_eq!(size_of(&mut t, usize_ty), Some(8));
        assert_eq!(size_of(&mut t, isize_ty), Some(8));
        assert_eq!(align_of(&mut t, i64), Some(8));
    }

    #[test]
    fn pointers_are_eight_bytes_regardless_of_pointee() {
        let mut t = empty_typeck();
        let i8 = t.tys.i8;
        let i64 = t.tys.i64;
        let ptr_i8 = t.tys.intern(TyKind::Ptr(i8, Mutability::Const));
        let ptr_i64 = t.tys.intern(TyKind::Ptr(i64, Mutability::Mut));
        assert_eq!(size_of(&mut t, ptr_i8), Some(8));
        assert_eq!(align_of(&mut t, ptr_i8), Some(8));
        assert_eq!(size_of(&mut t, ptr_i64), Some(8));
        assert_eq!(align_of(&mut t, ptr_i64), Some(8));
    }

    #[test]
    fn unit_and_never_are_zero_sized() {
        let mut t = empty_typeck();
        let unit = t.tys.unit;
        let never = t.tys.never;
        assert_eq!(size_of(&mut t, unit), Some(0));
        assert_eq!(align_of(&mut t, unit), Some(1));
        assert_eq!(size_of(&mut t, never), Some(0));
        assert_eq!(align_of(&mut t, never), Some(1));
    }

    #[test]
    fn unknown_layouts_return_none() {
        let mut t = empty_typeck();
        let i32 = t.tys.i32;
        let unsized_array = t.tys.intern(TyKind::Array(i32, None));
        assert_eq!(size_of(&mut t, unsized_array), None);
        assert_eq!(align_of(&mut t, unsized_array), None);

        let param = t.tys.intern(TyKind::Param(ParamId::from_raw(0)));
        assert_eq!(size_of(&mut t, param), None);
        assert_eq!(align_of(&mut t, param), None);

        let error = t.tys.error;
        assert_eq!(size_of(&mut t, error), None);

        let fn_ty = t.tys.intern(TyKind::Fn(vec![i32], i32, false));
        assert_eq!(size_of(&mut t, fn_ty), None);
    }

    #[test]
    fn array_size_is_elem_size_times_count() {
        let mut t = empty_typeck();
        let i32 = t.tys.i32;
        let u8 = t.tys.u8;
        let arr_3_i32 = t.tys.intern(TyKind::Array(i32, Some(3)));
        assert_eq!(size_of(&mut t, arr_3_i32), Some(12));
        assert_eq!(align_of(&mut t, arr_3_i32), Some(4));

        let arr_8_u8 = t.tys.intern(TyKind::Array(u8, Some(8)));
        assert_eq!(size_of(&mut t, arr_8_u8), Some(8));
        assert_eq!(align_of(&mut t, arr_8_u8), Some(1));
    }

    #[test]
    fn spec_worked_example_struct_u8_u32_u8_is_12_bytes() {
        // struct { a: u8, b: u32, c: u8 } per spec §size_of and align_of.
        let mut t = empty_typeck();
        let u8 = t.tys.u8;
        let u32 = t.tys.u32;
        let aid = push_adt(&mut t, "S", &[("a", u8), ("b", u32), ("c", u8)]);
        let s_ty = t.tys.intern(TyKind::Adt(aid, vec![]));
        assert_eq!(size_of(&mut t, s_ty), Some(12));
        assert_eq!(align_of(&mut t, s_ty), Some(4));
    }

    #[test]
    fn empty_struct_is_zero_size_align_one() {
        let mut t = empty_typeck();
        let aid = push_adt(&mut t, "Empty", &[]);
        let empty_ty = t.tys.intern(TyKind::Adt(aid, vec![]));
        assert_eq!(size_of(&mut t, empty_ty), Some(0));
        assert_eq!(align_of(&mut t, empty_ty), Some(1));
    }

    #[test]
    fn generic_adt_layouts_substitute_through_args() {
        // struct Pair<A, B> { a: A, b: B }
        let mut t = empty_typeck();
        let p0 = t.tys.intern(TyKind::Param(ParamId::from_raw(0)));
        let p1 = t.tys.intern(TyKind::Param(ParamId::from_raw(1)));
        let aid = push_generic_adt(&mut t, "Pair", 2, &[("a", p0), ("b", p1)]);

        let i32 = t.tys.i32;
        let u8 = t.tys.u8;
        let u64 = t.tys.u64;

        // Pair<i32, u8>: a at 0 (size 4), b at 4 (size 1) → 5 → padded
        // to align 4 → 8 bytes, align 4.
        let pair_i32_u8 = t.tys.intern(TyKind::Adt(aid, vec![i32, u8]));
        assert_eq!(size_of(&mut t, pair_i32_u8), Some(8));
        assert_eq!(align_of(&mut t, pair_i32_u8), Some(4));

        // Pair<u8, u64>: a at 0 (size 1), padded to 8 → b at 8 (size 8)
        // → 16 bytes, align 8.
        let pair_u8_u64 = t.tys.intern(TyKind::Adt(aid, vec![u8, u64]));
        assert_eq!(size_of(&mut t, pair_u8_u64), Some(16));
        assert_eq!(align_of(&mut t, pair_u8_u64), Some(8));
    }

    #[test]
    fn recursive_via_pointer_is_sixteen_bytes_via_ptr_termination() {
        // struct Node { next: *mut Node, val: i32 } — `*mut Node` is 8 bytes.
        let mut t = empty_typeck();
        // We need a Node TyId before defining its fields. Trick: use a
        // forward reference — push the AdtDef with a placeholder, then
        // patch the variants.
        let aid = t.adts.push(AdtDef {
            name: "Node".to_string(),
            kind: AdtKind::Struct,
            generic_params: vec![],
            variants: IndexVec::new(),
            partial: false,
        });
        let node_ty = t.tys.intern(TyKind::Adt(aid, vec![]));
        let next_ty = t.tys.intern(TyKind::Ptr(node_ty, Mutability::Mut));
        let i32 = t.tys.i32;
        let mut variants = IndexVec::new();
        let mut fields: IndexVec<crate::hir::FieldIdx, FieldDef> = IndexVec::new();
        fields.push(FieldDef {
            name: "next".to_string(),
            ty: next_ty,
            span: Span::default(),
        });
        fields.push(FieldDef {
            name: "val".to_string(),
            ty: i32,
            span: Span::default(),
        });
        variants.push(VariantDef { name: None, fields });
        t.adts[aid].variants = variants;

        // Layout: next at 0 (8 bytes), val at 8 (4 bytes) → 12, padded
        // to align 8 → 16 bytes, align 8.
        assert_eq!(size_of(&mut t, node_ty), Some(16));
        assert_eq!(align_of(&mut t, node_ty), Some(8));
    }

    #[test]
    fn nested_adt_in_adt_composes() {
        // struct Inner { x: i32, y: i32 }; struct Outer { i: Inner, b: u8 }
        let mut t = empty_typeck();
        let i32 = t.tys.i32;
        let u8 = t.tys.u8;
        let inner = push_adt(&mut t, "Inner", &[("x", i32), ("y", i32)]);
        let inner_ty = t.tys.intern(TyKind::Adt(inner, vec![]));
        let outer = push_adt(&mut t, "Outer", &[("i", inner_ty), ("b", u8)]);
        let outer_ty = t.tys.intern(TyKind::Adt(outer, vec![]));

        // Inner: 8 bytes, align 4. Outer: i at 0 (8), b at 8 (1) → 9
        // → padded to align 4 → 12 bytes, align 4.
        assert_eq!(size_of(&mut t, inner_ty), Some(8));
        assert_eq!(align_of(&mut t, inner_ty), Some(4));
        assert_eq!(size_of(&mut t, outer_ty), Some(12));
        assert_eq!(align_of(&mut t, outer_ty), Some(4));
    }

    #[test]
    fn round_up_helper_is_correct() {
        assert_eq!(round_up(0, 4), 0);
        assert_eq!(round_up(1, 4), 4);
        assert_eq!(round_up(3, 4), 4);
        assert_eq!(round_up(4, 4), 4);
        assert_eq!(round_up(5, 4), 8);
        assert_eq!(round_up(7, 8), 8);
        assert_eq!(round_up(0, 1), 0);
        assert_eq!(round_up(13, 1), 13);
    }
}
