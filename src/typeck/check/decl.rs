//! Multi-pass declaration resolution. Lives as a child submodule of
//! `check` so it can read/write `Checker`'s private fields directly
//! without leaking visibility to siblings (`ty.rs`, `error.rs`).
//!
//! Three phases (see spec/08_ADT.md "Phase ordering and module layout"):
//!
//!   0    — Alloc `AdtDef` stubs (`partial: true`) keyed by `AdtId`,
//!           1:1 with HIR's `HAdtId`. Pre-intern `TyKind::Adt(aid)` so
//!           subsequent passes can reference Adts by identity even
//!           before their fields are resolved. This is the "partial
//!           construction" trick — graph nodes exist before edges.
//!
//!   0.5  — Walk each `HirAdt`, resolve every field's `HirTy → TyId`,
//!           backfill `AdtDef.variants`, flip `partial: false`.
//!           Recursive (`struct A { x: A }`) and mutual (`A → B`,
//!           `B → A`) references resolve cleanly because every AdtId
//!           is known after phase 0.
//!
//!   1    — Resolve fn signatures from source annotations. ADT names
//!           in fn types resolve to the pre-interned `Adt(aid)` from
//!           phase 0; primitive names resolve via `TyArena::from_prim_name`.

use index_vec::IndexVec;

use crate::hir::HAdtId;

use super::super::ty::{AdtDef, AdtId, FieldDef, FnSig, TyKind, VariantDef};
use super::Checker;

pub(super) fn resolve_decls(cx: &mut Checker<'_>) {
    alloc_partial_adts(cx);
    resolve_adt_fields(cx);
    resolve_fn_sigs(cx);
}

/// Phase 0 — push an `AdtDef` stub per HIR adt and pre-intern its
/// `TyKind::Adt(aid)`. The `IndexVec` order is HAdtId-aligned so
/// `AdtId::from_raw(haid.raw())` is the lookup throughout.
/// FIXME: do not assume they are equal!
fn alloc_partial_adts(cx: &mut Checker<'_>) {
    for hir_adt in cx.hir.adts.iter() {
        let aid = cx.adts.push(AdtDef {
            name: hir_adt.name.clone(),
            kind: hir_adt.kind,
            variants: IndexVec::new(),
            partial: true,
        });
        // Pre-intern the identity so resolve_named_ty in phase 0.5 / 1 hits.
        let _ = cx.tys.intern(TyKind::Adt(aid));
    }
}

/// Phase 0.5 — backfill each AdtDef's variants/fields with resolved
/// TyIds. ADT-typed fields hit the pre-interned identity from phase 0;
/// primitive-typed fields resolve through `TyArena::from_prim_name`;
/// unknown type names emit `UnknownType` and resolve to `tys.error`.
fn resolve_adt_fields(cx: &mut Checker<'_>) {
    for raw in 0..cx.adts.len() {
        let aid = AdtId::from_raw(raw as u32);
        let haid = HAdtId::from_raw(raw as u32);
        // Sanity: AdtId/HAdtId numbering is 1:1 by phase 0 construction.
        debug_assert_eq!(cx.adts[aid].name, cx.hir.adts[haid].name);

        let mut variants: IndexVec<_, VariantDef> = IndexVec::new();

        for variant in cx.hir.adts[haid].variants.iter() {
            let variant_name = variant.name.clone();

            let mut fields: IndexVec<_, FieldDef> = IndexVec::new();
            for field in variant.fields.iter() {
                // Clone the field's HirTy so we don't keep a borrow on
                // cx.hir while we call resolve_named_ty (which takes
                // &mut cx.tys / &mut cx.errors).
                let ty = Checker::resolve_named_ty(&mut cx.tys, &mut cx.errors, &field.ty);
                fields.push(FieldDef {
                    name: field.name.clone(),
                    ty,
                    span: field.span.clone(),
                });
            }
            variants.push(VariantDef {
                name: variant_name,
                fields,
            });
        }

        cx.adts[aid].variants = variants;
        cx.adts[aid].partial = false;
    }
}

/// Phase 1 — resolve fn signatures. Bodies aren't touched; that's phase 2.
fn resolve_fn_sigs(cx: &mut Checker<'_>) {
    for (fid, hir_fn) in cx.hir.fns.iter_enumerated() {
        let mut params = Vec::with_capacity(hir_fn.params.len());
        for &lid in &hir_fn.params {
            let local = &cx.hir.locals[lid];
            let ty = Checker::resolve_annotation(
                &mut cx.tys,
                &mut cx.errors,
                local.ty.as_ref(),
                &local.span,
            );
            cx.local_tys[lid] = ty;
            params.push(ty);
        }
        let ret = match &hir_fn.ret_ty {
            Some(t) => Checker::resolve_named_ty(&mut cx.tys, &mut cx.errors, t),
            None => cx.tys.unit, // Rust-style: implicit unit
        };
        cx.fn_sigs[fid] = FnSig {
            params,
            ret,
            partial: false,
        };
    }
}
