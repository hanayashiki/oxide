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
use crate::reporter::Span;

use super::super::error::{ParamOrReturn, SizedPos, TypeError};
use super::super::ty::{AdtDef, AdtId, FieldDef, FnSig, TyId, TyKind, VariantDef};
use super::Checker;
use super::obligation::Obligation;

pub(super) fn resolve_decls(cx: &mut Checker<'_>) {
    alloc_partial_adts(cx);
    resolve_adt_fields(cx);
    check_recursive_adts(cx);
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
        // Pre-intern the identity so resolve_ty in phase 0.5 / 1 hits.
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
                // cx.hir while we call resolve_ty (which takes
                // &mut cx.tys / &mut cx.errors).
                let ty = Checker::resolve_ty(&mut cx.tys, &mut cx.errors, &field.ty);
                // Field types are concrete; push the Sized obligation
                // with the resolved TyId. Discharged at `finish()`. See
                // spec/09_ARRAY.md "E0261".
                cx.decl_obligations.push(Obligation::Sized {
                    ty,
                    pos: SizedPos::Field,
                    span: field.ty.span.clone(),
                });
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

/// Phase 0.6 — reject ADTs whose field-type graph contains a cycle
/// without a `Ptr` indirection layer. `Ptr(_, _)` lowers to opaque
/// `ptr` and breaks the cycle (the pointee isn't entered
/// structurally); `Array(T, Some(_))` propagates through `T`; every
/// other `TyKind` is a leaf for this purpose.
///
/// Tri-color DFS over the ADT graph: white = unvisited,
/// gray = currently on the DFS stack, black = fully explored. A gray
/// child is a back-edge — the cycle closes there. Emit one
/// `RecursiveAdt` per back-edge with the offending field's span. See
/// spec/08_ADT.md "Recursive type rejection" and
/// spec/BACKLOG/B013_RECURSIVE_ADT_ACCEPTED.md.
fn check_recursive_adts(cx: &mut Checker<'_>) {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    let mut color: IndexVec<AdtId, Color> =
        IndexVec::from_vec(vec![Color::White; cx.adts.len()]);

    for raw in 0..cx.adts.len() {
        let aid = AdtId::from_raw(raw as u32);
        if color[aid] == Color::White {
            visit(cx, aid, &mut color);
        }
    }

    fn visit(cx: &mut Checker<'_>, a: AdtId, color: &mut IndexVec<AdtId, Color>) {
        color[a] = Color::Gray;
        // Snapshot the outgoing edges before recursing — the call to
        // `visit(cx, ...)` needs `&mut cx`, which conflicts with a
        // live `&cx.adts` borrow held by an iterator.
        let edges = collect_adt_edges(cx, a);
        for (child, field_span) in edges {
            match color[child] {
                Color::Gray => {
                    let name = cx.adts[a].name.clone();
                    cx.errors.push(TypeError::RecursiveAdt {
                        adt: name,
                        span: field_span,
                    });
                }
                Color::White => visit(cx, child, color),
                Color::Black => {}
            }
        }
        color[a] = Color::Black;
    }

    fn collect_adt_edges(cx: &Checker<'_>, a: AdtId) -> Vec<(AdtId, Span)> {
        let mut out = Vec::new();
        for variant in &cx.adts[a].variants {
            for field in &variant.fields {
                walk_ty(cx, field.ty, &field.span, &mut out);
            }
        }
        out
    }

    fn walk_ty(cx: &Checker<'_>, ty: TyId, span: &Span, out: &mut Vec<(AdtId, Span)>) {
        match cx.tys.kind(ty) {
            TyKind::Adt(child) => out.push((*child, span.clone())),
            TyKind::Array(elem, Some(_)) => walk_ty(cx, *elem, span, out),
            // Ptr breaks the cycle; Prim / Unit / Never / Fn / Infer /
            // Error don't contribute edges. Unsized arrays
            // (Array(_, None)) at field position are rejected earlier
            // by the Sized obligation (E0269), but treating them as
            // non-edges here is harmless.
            _ => {}
        }
    }
}

/// Phase 1 — resolve fn signatures. Bodies aren't touched; that's phase 2.
fn resolve_fn_sigs(cx: &mut Checker<'_>) {
    for (fid, hir_fn) in cx.hir.fns.iter_enumerated() {
        let is_extern = hir_fn.is_extern;
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
            // Sized check at param position. Always enqueue: a missing
            // annotation resolves to `tys.error`, and discharge is a
            // no-op on `Error`. Diagnostic span is the whole local
            // declaration. See spec/09_ARRAY.md.
            cx.decl_obligations.push(Obligation::Sized {
                ty,
                pos: SizedPos::Param,
                span: local.span.clone(),
            });
            // E0264: sized array by value at `extern "C"` boundary.
            // Unsized arrays are caught by the Sized obligation above
            // (E0269), so this only fires for `Array(_, Some(_))`.
            // See spec/09_ARRAY.md "ABI: array-by-value across extern C".
            if is_extern && matches!(cx.tys.kind(ty), TyKind::Array(_, Some(_))) {
                cx.errors.push(TypeError::ArrayByValueAtExternC {
                    which: ParamOrReturn::Param,
                    ty,
                    span: local.span.clone(),
                });
            }
        }
        let ret = match &hir_fn.ret_ty {
            Some(t) => {
                let span = t.span.clone();
                let ty = Checker::resolve_ty(&mut cx.tys, &mut cx.errors, t);
                cx.decl_obligations.push(Obligation::Sized {
                    ty,
                    pos: SizedPos::Return,
                    span: span.clone(),
                });
                if is_extern && matches!(cx.tys.kind(ty), TyKind::Array(_, Some(_))) {
                    cx.errors.push(TypeError::ArrayByValueAtExternC {
                        which: ParamOrReturn::Return,
                        ty,
                        span,
                    });
                }
                ty
            }
            // Rust-style implicit unit return — Unit is sized; no
            // obligation needed.
            None => cx.tys.unit,
        };
        cx.fn_sigs[fid] = FnSig {
            params,
            ret,
            partial: false,
            c_variadic: hir_fn.is_variadic,
        };
    }
}
